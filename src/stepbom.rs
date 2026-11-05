//! Product structure of STEP assemblies (ISO 10303-21 files of AP203, AP214
//! and AP242) as a BOM table for the import (see
//! [`crate::services::imports`]).
//!
//! The reader looks at the product structure only, never at the geometry:
//!
//! * `PRODUCT('id', 'name', 'description', …)` – a part or an assembly; the
//!   id is its part number,
//! * `PRODUCT_DEFINITION_FORMATION(_WITH_SPECIFIED_SOURCE)` and
//!   `PRODUCT_DEFINITION` – the version and the definition the structure
//!   links,
//! * `NEXT_ASSEMBLY_USAGE_OCCURRENCE(…, #parent, #child, …)` – one placed
//!   occurrence of a component; the quantity is the number of occurrences
//!   below the same parent.
//!
//! The assembly is the product that no other product uses; with several of
//! them, each becomes a component of the new assembly. A sub-assembly lists
//! its components at its first occurrence only (the import reads the BOM of a
//! component once). Complex instances (`#1 = (A() B());`) and the quantities
//! of `QUANTIFIED_ASSEMBLY_COMPONENT_USAGE` are not read.
//!
//! An own minimal Part 21 reader: `ruststep` was rejected as a direct
//! dependency (see `docs/OPEN_WORK.md`), and only five entity types are needed.

use std::collections::{HashMap, HashSet};

use crate::bomtable::{field_label, BomRow, BomTable, Column, MAX_ROWS};

/// Deeper structures are cut (protects the recursion).
const MAX_DEPTH: usize = 64;

/// Deeper nested parameter lists are skipped without recursion, so a hostile
/// file cannot overflow the stack (real files nest a few levels at most).
const MAX_NESTING: usize = 32;

/// Whether the file name is a STEP file (`.step`, `.stp`, `.p21`).
pub fn is_step(filename: &str) -> bool {
    filename
        .rsplit_once('.')
        .is_some_and(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "step" | "stp" | "p21"))
}

/// A parameter of an entity instance.
#[derive(Debug, PartialEq)]
enum Param {
    Str(String),
    Ref(u64),
    List(Vec<Param>),
    /// `$`, `*`, numbers, enumerations and typed values – not needed here.
    Other,
}

struct Product {
    id: String,
    name: String,
    description: String,
}

impl Product {
    fn shown(&self) -> &str {
        if self.id.trim().is_empty() {
            self.name.trim()
        } else {
            self.id.trim()
        }
    }
}

