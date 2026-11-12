//! The tagged binary format of Creo sections (`docs/src/research/creo-brep-preview.md`).
//!
//! A section is a stream of values whose schema is written on first use:
//! `e0 TT name 00` defines field number N (in order of appearance) with type
//! TT, and the value follows. The writer remembers, for every field, which
//! transition came after it last time – how many struct levels close and
//! which field comes next – and only writes the tokens that differ
//! (`f1 f2 f3`/`f4 N` close levels, `f7 NN` names the next field, `f0` is
//! "zero closes, explicitly"). Structs begin with `e2`/`e3`, `e1` is null,
//! `f8 N`/`f9 R C`/`fa K d…` start arrays (dimensions are remembered per
//! field; `fc R` reuses the column count), `fb` precedes the elements of a
//! struct array, `e4`–`e7 N` repeat the previous array element.
//!
//! Integers: `00`–`7f`, `80`–`bf` + 1 byte, `c0`–`df` + 2 bytes, `e8`–`eb`
//! + 1–4 bytes, `f5 <int>` negated, `f6` = −1. Doubles: see [`double`].

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

/// Field types that are read as integers.
/// Largest number of nodes built from one section (memory guard).
const MAX_NODES: usize = 20_000_000;
/// Largest array announced by one header.
const MAX_ARRAY: usize = 50_000_000;

/// A parsed section.
#[derive(Debug, Default)]
pub struct Tree {
    pub fields: Vec<Field>,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub ty: u8,
    pub name: String,
    dims: Option<Vec<usize>>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Null,
    Int(i64),
    Dbl(f64),
    Str(String),
    /// Leaf array (nested lists for 2-D arrays).
    List(Vec<usize>),
    /// Struct: (field, child node) in file order; a field may repeat.
    Struct(Vec<(u32, usize)>),
}

impl Tree {
    pub fn root(&self) -> usize {
        0
    }

    pub fn node(&self, i: usize) -> &Node {
        self.nodes.get(i).unwrap_or(&Node::Null)
    }

    /// The first child `name` of struct `i`.
    pub fn child(&self, i: usize, name: &str) -> Option<usize> {
        match self.node(i) {
            Node::Struct(kids) => kids
                .iter()
                .find(|(f, _)| {
                    self.fields
                        .get(*f as usize)
                        .is_some_and(|fd| fd.name == name)
                })
                .map(|(_, c)| *c),
            _ => None,
        }
    }