/// Reads the product structure of a STEP file.
pub fn read(bytes: &[u8]) -> Result<BomTable, String> {
    let text = String::from_utf8_lossy(bytes);
    if !text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("ISO-10303-21")
    {
        return Err("Not a STEP file: it does not start with ISO-10303-21.".into());
    }
    let mut products: HashMap<u64, Product> = HashMap::new();
    // formation → product, definition → formation
    let mut formations: HashMap<u64, u64> = HashMap::new();
    let mut definitions: HashMap<u64, u64> = HashMap::new();
    // (parent definition, child definition) in file order
    let mut usages: Vec<(u64, u64)> = Vec::new();
    for_each_instance(&text, |id, keyword, body| {
        let is = |name: &str| keyword.eq_ignore_ascii_case(name);
        if is("PRODUCT") {
            let p = params(body);
            products.insert(
                id,
                Product {
                    id: string(&p, 0),
                    name: string(&p, 1),
                    description: string(&p, 2),
                },
            );
        } else if is("PRODUCT_DEFINITION_FORMATION")
            || is("PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE")
        {
            if let Some(product) = reference(&params(body), 2) {
                formations.insert(id, product);
            }
        } else if is("PRODUCT_DEFINITION") || is("PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS") {
            if let Some(formation) = reference(&params(body), 2) {
                definitions.insert(id, formation);
            }
        } else if is("NEXT_ASSEMBLY_USAGE_OCCURRENCE") {
            let p = params(body);
            if let (Some(parent), Some(child)) = (reference(&p, 3), reference(&p, 4)) {
                usages.push((parent, child));
            }
        }
    });

    let product_of = |definition: u64| {
        definitions
            .get(&definition)
            .and_then(|f| formations.get(f))
            .filter(|p| products.contains_key(p))
            .copied()
    };
    // parent product → (child product, occurrences), both in file order
    let mut children: HashMap<u64, Vec<(u64, usize)>> = HashMap::new();
    let mut parents: Vec<u64> = Vec::new();
    let mut used = HashSet::new();
    let mut unresolved = 0;
    for (parent, child) in usages {
        let (Some(p), Some(c)) = (product_of(parent), product_of(child)) else {
            unresolved += 1;
            continue;
        };
        let list = children.entry(p).or_insert_with(|| {
            parents.push(p);
            Vec::new()
        });
        match list.iter_mut().find(|(x, _)| *x == c) {
            Some(entry) => entry.1 += 1,
            None => list.push((c, 1)),
        }
        used.insert(c);
    }
    if children.is_empty() {
        return Err(
            "No assembly structure found: the STEP file has no NEXT_ASSEMBLY_USAGE_OCCURRENCE – \
             it holds a single part or was exported without its product structure."
                .into(),
        );
    }
    let roots: Vec<u64> = parents
        .iter()
        .filter(|p| !used.contains(*p))
        .copied()
        .collect();
    if roots.is_empty() {
        return Err("The product structure of the STEP file is circular.".into());
    }

    let mut walk = Walk {
        products: &products,
        children: &children,
        expanded: HashSet::new(),
        path: Vec::new(),
        rows: Vec::new(),
        warnings: Vec::new(),
    };
    if unresolved > 0 {
        walk.warnings.push(format!(
            "{unresolved} assembly occurrences point to unknown product definitions and are \
             ignored."
        ));
    }
    let root = if let [root] = roots[..] {
        let mut row = walk.row(root, 0, 1.0);
        row.line = 1;
        walk.expand(root, 0)?;
        Some(row)
    } else {
        walk.warnings.push(format!(
            "The STEP file holds {} top-level assemblies: each becomes a component of the new \
             assembly.",
            roots.len()
        ));
        for &root in &roots {
            let row = walk.row(root, 1, 1.0);
            walk.rows.push(row);
            walk.expand(root, 1)?;
        }
        None
    };
    let columns = [
        ("PRODUCT id", "number"),
        ("PRODUCT name", "name"),
        ("PRODUCT description", "description"),
        ("NEXT_ASSEMBLY_USAGE_OCCURRENCE", "quantity"),
    ]
    .into_iter()
    .map(|(header, field)| Column {
        header: header.into(),
        field,
        label: field_label(field),
    })
    .collect();
    Ok(BomTable {
        root,
        rows: walk.rows,
        columns,
        warnings: walk.warnings,
    })
}

/// Depth-first walk through the structure, producing the rows in BOM order.
struct Walk<'a> {
    products: &'a HashMap<u64, Product>,
    children: &'a HashMap<u64, Vec<(u64, usize)>>,
    expanded: HashSet<u64>,
    path: Vec<u64>,
    rows: Vec<BomRow>,
    warnings: Vec<String>,
}

impl Walk<'_> {
    /// Line 1 is the assembly, the components follow in BOM order.
    fn row(&self, product: u64, level: usize, quantity: f64) -> BomRow {
        let p = &self.products[&product];
        let number = p.id.trim().to_string();
        let name = match p.name.trim() {
            "" => number.clone(),
            name => name.to_string(),
        };
        let description = match p.description.trim() {
            d if d == name || d == number => String::new(),
            d => d.to_string(),
        };
        BomRow {
            line: self.rows.len() + 2,
            level,
            find_number: None,
            number,
            name,
            description,
            quantity,
            unit: String::new(),
            material: String::new(),
            mass_kg: None,
        }
    }

    /// Adds the components of `product` (on `level` + 1) – at its first
    /// occurrence only.
    fn expand(&mut self, product: u64, level: usize) -> Result<(), String> {
        if !self.expanded.insert(product) {
            return Ok(());
        }
        let Some(children) = self.children.get(&product) else {
            return Ok(());
        };
        if level >= MAX_DEPTH {
            self.warnings.push(format!(
                "The structure is deeper than {MAX_DEPTH} levels – the components of {} are \
                 skipped.",
                self.products[&product].shown()
            ));
            return Ok(());
        }
        self.path.push(product);
        for &(child, count) in children {
            if self.path.contains(&child) {
                self.warnings.push(format!(
                    "{} would contain itself – the occurrence is skipped.",
                    self.products[&child].shown()
                ));
                continue;
            }
            if self.rows.len() >= MAX_ROWS {
                return Err(format!("The assembly has more than {MAX_ROWS} BOM rows."));
            }
            let row = self.row(child, level + 1, count as f64);
            self.rows.push(row);
            self.expand(child, level + 1)?;
        }
        self.path.pop();
        Ok(())
    }
}

/// The text parameter at `index`, decoded; empty if there is none.
fn string(params: &[Param], index: usize) -> String {
    match params.get(index) {
        Some(Param::Str(s)) => s.clone(),
        _ => String::new(),
    }
}

fn reference(params: &[Param], index: usize) -> Option<u64> {
    match params.get(index) {
        Some(Param::Ref(r)) => Some(*r),
        _ => None,
    }
}

/// Calls `f(id, keyword, parameters)` for every simple entity instance
/// `#id = KEYWORD(parameters);` of the file (header entries have no `#id`).
fn for_each_instance(text: &str, mut f: impl FnMut(u64, &str, &str)) {
    let b = text.as_bytes();
    let (mut start, mut i) = (0, 0);
    let mut in_string = false;
    while i < b.len() {
        let c = b[i];
        if in_string {
            if c == b'\'' {
                if b.get(i + 1) == Some(&b'\'') {
                    i += 1;
                } else {
                    in_string = false;
                }
            }
        } else if c == b'\'' {
            in_string = true;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            continue;
        } else if c == b';' {
            if let Some((id, keyword, body)) = instance(&text[start..i]) {
                f(id, keyword, body);
            }
            start = i + 1;
        }
        i += 1;
    }
}

/// Splits `#12 = KEYWORD ( … )` into id, keyword and the text between the
/// outer parentheses.
fn instance(mut record: &str) -> Option<(u64, &str, &str)> {
    // Comments before the instance.
    while let Some(comment) = record.trim_start().strip_prefix("/*") {
        record = comment.split_once("*/")?.1;
    }
    let rest = record.trim_start().strip_prefix('#')?;
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    let id = rest[..digits].parse().ok()?;
    let rest = rest[digits..].trim_start().strip_prefix('=')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let (keyword, rest) = rest.split_at(end);
    if keyword.is_empty() {
        return None; // complex instance
    }
    let body = rest.trim().strip_prefix('(')?.strip_suffix(')')?;
    Some((id, keyword, body))
}