    /// All children of struct `i` whose field name starts with `prefix`,
    /// with their full field names.
    pub fn children_with_prefix(&self, i: usize, prefix: &str) -> Vec<(&str, usize)> {
        match self.node(i) {
            Node::Struct(kids) => kids
                .iter()
                .filter_map(|(f, c)| {
                    let name = self.fields.get(*f as usize)?.name.as_str();
                    name.starts_with(prefix).then_some((name, *c))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// All children named `name` (repeated fields, flattened lists).
    pub fn children(&self, i: usize, name: &str) -> Vec<usize> {
        match self.node(i) {
            Node::Struct(kids) => kids
                .iter()
                .filter(|(f, _)| {
                    self.fields
                        .get(*f as usize)
                        .is_some_and(|fd| fd.name == name)
                })
                .map(|(_, c)| *c)
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn int(&self, i: usize) -> Option<i64> {
        match self.node(i) {
            Node::Int(v) => Some(*v),
            Node::Dbl(v) if v.fract() == 0.0 => Some(*v as i64),
            _ => None,
        }
    }

    pub fn int_at(&self, i: usize, name: &str) -> Option<i64> {
        self.child(i, name).and_then(|c| self.int(c))
    }

    pub fn dbl(&self, i: usize) -> Option<f64> {
        match self.node(i) {
            Node::Dbl(v) => Some(*v),
            Node::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn dbl_at(&self, i: usize, name: &str) -> Option<f64> {
        self.child(i, name).and_then(|c| self.dbl(c))
    }

    pub fn list(&self, i: usize) -> &[usize] {
        match self.node(i) {
            Node::List(v) => v,
            _ => &[],
        }
    }

    /// A flat list of doubles (NaN for anything that is not a number).
    pub fn dbls(&self, i: usize) -> Vec<f64> {
        self.list(i)
            .iter()
            .map(|&c| self.dbl(c).unwrap_or(f64::NAN))
            .collect()
    }

    /// A list of rows of doubles.
    pub fn rows(&self, i: usize) -> Vec<Vec<f64>> {
        self.list(i).iter().map(|&r| self.dbls(r)).collect()
    }

    pub fn str_at(&self, i: usize, name: &str) -> Option<&str> {
        match self.child(i, name).map(|c| self.node(c)) {
            Some(Node::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }
}

struct Frame {
    field: u32,
    node: usize,
    is_array: bool,
    /// Elements still expected (struct arrays).
    left: usize,
    /// R9: node holding foreign fields written at array level.
    extra: Option<usize>,
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
    tree: Tree,
    /// Remembered transition per field: (closes, next field).
    trans: HashMap<u32, (usize, u32)>,
    stack: Vec<Frame>,
    explicit: bool,
}

/// Parses a tagged section.
pub fn parse(data: &[u8]) -> Result<Tree> {
    let mut r = Reader {
        d: data,
        p: 0,
        tree: Tree::default(),
        trans: HashMap::new(),
        stack: Vec::new(),
        explicit: false,
    };
    r.run()?;
    Ok(r.tree)
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .d
            .get(self.p)
            .ok_or_else(|| anyhow!("section ends inside a value"))?;
        self.p += 1;
        Ok(b)
    }

    fn peek(&self) -> Option<u8> {
        self.d.get(self.p).copied()
    }

    /// The bytes not read yet.
    fn rest(&self) -> &'a [u8] {
        self.d.get(self.p..).unwrap_or_default()
    }

    fn field(&self, f: u32) -> Result<&Field> {
        self.tree
            .fields
            .get(f as usize)
            .ok_or_else(|| anyhow!("unknown field {f}"))
    }

    fn set_dims(&mut self, f: u32, dims: Vec<usize>) {
        if let Some(fld) = self.tree.fields.get_mut(f as usize) {
            fld.dims = Some(dims);
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .p
            .checked_add(n)
            .filter(|&e| e <= self.d.len())
            .ok_or_else(|| anyhow!("section ends inside a value"))?;
        let s = self.d.get(self.p..end).unwrap_or_default();
        self.p = end;
        Ok(s)
    }

    fn read_int(&mut self) -> Result<i64> {
        // `f5` negates the integer that follows; counted in a loop, so a run
        // of them in a damaged file cannot exhaust the stack
        let mut negate = false;
        let mut b = self.byte()?;
        while b == 0xf5 {
            negate = !negate;
            b = self.byte()?;
        }
        let v = self.read_int_after(b)?;
        Ok(if negate {
            v.checked_neg()
                .ok_or_else(|| anyhow!("integer out of range"))?
        } else {
            v
        })
    }

    fn read_int_after(&mut self, b: u8) -> Result<i64> {
        Ok(match b {
            0x00..=0x7f => i64::from(b),
            0x80..=0xbf => (i64::from(b & 0x3f) << 8) | i64::from(self.byte()?),
            0xc0..=0xdf => {
                let hi = i64::from(b & 0x1f) << 16;
                let m = i64::from(self.byte()?) << 8;
                hi | m | i64::from(self.byte()?)
            }
            0xe8..=0xeb => {
                let n = usize::from(b - 0xe7);
                let s = self.take(n)?;
                let mut v: i64 = 0;
                for &x in s {
                    v = (v << 8) | i64::from(x);
                }
                if n == 4 {
                    v = i64::from(v as u32 as i32);
                }
                v
            }
            0xf6 => -1,
            other => bail!("integer token {other:02x} at {}", self.p - 1),
        })
    }

    fn read_string(&mut self) -> Result<Node> {
        if self.peek() == Some(0xe1) {
            self.p += 1;
            return Ok(Node::Null);
        }
        let rest = self.rest();
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow!("unterminated string"))?;
        let s = String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).into_owned();
        self.p += end + 1;
        Ok(Node::Str(s))
    }

    fn read_scalar(&mut self, ty: u8) -> Result<Node> {
        Ok(match ty {
            0x02 => {
                let (v, n) = double::decode(self.rest())?;
                self.p += n;
                Node::Dbl(v)
            }
            0x03 | 0x0a => self.read_string()?,
            0x06 => {
                let (v, n) = double::decode_float(self.rest())?;
                self.p += n;
                Node::Dbl(v)
            }
            _ => Node::Int(self.read_int()?),
        })
    }

    fn read_values(&mut self, ty: u8, count: usize) -> Result<Vec<usize>> {
        if count > MAX_ARRAY {
            bail!("array of {count} elements");
        }
        let mut out: Vec<usize> = Vec::new();
        while out.len() < count {
            if let (Some(b @ 0xe4..=0xe7), Some(&last)) = (self.peek(), out.last()) {
                self.p += 1;
                let n = if b == 0xe7 {
                    usize::try_from(self.read_int()?).unwrap_or(usize::MAX)
                } else {
                    usize::from(b - 0xe3)
                };
                if out.len().saturating_add(n) > count {
                    bail!("array repeat beyond the array");
                }
                out.extend(std::iter::repeat_n(last, n));
                continue;
            }
            let v = self.read_scalar(ty)?;
            out.push(self.push(v)?);
        }
        Ok(out)
    }

    fn push(&mut self, n: Node) -> Result<usize> {
        if self.tree.nodes.len() >= MAX_NODES {
            bail!("section with more than {MAX_NODES} values");
        }
        self.tree.nodes.push(n);
        Ok(self.tree.nodes.len() - 1)
    }

    fn dim(&mut self) -> Result<usize> {
        let v = self.read_int()?;
        usize::try_from(v)
            .ok()
            .filter(|&n| n <= MAX_ARRAY)
            .ok_or_else(|| anyhow!("array dimension {v}"))
    }

    fn shaped(&mut self, ty: u8, dims: &[usize]) -> Result<Node> {
        let total = dims
            .iter()
            .try_fold(1usize, |a, &d| a.checked_mul(d))
            .filter(|&n| n <= MAX_ARRAY)
            .ok_or_else(|| anyhow!("array too large"))?;
        let flat = self.read_values(ty, total)?;
        if let [r, c] = *dims {
            if c == 0 {
                return Ok(Node::List(flat));
            }
            let mut rows = Vec::with_capacity(r);
            for chunk in flat.chunks(c) {
                rows.push(self.push(Node::List(chunk.to_vec()))?);
            }
            return Ok(Node::List(rows));
        }
        Ok(Node::List(flat))
    }

    fn read_leaf(&mut self, field: u32) -> Result<Node> {
        let ty = self.field(field)?.ty;
        let dims = self.field(field)?.dims.clone();
        match self.peek() {
            Some(0xf8) => {
                self.p += 1;
                let n = self.dim()?;
                self.set_dims(field, vec![n]);
                self.shaped(ty, &[n])
            }
            Some(0xfc) if dims.as_ref().is_some_and(|d| d.len() == 2) => {
                self.p += 1;
                let r = self.dim()?;
                let c = dims.as_ref().and_then(|d| d.get(1)).copied().unwrap_or(0);
                self.set_dims(field, vec![r, c]);
                self.shaped(ty, &[r, c])
            }
            Some(0xf9) => {
                self.p += 1;
                let r = self.dim()?;
                let c = self.dim()?;
                self.set_dims(field, vec![r, c]);
                self.shaped(ty, &[r, c])
            }
            Some(0xfa) => {
                self.p += 1;
                let k = self.dim()?;
                if k > 8 {
                    bail!("{k}-dimensional array");
                }
                let mut ds = Vec::with_capacity(k);
                for _ in 0..k {
                    ds.push(self.dim()?);
                }
                self.set_dims(field, ds.clone());
                // flat, like the prototype
                let total = ds
                    .iter()
                    .try_fold(1usize, |a, &d| a.checked_mul(d))
                    .filter(|&n| n <= MAX_ARRAY)
                    .ok_or_else(|| anyhow!("array too large"))?;
                Ok(Node::List(self.read_values(ty, total)?))
            }
            _ => match dims {
                Some(ds) => self.shaped(ty, &ds),
                None => self.read_scalar(ty),
            },
        }
    }

    fn read_def(&mut self) -> Result<u32> {
        if self.byte()? != 0xe0 {
            bail!("field definition expected");
        }
        let ty = self.byte()?;
        let Node::Str(name) = self.read_string()? else {
            bail!("field without name");
        };
        if self.tree.fields.len() >= u32::MAX as usize / 2 {
            bail!("too many fields");
        }
        self.tree.fields.push(Field {
            ty,
            name,
            dims: None,
        });
        Ok(self.tree.fields.len() as u32 - 1)
    }

    fn read_closes(&mut self) -> Result<usize> {
        let mut n = 0usize;
        self.explicit = false;
        loop {
            match self.peek() {
                Some(b @ 0xf1..=0xf3) => {
                    self.explicit = true;
                    self.p += 1;
                    n += usize::from(b - 0xf0);
                }
                Some(0xf4) => {
                    self.explicit = true;
                    self.p += 1;
                    n = n.saturating_add(usize::try_from(self.read_int()?).unwrap_or(usize::MAX));
                }
                Some(0xf0) => {
                    self.explicit = true;
                    self.p += 1;
                }
                _ => return Ok(n),
            }
        }
    }

    fn close(&mut self, n: usize) -> Result<()> {
        for _ in 0..n {
            if self.stack.len() <= 1 {
                bail!("more levels closed than open at {}", self.p);
            }
            self.stack.pop();
        }
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        let root_field = self.read_def()?;
        let tok = self.byte()?;
        if self.field(root_field)?.ty != 0 || !(tok == 0xe2 || tok == 0xe3) {
            bail!("section does not start with a struct");
        }
        let root = self.push(Node::Struct(Vec::new()))?;
        self.stack.push(Frame {
            field: root_field,
            node: root,
            is_array: false,
            left: 0,
            extra: None,
        });
        let mut key = root_field;
        let mut last_p = usize::MAX;
        let mut stall = 0;
        while self.p < self.d.len() {
            if self.p == last_p {
                stall += 1;
                if stall > 50 {
                    bail!("parser stalled at {}", self.p);
                }
            } else {
                stall = 0;
            }
            last_p = self.p;
            let mut closes = self.read_closes()?;
            let Some(b) = self.peek() else { break };
            let mut explicit = self.explicit;
            let next: u32;
            if b == 0xf7 || b == 0xe0 {
                if b == 0xf7 {
                    self.p += 1;
                    let id = self.read_int()?;
                    // field numbers in the file count from 1
                    next = u32::try_from(id)
                        .ok()
                        .and_then(|i| i.checked_sub(1))
                        .filter(|&i| (i as usize) < self.tree.fields.len())
                        .ok_or_else(|| anyhow!("unknown field {id}"))?;
                } else {
                    next = self.read_def()?;
                }
                closes += self.read_closes()?;
                explicit |= self.explicit;
                if !explicit {
                    closes = self.trans.get(&key).map_or(0, |m| m.0);
                }
                self.trans.insert(key, (closes, next));
            } else {
                let m = self.trans.get(&key).copied();
                match (explicit, m) {
                    (true, Some((_, n))) => {
                        next = n;
                        self.trans.insert(key, (closes, next));
                    }
                    (false, Some((c, n))) => {
                        closes = c;
                        next = n;
                    }
                    (_, None) => bail!("no transition remembered for field {key} at {}", self.p),
                }
            }
            self.close(closes)?;
            key = self.read_value(next)?;
        }
        Ok(())
    }

    fn store(&mut self, field: u32, child: usize) {
        let Some(top) = self.stack.last() else { return };
        let node = top.node;
        if let Some(Node::Struct(kids)) = self.tree.nodes.get_mut(node) {
            kids.push((field, child));
        } else if let Some(Node::List(items)) = self.tree.nodes.get_mut(node) {
            items.push(child);
        }
    }

    fn read_value(&mut self, field: u32) -> Result<u32> {
        let top_is_array = self.stack.last().is_some_and(|f| f.is_array);
        let top_field = self.stack.last().map_or(u32::MAX, |f| f.field);
        if top_is_array && field != top_field {
            // R9: a foreign field at array level lives in a side struct
            let extra = match self.stack.last().and_then(|f| f.extra) {
                Some(e) => e,
                None => {
                    let e = self.push(Node::Struct(Vec::new()))?;
                    if let Some(f) = self.stack.last_mut() {
                        f.extra = Some(e);
                    }
                    e
                }
            };
            let depth = self.stack.len();
            self.stack.push(Frame {
                field: top_field,
                node: extra,
                is_array: false,
                left: 0,
                extra: None,
            });
            let r = self.read_value(field);
            // the pseudo frame is removed again unless a struct was opened under it
            if self.stack.len() == depth + 1 {
                self.stack.pop();
            } else if self.stack.len() > depth {
                self.stack.remove(depth);
            }
            return r;
        }
        let ty = self.field(field)?.ty;
        if ty == 0 {
            let b = self.byte()?;
            match b {
                0xe1 => {
                    let n = self.push(Node::Null)?;
                    if let Some(top) = self.stack.last_mut() {
                        if top.is_array {
                            top.left = top.left.saturating_sub(1);
                        }
                    }
                    self.store(field, n);
                    return Ok(field);
                }
                0xf8 | 0xf9 if !top_is_array => {
                    let mut n = self.dim()?;
                    if b == 0xf9 {
                        n = n
                            .checked_mul(self.dim()?)
                            .filter(|&x| x <= MAX_ARRAY)
                            .ok_or_else(|| anyhow!("array too large"))?;
                    }
                    let list = self.push(Node::List(Vec::new()))?;
                    self.store(field, list);
                    if n == 0 {
                        return Ok(field);
                    }
                    self.stack.push(Frame {
                        field,
                        node: list,
                        is_array: true,
                        left: n,
                        extra: None,
                    });
                    return Ok(field);
                }
                0xfb if top_is_array => {
                    let b2 = self.byte()?;
                    if b2 == 0xe1 {
                        let n = self.push(Node::Null)?;
                        if let Some(top) = self.stack.last_mut() {
                            top.left = top.left.saturating_sub(1);
                        }
                        self.store(field, n);
                        return Ok(field);
                    }
                    if !(b2 == 0xe2 || b2 == 0xe3) {
                        bail!("struct token {b2:02x} at {}", self.p - 1);
                    }
                }
                0xe2 | 0xe3 => {}
                other => bail!("struct token {other:02x} at {}", self.p - 1),
            }
            let node = self.push(Node::Struct(Vec::new()))?;
            if let Some(top) = self.stack.last_mut() {
                if top.is_array {
                    top.left = top.left.saturating_sub(1);
                }
            }
            self.store(field, node);
            self.stack.push(Frame {
                field,
                node,
                is_array: false,
                left: 0,
                extra: None,
            });
            return Ok(field);
        }
        let leaf = self.read_leaf(field)?;
        let n = self.push(leaf)?;
        self.store(field, n);
        Ok(field)
    }
}

/// Compact doubles of the tagged format.
pub mod double {
    use anyhow::{anyhow, bail, Result};

    use std::f64::consts::PI;

    /// Top byte of the ten groups of tokens `19`–`4a`.
    const GROUP_TOP: [u8; 10] = [0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0xbc, 0xbd, 0xbe, 0xbf, 0xc0];

    /// One-byte constants.
    fn constant(t: u8) -> Option<f64> {
        Some(match t {
            0x03 => 0.125,
            0x04 => f64::from_bits(0x3FEF_FFFF_FFFF_FFFF),
            0x0b => 0.06,
            0x0c => 0.1,
            0x0d => 0.25,
            0x0e => 0.5,
            0x0f => 1.0,
            0x10 => -1.0,
            0x11 => PI,
            0x12 => 0.5 * PI,
            0x13 => 2.0 * PI,
            0x14 => 1.5 * PI,
            0x15 => -PI,
            0x16 => -0.5 * PI,
            0x17 => -2.0 * PI,
            0x18 => 0.0,
            0x05..=0x0a => f64::NAN, // unknown constants
            _ => return None,
        })
    }

    /// Top 16 bits (sign, exponent, four mantissa bits) of the class tokens
    /// `4b`–`df`.
    fn class_top(t: u8) -> Option<u16> {
        Some(match t {
            0x4b => 0x3fb0,
            0x4c => 0x3fb1,
            0x4d => 0x3fb5,
            0x4e => 0x3fc0,
            0x4f..=0x53 => 0x3fc1 + u16::from(t - 0x4f),
            0x54..=0x57 => 0x3fc7 + u16::from(t - 0x54),
            0x58 | 0x59 => 0x3fcc + u16::from(t - 0x58),
            0x5a..=0xa3 => 0x3fcf + u16::from(t - 0x5a),
            0xa4 => 0xbfc0,
            0xa5 | 0xa6 => 0xbfd0 + u16::from(t - 0xa5),
            0xa7..=0xb2 => 0xbfd3 + u16::from(t - 0xa7),
            0xb3..=0xcf => 0xbfe0 + u16::from(t - 0xb3),
            0xd0..=0xdc => 0xbffe + u16::from(t - 0xd0),
            0xdd => 0xc00c,
            0xde => 0xc010,
            0xdf => 0xc011,
            _ => return None,
        })
    }

    fn bytes_to_f64(b: [u8; 8]) -> f64 {
        f64::from_be_bytes(b)
    }

    /// Decodes one double at the start of `d`: (value, bytes consumed).
    pub fn decode(d: &[u8]) -> Result<(f64, usize)> {
        let &t = d
            .first()
            .ok_or_else(|| anyhow!("section ends inside a number"))?;
        let need = |n: usize| -> Result<&[u8]> {
            d.get(1..1 + n)
                .ok_or_else(|| anyhow!("section ends inside a number"))
        };
        if let Some(c) = constant(t) {
            return Ok((c, 1));
        }
        match t {
            0x00..=0x02 => {
                // near-constant: constant `c` with its lowest n bytes replaced
                let n = usize::from(t) + 1;
                let s = need(n + 1)?;
                let (&c, low_bytes) = s
                    .split_first()
                    .ok_or_else(|| anyhow!("section ends inside a number"))?;
                let base = constant(c).unwrap_or(f64::NAN);
                if base.is_nan() {
                    return Ok((f64::NAN, n + 2));
                }
                let mut bits = base.to_bits();
                let mask = (1u64 << (8 * n)) - 1;
                let mut low = 0u64;
                for &x in low_bytes {
                    low = (low << 8) | u64::from(x);
                }
                bits = (bits & !mask) | low;
                Ok((f64::from_bits(bits), n + 2))
            }
            0x19..=0x4a => {
                let g = usize::from((t - 0x19) / 5);
                let k = (t - 0x19) % 5;
                let top = GROUP_TOP.get(g).copied().unwrap_or(0x3c);
                let mut raw = [0u8; 8];
                raw[0] = top;
                let used = match k {
                    0 => {
                        raw[1..8].copy_from_slice(need(7)?);
                        7
                    }
                    1 => {
                        // two bytes, the last one repeated to the end
                        let &[a, b] = need(2)? else {
                            bail!("section ends inside a number")
                        };
                        raw = [top, a, b, b, b, b, b, b];
                        2
                    }
                    2 => {
                        let &[a, b] = need(2)? else {
                            bail!("section ends inside a number")
                        };
                        raw = [top, a, b, 0, 0, 0, 0, 0];
                        2
                    }
                    3 => {
                        raw[1..4].copy_from_slice(need(3)?);
                        3
                    }
                    _ => {
                        raw[1..7].copy_from_slice(need(6)?);
                        6
                    }
                };
                Ok((bytes_to_f64(raw), 1 + used))
            }
            0x4b..=0xdf => {
                let top = class_top(t).unwrap_or(0x7ff8);
                let s = need(6)?;
                let mut raw = [0u8; 8];
                raw[0..2].copy_from_slice(&top.to_be_bytes());
                raw[2..8].copy_from_slice(s);
                Ok((bytes_to_f64(raw), 7))
            }
            0xe8..=0xef => {
                let n = match t {
                    0xe8 => 1,
                    0xe9 => 2,
                    0xea => 3,
                    0xeb => 4,
                    0xec => 7,
                    0xed => 8,
                    0xee => 3,
                    _ => 2,
                };
                let s = need(n)?;
                let mut raw = [0u8; 8];
                for (r, &x) in raw.iter_mut().zip(s) {
                    *r = x;
                }
                Ok((bytes_to_f64(raw), 1 + n))
            }
            other => bail!("number token {other:02x}"),
        }
    }

    /// Type-06 numbers (float32 precision stored with the same token
    /// families, three payload bytes): (value, bytes consumed).
    pub fn decode_float(d: &[u8]) -> Result<(f64, usize)> {
        let &t = d
            .first()
            .ok_or_else(|| anyhow!("section ends inside a number"))?;
        let need = |n: usize| -> Result<&[u8]> {
            d.get(1..1 + n)
                .ok_or_else(|| anyhow!("section ends inside a number"))
        };
        match t {
            0x00..=0x32 => Ok((
                if t == 0 {
                    0.0
                } else {
                    constant(t).unwrap_or(f64::NAN)
                },
                1,
            )),
            0x50..=0xb7 | 0xbc..=0xdf => {
                need(2)?;
                Ok((f64::NAN, 3))
            }
            0xe8..=0xeb => {
                let n = usize::from(t - 0xe7);
                let s = need(n)?;
                let mut raw = [0u8; 4];
                for (r, &x) in raw.iter_mut().zip(s) {
                    *r = x;
                }
                Ok((f64::from(f32::from_be_bytes(raw)), 1 + n))
            }
            0x33..=0x4f | 0xb8..=0xbb => {
                let s = need(3)?;
                let mut raw = [0u8; 8];
                if (0x33..=0x4a).contains(&t) {
                    raw[0] = GROUP_TOP
                        .get(usize::from((t - 0x19) / 5))
                        .copied()
                        .unwrap_or(0x3c);
                    raw[1..4].copy_from_slice(s);
                } else {
                    let top = class_top(t).unwrap_or(0x7ff8);
                    raw[0..2].copy_from_slice(&top.to_be_bytes());
                    raw[2..5].copy_from_slice(s);
                }
                Ok((bytes_to_f64(raw), 4))
            }
            other => bail!("float token {other:02x}"),
        }
    }
}

/// Writes sections in the tagged format, reduced to what the readers use:
/// every transition is spelled out (`f0`–`f4` closes, then `f7 NN` or a
/// field definition), integers in four bytes, doubles in eight. For tests
/// and synthetic files (`asm::sample`); Creo itself writes far more
/// compactly.
#[doc(hidden)]
pub struct Writer {
    out: Vec<u8>,
    fields: Vec<(u8, String)>,
    /// Levels to close before the next value.
    pending: usize,
}

/// Field types of the writer.
const TY_STRUCT: u8 = 0x00;
const TY_INT: u8 = 0x01;
const TY_DOUBLE: u8 = 0x02;
const TY_STRING: u8 = 0x03;

impl Writer {
    /// A section whose root struct is `root`.
    pub fn new(root: &str) -> Writer {
        let mut w = Writer {
            out: Vec::new(),
            fields: vec![(TY_STRUCT, root.to_string())],
            pending: 0,
        };
        w.out.extend([0xe0, TY_STRUCT]);
        w.out.extend(root.as_bytes());
        w.out.extend([0x00, 0xe2]);
        w
    }

    fn int_bytes(&mut self, v: i64) {
        if v < 0 {
            self.out.push(0xf5);
        }
        let a = v.unsigned_abs();
        if a < 0x80 {
            self.out.push(a as u8);
        } else {
            self.out.push(0xeb);
            self.out
                .extend((a.min(i32::MAX as u64) as u32).to_be_bytes());
        }
    }

    /// Closes, then names the field (defining it on first use).
    fn key(&mut self, name: &str, ty: u8) {
        match self.pending {
            0 => self.out.push(0xf0),
            n @ 1..=3 => self.out.push(0xf0 + n as u8),
            n => {
                self.out.push(0xf4);
                self.int_bytes(n as i64);
            }
        }
        self.pending = 0;
        match self.fields.iter().position(|(t, n)| *t == ty && n == name) {
            Some(i) => {
                self.out.push(0xf7);
                self.int_bytes(i as i64 + 1);
            }
            None => {
                self.fields.push((ty, name.to_string()));
                self.out.extend([0xe0, ty]);
                self.out.extend(name.as_bytes());
                self.out.push(0x00);
            }
        }
    }

    fn dbl_bytes(&mut self, v: f64) {
        self.out.push(0xed);
        self.out.extend(v.to_be_bytes());
    }

    pub fn int(&mut self, name: &str, v: i64) -> &mut Writer {
        self.key(name, TY_INT);
        self.int_bytes(v);
        self
    }

    pub fn dbl(&mut self, name: &str, v: f64) -> &mut Writer {
        self.key(name, TY_DOUBLE);
        self.dbl_bytes(v);
        self
    }

    pub fn string(&mut self, name: &str, s: &str) -> &mut Writer {
        self.key(name, TY_STRING);
        self.out.extend(s.as_bytes());
        self.out.push(0x00);
        self
    }

    pub fn dbls(&mut self, name: &str, v: &[f64]) -> &mut Writer {
        self.key(name, TY_DOUBLE);
        self.out.push(0xf8);
        self.int_bytes(v.len() as i64);
        for &x in v {
            self.dbl_bytes(x);
        }
        self
    }

    pub fn strings(&mut self, name: &str, v: &[&str]) -> &mut Writer {
        self.key(name, TY_STRING);
        self.out.push(0xf8);
        self.int_bytes(v.len() as i64);
        for s in v {
            self.out.extend(s.as_bytes());
            self.out.push(0x00);
        }
        self
    }

    /// Opens a struct; [`Writer::end`] closes it.
    pub fn begin(&mut self, name: &str) -> &mut Writer {
        self.key(name, TY_STRUCT);
        self.out.push(0xe2);
        self
    }

    /// Opens an array of `n` structs; each element is opened with
    /// [`Writer::element`] and closed with [`Writer::end`], the array itself
    /// with a last [`Writer::end`] (none for `n` = 0).
    pub fn begin_array(&mut self, name: &str, n: usize) -> &mut Writer {
        self.key(name, TY_STRUCT);
        self.out.push(0xf8);
        self.int_bytes(n as i64);
        self
    }

    pub fn element(&mut self, array: &str) -> &mut Writer {
        self.key(array, TY_STRUCT);
        self.out.extend([0xfb, 0xe2]);
        self
    }

    pub fn end(&mut self) -> &mut Writer {
        self.pending += 1;
        self
    }

    pub fn finish(&self) -> Vec<u8> {
        self.out.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_writer_round_trips_through_the_reader() {
        let mut w = Writer::new("top");
        w.int("count", 2)
            .int("neg", -300)
            .dbl("pi", std::f64::consts::PI)
            .string("name", "MS_TIP")
            .dbls("axis", &[1.0, -0.5, 2.0])
            .begin_array("items", 2);
        for k in 0..2 {
            w.element("items")
                .int("id", k)
                .dbls("o", &[k as f64; 3])
                .end();
        }
        w.end()
            .begin("tail")
            .strings("words", &["a", "Offset", "in"])
            .end();
        let t = parse(&w.finish()).unwrap();
        let r = t.root();
        assert_eq!(t.int_at(r, "count"), Some(2));
        assert_eq!(t.int_at(r, "neg"), Some(-300));
        assert_eq!(t.dbl_at(r, "pi"), Some(std::f64::consts::PI));
        assert_eq!(t.str_at(r, "name"), Some("MS_TIP"));
        assert_eq!(t.dbls(t.child(r, "axis").unwrap()), vec![1.0, -0.5, 2.0]);
        let items = t.list(t.child(r, "items").unwrap());
        assert_eq!(items.len(), 2);
        assert_eq!(t.int_at(items[1], "id"), Some(1));
        assert_eq!(t.dbls(t.child(items[1], "o").unwrap()), vec![1.0; 3]);
        let tail = t.child(r, "tail").unwrap();
        let words: Vec<_> = t
            .list(t.child(tail, "words").unwrap())
            .iter()
            .map(|&i| match t.node(i) {
                Node::Str(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(words, ["a", "Offset", "in"]);
    }

    fn visib_geom() -> Vec<u8> {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cad/nist/ctc-01/creo-10/nist_ctc_01_asme1_rd_cr10030.prt.5"
        ))
        .unwrap();
        let toc = super::super::container::toc(&data).unwrap();
        super::super::container::section(&data, &toc, "VisibGeom")
            .unwrap()
            .unwrap()
    }

    #[test]
    fn numbers_decode_like_the_prototype() {
        // 3π/2 + 2 ulp as a near-constant
        let (v, n) = double::decode(&[0x00, 0x14, 0xd4]).unwrap();
        assert_eq!(n, 3);
        assert_eq!(v.to_bits(), 0x4012_d97c_7f33_21d4);
        // group token 2e (top 0x40, two bytes, last byte repeated): 5.05
        let (v, n) = double::decode(&[0x2e, 0x14, 0x33]).unwrap();
        assert_eq!(n, 3);
        assert!((v - 5.05).abs() < 1e-12, "{v}");
        // class token d8 with 6 mantissa bytes: -2.8463…
        let (v, n) = double::decode(&[0xd8, 0xc5, 0x37, 0x3d, 0x13, 0x16, 0x18]).unwrap();
        assert_eq!(n, 7);
        assert!((v + 2.8463).abs() < 1e-4, "{v}");
        assert_eq!(double::decode(&[0x0f]).unwrap(), (1.0, 1));
        assert!(double::decode(&[0xf7]).is_err());
        assert!(double::decode(&[0x46, 1]).is_err());
    }

    #[test]
    fn the_visible_geometry_of_ctc_01_parses() {
        let sec = visib_geom();
        let tree = parse(&sec).unwrap();
        let root = tree.root();
        let active = tree.child(root, "active_geom").unwrap();
        let surfaces = tree.child(active, "srf_array").unwrap();
        assert_eq!(tree.list(surfaces).len(), 141);
        let curves = tree.child(active, "crv_array").unwrap();
        assert_eq!(tree.list(curves).len(), 422);
        let first = tree.list(surfaces)[0];
        assert_eq!(tree.int_at(first, "geom_id"), Some(106));
        assert_eq!(tree.int_at(first, "geom_type"), Some(34));
        let prim = tree.child(first, "srf_prim_ptr(plane)").unwrap();
        let ls = tree.rows(tree.child(prim, "local_sys").unwrap());
        assert_eq!(ls.len(), 4);
        assert_eq!(ls[0], [1.0, 0.0, 0.0]);
        let body = tree.child(active, "body_array").unwrap();
        let b = tree.list(body)[0];
        assert_eq!(tree.int_at(b, "body_id"), Some(-5778));
        assert_eq!(
            tree.str_at(b, "mat_name_ptr"),
            Some("PTC_SYSTEM_MTRL_PROPS")
        );
    }

    #[test]
    fn cut_and_damaged_sections_are_errors_not_panics() {
        let sec = visib_geom();
        for cut in (0..sec.len()).step_by(sec.len() / 60) {
            let _ = parse(&sec[..cut]);
        }
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..30 {
            let mut damaged = sec.clone();
            for _ in 0..8 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let at = (state % damaged.len() as u64) as usize;
                damaged[at] = (state >> 32) as u8;
            }
            let _ = parse(&damaged);
        }
    }
}