/// Parses a parameter list (without its outer parentheses).
fn params(body: &str) -> Vec<Param> {
    let mut parser = Parser {
        text: body,
        bytes: body.as_bytes(),
        i: 0,
        depth: 0,
    };
    parser.list()
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    i: usize,
    /// Nesting of the list being read.
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.i).copied()
    }

    /// Skips white space and comments.
    fn space(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_ascii_whitespace() => self.i += 1,
                Some(b'/') if self.bytes.get(self.i + 1) == Some(&b'*') => {
                    self.i = self.text[self.i + 2..]
                        .find("*/")
                        .map_or(self.bytes.len(), |e| self.i + 2 + e + 2);
                }
                _ => return,
            }
        }
    }

    /// Parameters up to the closing parenthesis (or the end).
    fn list(&mut self) -> Vec<Param> {
        let mut out = Vec::new();
        loop {
            self.space();
            match self.peek() {
                None => return out,
                Some(b')') => {
                    self.i += 1;
                    return out;
                }
                Some(b',') => self.i += 1,
                Some(b'\'') => out.push(Param::Str(self.string())),
                Some(b'#') => {
                    self.i += 1;
                    let start = self.i;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.i += 1;
                    }
                    out.push(match self.text[start..self.i].parse() {
                        Ok(r) => Param::Ref(r),
                        Err(_) => Param::Other,
                    });
                }
                Some(b'(') if self.depth >= MAX_NESTING => {
                    // `other` skips the balanced list iteratively.
                    self.other();
                    out.push(Param::Other);
                }
                Some(b'(') => {
                    self.i += 1;
                    self.depth += 1;
                    let list = self.list();
                    self.depth -= 1;
                    out.push(Param::List(list));
                }
                Some(_) => {
                    self.other();
                    out.push(Param::Other);
                }
            }
        }
    }

    /// Skips one other parameter, e.g. `$`, `.T.`, `2.5` or `LENGTH_MEASURE(2.5)`.
    fn other(&mut self) {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                b',' | b')' if depth == 0 => return,
                b'(' => depth += 1,
                b')' => depth -= 1,
                b'\'' => {
                    self.string();
                    continue;
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    /// A quoted string starting at the current quote, decoded.
    fn string(&mut self) -> String {
        self.i += 1;
        let mut raw = String::new();
        let mut from = self.i;
        while let Some(c) = self.peek() {
            if c == b'\'' {
                raw.push_str(&self.text[from..self.i]);
                if self.bytes.get(self.i + 1) == Some(&b'\'') {
                    raw.push('\'');
                    self.i += 2;
                    from = self.i;
                    continue;
                }
                self.i += 1;
                return decode(&raw);
            }
            self.i += 1;
        }
        raw.push_str(&self.text[from..]);
        decode(&raw)
    }
}

/// Decodes the escapes of Part 21 strings: `\\`, `\X2\…\X0\` (UTF-16),
/// `\X4\…\X0\` (UTF-32), `\X\hh` and `\S\c` (ISO 8859-1), `\N\`, `\T\`;
/// code page switches (`\PA\`) are dropped.
fn decode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(p) = rest.find('\\') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        if let Some(r) = rest.strip_prefix("\\\\") {
            out.push('\\');
            rest = r;
        } else if let Some((width, r)) = rest
            .strip_prefix("\\X2\\")
            .map(|r| (4, r))
            .or_else(|| rest.strip_prefix("\\X4\\").map(|r| (8, r)))
        {
            let (hex, after) = r.split_once("\\X0\\").unwrap_or((r, ""));
            let units = hex.as_bytes().chunks(width).filter_map(|chunk| {
                std::str::from_utf8(chunk)
                    .ok()
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
            });
            if width == 4 {
                let units: Vec<u16> = units.map(|u| u as u16).collect();
                out.extend(char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')));
            } else {
                out.extend(units.map(|u| char::from_u32(u).unwrap_or('\u{fffd}')));
            }
            rest = after;
        } else if let Some(v) = rest
            .strip_prefix("\\X\\")
            .and_then(|r| r.get(..2))
            .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(char::from(v));
            rest = &rest[5..];
        } else if let Some(c) = rest
            .strip_prefix("\\S\\")
            .and_then(|r| r.bytes().next())
            .filter(u8::is_ascii)
        {
            out.push(char::from(c + 128));
            rest = &rest[4..];
        } else if let Some(r) = rest.strip_prefix("\\N\\") {
            out.push('\n');
            rest = r;
        } else if let Some(r) = rest.strip_prefix("\\T\\") {
            out.push('\t');
            rest = r;
        } else if rest.len() >= 4 && rest.as_bytes()[1] == b'P' && rest.as_bytes()[3] == b'\\' {
            rest = &rest[4..];
        } else {
            out.push('\\');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A STEP file with the given entity instances in its DATA section.
    fn step(data: &str) -> Vec<u8> {
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
             FILE_NAME('pump.step','2026-11-04T12:00:00',(''),(''),'','','');\n\
             FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n{data}\nENDSEC;\n\
             END-ISO-10303-21;\n"
        )
        .into_bytes()
    }

    /// PRODUCT, formation and definition of one product: definition `#<n>3`.
    fn product(n: u32, id: &str, name: &str) -> String {
        format!(
            "#{n}1=PRODUCT('{id}','{name}','',(#9));\n\
             #{n}2=PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE('','',#{n}1,.NOT_KNOWN.);\n\
             #{n}3=PRODUCT_DEFINITION('design','',#{n}2,#8);\n"
        )
    }

    fn usage(id: u32, parent: u32, child: u32) -> String {
        format!("#{id}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('{id}','','',#{parent}3,#{child}3,$);\n")
    }

    fn flat(t: &BomTable) -> Vec<(usize, &str, f64)> {
        t.rows
            .iter()
            .map(|r| (r.level, r.number.as_str(), r.quantity))
            .collect()
    }

    #[test]
    fn assembly_with_sub_assembly_and_repeated_parts() {
        let data = [
            product(1, "PUMP-100", "Pump"),
            product(2, "HSG-1", "Housing"),
            product(3, "ROT-1", "Rotor assembly"),
            product(4, "M6x20", "Screw"),
            product(5, "SHAFT-1", "Shaft"),
            // the first occurrence of the rotor comes before the screws
            usage(101, 1, 2),
            usage(102, 1, 3),
            usage(103, 1, 4),
            usage(104, 1, 4),
            usage(105, 3, 5),
            usage(106, 3, 4),
            usage(107, 1, 4),
        ]
        .concat();
        let t = read(&step(&data)).unwrap();
        let root = t.root.as_ref().unwrap();
        assert_eq!((root.level, root.number.as_str()), (0, "PUMP-100"));
        assert_eq!(root.name, "Pump");
        assert_eq!(
            flat(&t),
            vec![
                (1, "HSG-1", 1.0),
                (1, "ROT-1", 1.0),
                (2, "SHAFT-1", 1.0),
                (2, "M6x20", 1.0),
                (1, "M6x20", 3.0),
            ]
        );
        assert_eq!(t.rows[1].name, "Rotor assembly");
        assert_eq!(
            t.rows.iter().map(|r| r.line).collect::<Vec<_>>(),
            vec![2, 3, 4, 5, 6]
        );
        assert!(t.warnings.is_empty());
        assert_eq!(t.columns.len(), 4);
    }

    #[test]
    fn shared_sub_assembly_is_listed_once() {
        let data = [
            product(1, "TOP", "Top"),
            product(2, "LEFT", "Left"),
            product(3, "RIGHT", "Right"),
            product(4, "WHEEL", "Wheel"),
            product(5, "HUB", "Hub"),
            usage(101, 1, 2),
            usage(102, 1, 3),
            usage(103, 2, 4),
            usage(104, 3, 4),
            usage(105, 4, 5),
        ]
        .concat();
        let t = read(&step(&data)).unwrap();
        assert_eq!(
            flat(&t),
            vec![
                (1, "LEFT", 1.0),
                (2, "WHEEL", 1.0),
                (3, "HUB", 1.0),
                (1, "RIGHT", 1.0),
                (2, "WHEEL", 1.0),
            ]
        );
    }

    #[test]
    fn several_top_level_assemblies() {
        let data = [
            product(1, "A", "A"),
            product(2, "B", "B"),
            product(3, "P", "P"),
            usage(101, 1, 3),
            usage(102, 2, 3),
        ]
        .concat();
        let t = read(&step(&data)).unwrap();
        assert!(t.root.is_none());
        assert_eq!(
            flat(&t),
            vec![(1, "A", 1.0), (2, "P", 1.0), (1, "B", 1.0), (2, "P", 1.0)]
        );
        assert!(t.warnings[0].contains("2 top-level assemblies"));
    }

    #[test]
    fn single_parts_and_other_files_are_refused() {
        let part = read(&step(&product(1, "BRACKET", "Bracket"))).unwrap_err();
        assert!(part.contains("No assembly structure"), "{part}");
        let fixture = std::fs::read("tests/fixtures/cube.step").unwrap();
        assert!(read(&fixture).is_err());
        assert!(read(b"solid cube\nendsolid\n")
            .unwrap_err()
            .contains("Not a STEP"));
        let circular = [
            product(1, "A", "A"),
            product(2, "B", "B"),
            usage(101, 1, 2),
            usage(102, 2, 1),
        ]
        .concat();
        assert!(read(&step(&circular)).unwrap_err().contains("circular"));
    }

    #[test]
    fn strings_comments_and_escapes() {
        let data = "/* exported; with comments */\n\
            #11=PRODUCT('M\\X2\\00DC\\X0\\LLER-1','Gr\\S\\|n''s part; \\X\\E4','',(#9));\n\
            #12=PRODUCT_DEFINITION_FORMATION('','',#11);\n\
            #13=PRODUCT_DEFINITION('design','' /* note */,#12,#8);\n\
            #21=PRODUCT('X','Box','A box',(#9));\n\
            #22=PRODUCT_DEFINITION_FORMATION('','',#21);\n\
            #23=PRODUCT_DEFINITION('design','',#22,#8);\n\
            #30=(GEOMETRIC_REPRESENTATION_CONTEXT(3)REPRESENTATION_CONTEXT('',''));\n\
            #31=NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','','',#23,#13,$);\n";
        let t = read(&step(data)).unwrap();
        let root = t.root.unwrap();
        assert_eq!(
            (root.number.as_str(), root.description.as_str()),
            ("X", "A box")
        );
        assert_eq!(t.rows[0].number, "MÜLLER-1");
        assert_eq!(t.rows[0].name, "Grün's part; ä");
    }

    #[test]
    fn decoding() {
        assert_eq!(decode("a\\\\b"), "a\\b");
        assert_eq!(decode("\\X2\\03B103B2\\X0\\!"), "αβ!");
        assert_eq!(decode("\\X4\\0001F600\\X0\\"), "😀");
        assert_eq!(decode("\\PA\\x\\N\\y"), "x\ny");
        assert_eq!(decode("50\\%"), "50\\%");
        assert!(is_step("Pump.STEP") && is_step("a.stp") && !is_step("a.stl"));
        assert_eq!(
            params("'a',#5,(#6,#7),.T.,LENGTH_MEASURE(2.5),$,'x(y'"),
            vec![
                Param::Str("a".into()),
                Param::Ref(5),
                Param::List(vec![Param::Ref(6), Param::Ref(7)]),
                Param::Other,
                Param::Other,
                Param::Other,
                Param::Str("x(y".into()),
            ]
        );
    }

    #[test]
    fn deeply_nested_parameters_do_not_overflow_the_stack() {
        let n = 200_000;
        let body = format!("'a',{}{},#7", "(".repeat(n), ")".repeat(n));
        let p = params(&body);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], Param::Str("a".into()));
        assert_eq!(p[2], Param::Ref(7));
        // The outer levels are kept, the rest is skipped as one parameter.
        let mut level = &p[1];
        let mut depth = 0;
        while let Param::List(inner) = level {
            depth += 1;
            level = &inner[0];
        }
        assert_eq!((depth, level), (MAX_NESTING, &Param::Other));

        let file = format!(
            "{}#1=PRODUCT({}{});\n",
            product(1, "A", "A"),
            "(".repeat(n),
            ")".repeat(n)
        );
        assert!(read(&step(&file)).is_err());
    }
}
