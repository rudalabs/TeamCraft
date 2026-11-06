//! A small, bounded reader of ISO 10303-21 exchange files ("STEP physical
//! files") for the 3D preview.
//!
//! It does three things the B-rep reader of the `truck` kernel cannot do
//! with real-world files:
//!
//! * [`scan`] splits the DATA section into entity instances without
//!   building a syntax tree (an index of byte ranges, so even large files
//!   need little memory) and rejects damaged files with a clear message;
//! * [`write_normalized`] re-writes one instance in the compact form the
//!   kernel's parser understands – no whitespace or comments inside
//!   instances (AP242 exporters wrap complex instances over several lines),
//!   no escaped quotes or binary values, canonical numbers, no empty lists;
//! * [`Params`] reads the parameters of one instance as a stream (the
//!   tessellated geometry of AP242, see `tessellated.rs`).
//!
//! The reader works on bytes: text outside strings is ASCII by the
//! standard, strings may hold anything (Latin-1 or UTF-8 names).
//! Nothing here panics or recurses: nesting is limited to [`MAX_DEPTH`].

use anyhow::{anyhow, bail, Result};

/// Deeper parameter nesting is rejected (STEP geometry needs four levels).
pub const MAX_DEPTH: usize = 32;

/// One token of the exchange structure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tok<'a> {
    /// `#123`
    Ref(u64),
    /// `NAME` or `!NAME` (user defined)
    Keyword(&'a [u8]),
    Int(i64),
    Real(f64),
    /// Raw content between the quotes (with `''` escapes).
    Str(&'a [u8]),
    /// `.NAME.` without the dots.
    Enum(&'a [u8]),
    /// Binary values, constants (`#NAME`) and value instances (`@…`) –
    /// never needed for geometry, kept as `$`.
    Other,
    /// `$`
    Null,
    /// `*`
    Omitted,
    Open,
    Close,
    Comma,
    Eq,
    Semi,
}

/// Tokenizer over `data[pos..end]`.
#[derive(Clone)]
pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

fn damaged(what: &str, at: usize) -> anyhow::Error {
    anyhow!("The STEP file is damaged: {what} (at byte {at}).")
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Lexer<'a> {
        Lexer {
            data,
            pos: 0,
            end: data.len(),
        }
    }

    /// A lexer over `data[start..end]` (empty when `start` is past `end`).
    pub fn span(data: &'a [u8], start: usize, end: usize) -> Lexer<'a> {
        let end = end.min(data.len());
        Lexer {
            data,
            pos: start.min(end),
            end,
        }
    }

    /// The byte at `i`, if it is inside the lexer's range.
    fn byte(&self, i: usize) -> Option<u8> {
        if i < self.end {
            self.data.get(i).copied()
        } else {
            None
        }
    }

    /// `data[from..to]`, empty if the range is outside the data.
    fn bytes(&self, from: usize, to: usize) -> &'a [u8] {
        self.data.get(from..to).unwrap_or_default()
    }

    fn skip_space(&mut self) -> Result<()> {
        loop {
            while self.byte(self.pos).is_some_and(|b| b <= b' ') {
                self.pos += 1;
            }
            if self.bytes(self.pos, self.end).starts_with(b"/*") {
                let body = self.bytes(self.pos + 2, self.end);
                match body.windows(2).position(|w| w == b"*/") {
                    Some(i) => self.pos += 2 + i + 2,
                    None => return Err(damaged("unterminated comment", self.pos)),
                }
            } else {
                return Ok(());
            }
        }
    }

    fn take_while(&mut self, f: impl Fn(u8) -> bool) -> &'a [u8] {
        let start = self.pos;
        while self.byte(self.pos).is_some_and(&f) {
            self.pos += 1;
        }
        self.bytes(start, self.pos)
    }

    fn peek_byte(&self) -> Option<u8> {
        self.byte(self.pos)
    }

    /// The next token and the byte offset where it starts.
    pub fn next_at(&mut self) -> Result<Option<(usize, Tok<'a>)>> {
        self.skip_space()?;
        let start = self.pos;
        let Some(b) = self.peek_byte() else {
            return Ok(None);
        };
        let tok = match b {
            b'(' | b')' | b',' | b'=' | b';' | b'$' | b'*' => {
                self.pos += 1;
                match b {
                    b'(' => Tok::Open,
                    b')' => Tok::Close,
                    b',' => Tok::Comma,
                    b'=' => Tok::Eq,
                    b';' => Tok::Semi,
                    b'$' => Tok::Null,
                    _ => Tok::Omitted,
                }
            }
            b'\'' => {
                self.pos += 1;
                let body = self.pos;
                loop {
                    match self.peek_byte() {
                        None => return Err(damaged("unterminated string", start)),
                        Some(b'\'') if self.data.get(self.pos + 1) == Some(&b'\'') => self.pos += 2,
                        Some(b'\'') => break,
                        Some(_) => self.pos += 1,
                    }
                }
                let s = self.bytes(body, self.pos);
                self.pos += 1;
                Tok::Str(s)
            }
            b'"' => {
                self.pos += 1;
                self.take_while(|c| c != b'"');
                if self.peek_byte() != Some(b'"') {
                    return Err(damaged("unterminated binary value", start));
                }
                self.pos += 1;
                Tok::Other
            }
            b'#' | b'@' => {
                self.pos += 1;
                let digits = self.take_while(|c| c.is_ascii_digit());
                if b == b'#' && !digits.is_empty() {
                    let id = std::str::from_utf8(digits)
                        .ok()
                        .and_then(|d| d.parse::<u64>().ok())
                        .ok_or_else(|| damaged("entity number out of range", start))?;
                    Tok::Ref(id)
                } else {
                    if digits.is_empty()
                        && self
                            .take_while(|c| c.is_ascii_alphanumeric() || c == b'_')
                            .is_empty()
                    {
                        return Err(damaged("invalid name", start));
                    }
                    Tok::Other
                }
            }
            b'.' => {
                self.pos += 1;
                let name = self.take_while(|c| c.is_ascii_alphanumeric() || c == b'_');
                if name.is_empty() || self.peek_byte() != Some(b'.') {
                    return Err(damaged("invalid enumeration", start));
                }
                self.pos += 1;
                Tok::Enum(name)
            }
            b'+' | b'-' | b'0'..=b'9' => self.number(start)?,
            b'!' | b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                if b == b'!' {
                    self.pos += 1;
                }
                let name = self.take_while(|c| c.is_ascii_alphanumeric() || c == b'_');
                if name.is_empty() {
                    return Err(damaged("invalid keyword", start));
                }
                Tok::Keyword(name)
            }
            _ => return Err(damaged("unexpected character", start)),
        };
        Ok(Some((start, tok)))
    }

    pub fn next(&mut self) -> Result<Option<Tok<'a>>> {
        Ok(self.next_at()?.map(|(_, t)| t))
    }

    fn number(&mut self, start: usize) -> Result<Tok<'a>> {
        if matches!(self.peek_byte(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        if self.take_while(|c| c.is_ascii_digit()).is_empty() {
            return Err(damaged("invalid number", start));
        }
        let mut real = false;
        if self.peek_byte() == Some(b'.') {
            real = true;
            self.pos += 1;
            self.take_while(|c| c.is_ascii_digit());
        }
        if matches!(self.peek_byte(), Some(b'E' | b'e')) {
            let mut at = self.pos + 1;
            if matches!(self.data.get(at), Some(b'+' | b'-')) {
                at += 1;
            }
            if self.byte(at).is_some_and(|b| b.is_ascii_digit()) {
                real = true;
                self.pos = at;
                self.take_while(|c| c.is_ascii_digit());
            }
        }
        // ASCII by construction
        let text = std::str::from_utf8(self.bytes(start, self.pos)).unwrap_or("x");
        if real {
            match text.parse::<f64>() {
                Ok(v) if v.is_finite() => Ok(Tok::Real(v)),
                _ => Err(damaged("number out of range", start)),
            }
        } else {
            text.parse::<i64>()
                .map(Tok::Int)
                .map_err(|_| damaged("number out of range", start))
        }
    }

    /// Skips tokens up to the `;` that ends the statement (`depth` = open
    /// parentheses already read).
    fn skip_statement(&mut self, mut depth: usize) -> Result<()> {
        loop {
            match self.next_at()? {
                None => return Err(damaged("unexpected end of file", self.pos)),
                Some((at, Tok::Open)) => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err(damaged("too deeply nested", at));
                    }
                }
                Some((at, Tok::Close)) => {
                    if depth == 0 {
                        return Err(damaged("unbalanced parentheses", at));
                    }
                    depth -= 1;
                }
                Some((_, Tok::Semi)) if depth == 0 => return Ok(()),
                Some(_) => {}
            }
        }
    }
}

/// One entity instance of the DATA section: `#id = NAME(…);` or the complex
/// form `#id = (A(…) B(…));`.
#[derive(Clone, Copy, Debug)]
pub struct Instance<'a> {
    pub id: u64,
    /// Byte range of the whole instance, from `#` to `;`.
    pub start: usize,
    pub end: usize,
    /// The entity name; for a complex instance the first record's name.
    pub name: &'a [u8],
    pub complex: bool,
}

impl Instance<'_> {
    /// Whether the (first) entity name is `name` (ASCII case-insensitive).
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name.as_bytes())
    }
}

/// The instances of all DATA sections of `data`.
pub fn scan(data: &[u8]) -> Result<Vec<Instance<'_>>> {
    let mut lx = Lexer::new(data);
    let mut out = Vec::new();
    let mut in_data = false;
    while let Some((start, tok)) = lx.next_at()? {
        match tok {
            Tok::Keyword(k) if !in_data && k.eq_ignore_ascii_case(b"DATA") => {
                match lx.next()? {
                    Some(Tok::Semi) => {}
                    Some(Tok::Open) => lx.skip_statement(1)?,
                    _ => return Err(damaged("invalid DATA section", start)),
                }
                in_data = true;
            }
            // `END-ISO-10303-21;` – the rest is the signature, if any
            Tok::Keyword(k) if !in_data && k.eq_ignore_ascii_case(b"END") => break,
            Tok::Keyword(k) if in_data && k.eq_ignore_ascii_case(b"ENDSEC") => {
                if lx.next()? != Some(Tok::Semi) {
                    return Err(damaged("invalid end of section", start));
                }
                in_data = false;
            }
            Tok::Ref(id) if in_data => {
                if lx.next()? != Some(Tok::Eq) {
                    return Err(damaged("'=' expected", start));
                }
                let (name, complex) = match lx.next()? {
                    Some(Tok::Keyword(k)) => (k, false),
                    Some(Tok::Open) => match lx.next()? {
                        Some(Tok::Keyword(k)) => (k, true),
                        _ => return Err(damaged("invalid complex instance", start)),
                    },
                    _ => return Err(damaged("entity name expected", start)),
                };
                lx.skip_statement(usize::from(complex))?;
                out.push(Instance {
                    id,
                    start,
                    end: lx.pos,
                    name,
                    complex,
                });
            }
            _ if in_data => return Err(damaged("entity instance expected", start)),
            // header entities, `HEADER;`, `ENDSEC;`, `ISO-10303-21;`
            _ => lx.skip_statement(0)?,
        }
    }
    Ok(out)
}

/// The names of the records of an instance (one for a simple instance).
pub fn record_names<'a>(data: &'a [u8], inst: &Instance<'a>) -> Result<Vec<&'a [u8]>> {
    if !inst.complex {
        return Ok(vec![inst.name]);
    }
    let mut lx = Lexer::span(data, inst.start, inst.end);
    let mut names = Vec::new();
    let mut depth = 0usize;
    while let Some(tok) = lx.next()? {
        match tok {
            Tok::Open => depth += 1,
            Tok::Close => depth = depth.saturating_sub(1),
            Tok::Keyword(k) if depth == 1 => names.push(k),
            _ => {}
        }
    }
    Ok(names)
}

/// Every instance an instance refers to.
pub fn references(data: &[u8], inst: &Instance<'_>) -> Result<Vec<u64>> {
    let mut lx = Lexer::span(data, inst.start, inst.end);
    lx.next()?; // own name
    let mut refs = Vec::new();
    while let Some(tok) = lx.next()? {
        if let Tok::Ref(id) = tok {
            refs.push(id);
        }
    }
    Ok(refs)
}

/// Appends the instance in the compact form the `ruststep` parser of the
/// B-rep reader accepts, followed by a newline.
pub fn write_normalized(data: &[u8], inst: &Instance<'_>, out: &mut Vec<u8>) -> Result<()> {
    use std::io::Write;
    let mut lx = Lexer::span(data, inst.start, inst.end);
    let mut after_keyword = false;
    while let Some(tok) = lx.next()? {
        let keyword = matches!(tok, Tok::Keyword(_));
        match tok {
            Tok::Ref(id) => write!(out, "#{id}")?,
            Tok::Keyword(k) => out.extend(k.iter().map(u8::to_ascii_uppercase)),
            Tok::Int(v) => write!(out, "{v}")?,
            Tok::Real(v) => write_real(v, out),
            Tok::Str(s) => {
                out.push(b'\'');
                // the quotes of `''` become spaces; the parser accepts
                // printable ASCII, the name is not needed for geometry
                out.extend(s.iter().map(|&c| match c {
                    b'\'' => b' ',
                    b' '..=b'~' => c,
                    _ => b'?',
                }));
                out.push(b'\'');
            }
            Tok::Enum(e) => {
                out.push(b'.');
                out.extend(e.iter().map(u8::to_ascii_uppercase));
                out.push(b'.');
            }
            Tok::Other | Tok::Null => out.push(b'$'),
            Tok::Omitted => out.push(b'*'),
            Tok::Open => {
                // `()` as a parameter (an empty list) is not understood
                let mut ahead = lx.clone();
                if !after_keyword && matches!(ahead.next()?, Some(Tok::Close)) {
                    lx = ahead;
                    out.push(b'$');
                } else {
                    out.push(b'(');
                }
            }
            Tok::Close => out.push(b')'),
            Tok::Comma => out.push(b','),
            Tok::Eq => out.push(b'='),
            Tok::Semi => out.push(b';'),
        }
        after_keyword = keyword;
    }
    out.push(b'\n');
    Ok(())
}

/// A real in the form `ruststep` expects: digits, `.`, exponent.
fn write_real(v: f64, out: &mut Vec<u8>) {
    let s = format!("{v:E}"); // shortest round trip: "1.5E0", "1E-5"
    let (mantissa, exponent) = s.split_once('E').unwrap_or((&s, "0"));
    out.extend_from_slice(mantissa.as_bytes());
    if !mantissa.contains('.') {
        out.push(b'.');
    }
    out.push(b'E');
    out.extend_from_slice(exponent.as_bytes());
}

/// Streaming access to the parameters of a simple instance.
pub struct Params<'a> {
    lx: Lexer<'a>,
}

impl<'a> Params<'a> {
    /// Positioned before the first parameter of `inst` (simple instances).
    pub fn of(data: &'a [u8], inst: &Instance<'a>) -> Result<Params<'a>> {
        let mut lx = Lexer::span(data, inst.start, inst.end);
        let head = (lx.next()?, lx.next()?, lx.next()?, lx.next()?);
        if inst.complex || !matches!(head, (_, _, Some(Tok::Keyword(_)), Some(Tok::Open))) {
            bail!("#{} is not a simple entity instance", inst.id);
        }
        Ok(Params { lx })
    }

    fn tok(&mut self) -> Result<Tok<'a>> {
        self.lx
            .next()?
            .ok_or_else(|| anyhow!("unexpected end of an entity instance"))
    }

    fn invalid(&self, what: &str) -> anyhow::Error {
        damaged(what, self.lx.pos)
    }

    /// The `,` between two parameters.
    pub fn comma(&mut self) -> Result<()> {
        match self.tok()? {
            Tok::Comma => Ok(()),
            _ => Err(self.invalid("',' expected")),
        }
    }

    /// Skips one parameter.
    pub fn skip(&mut self) -> Result<()> {
        let mut depth = 0usize;
        loop {
            match self.tok()? {
                Tok::Open => depth += 1,
                Tok::Close if depth == 0 => return Err(self.invalid("parameter expected")),
                Tok::Close => depth -= 1,
                // typed parameter `NAME(…)`: its list follows
                Tok::Keyword(_) => continue,
                Tok::Semi => return Err(self.invalid("parameter expected")),
                _ => {}
            }
            if depth == 0 {
                return Ok(());
            }
        }
    }

    /// An entity reference; `$` and `*` are `None`.
    pub fn reference(&mut self) -> Result<Option<u64>> {
        match self.tok()? {
            Tok::Ref(id) => Ok(Some(id)),
            Tok::Null | Tok::Omitted => Ok(None),
            _ => Err(self.invalid("entity reference expected")),
        }
    }

    pub fn int(&mut self) -> Result<i64> {
        match self.tok()? {
            Tok::Int(v) => Ok(v),
            _ => Err(self.invalid("integer expected")),
        }
    }

    /// A real (integers are accepted as well).
    pub fn real(&mut self) -> Result<f64> {
        match self.tok()? {
            Tok::Real(v) => Ok(v),
            Tok::Int(v) => Ok(v as f64),
            _ => Err(self.invalid("number expected")),
        }
    }

    /// A list `(a, b, …)`: `item` reads each element. `$` and `()` are
    /// empty lists.
    pub fn list(&mut self, mut item: impl FnMut(&mut Params<'a>) -> Result<()>) -> Result<()> {
        match self.tok()? {
            Tok::Null | Tok::Omitted => return Ok(()),
            Tok::Open => {}
            _ => return Err(self.invalid("list expected")),
        }
        let mut ahead = self.lx.clone();
        if ahead.next()? == Some(Tok::Close) {
            self.lx = ahead;
            return Ok(());
        }
        loop {
            item(self)?;
            match self.tok()? {
                Tok::Comma => {}
                Tok::Close => return Ok(()),
                _ => return Err(self.invalid("',' or ')' expected")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &[u8] =
        b"ISO-10303-21;\nHEADER;\nFILE_NAME('a;b','',(''),(''),'','','');\nENDSEC;\nDATA;\n\
        #1 = CARTESIAN_POINT ( 'it''s', ( 1. , -2.5E+01, 3 ) ) ;\n\
        /* comment ; with semicolon */\n\
        #2=(\nLENGTH_UNIT()\nNAMED_UNIT(*)\nSI_UNIT(.MILLI.,.METRE.)\n);\n\
        #3=X('',(),\"0FF\",$,#1);\n\
        ENDSEC;\nEND-ISO-10303-21;\n";

    #[test]
    fn scans_simple_and_complex_instances() {
        let insts = scan(FILE).unwrap();
        assert_eq!(insts.len(), 3);
        assert!(insts[0].is("cartesian_point") && !insts[0].complex);
        assert!(insts[1].complex && insts[1].is("LENGTH_UNIT"));
        assert_eq!(
            record_names(FILE, &insts[1]).unwrap(),
            vec![&b"LENGTH_UNIT"[..], b"NAMED_UNIT", b"SI_UNIT"]
        );
        assert_eq!(references(FILE, &insts[2]).unwrap(), vec![1]);
        let mut out = Vec::new();
        for i in &insts {
            write_normalized(FILE, i, &mut out).unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "#1=CARTESIAN_POINT('it  s',(1.E0,-2.5E1,3));\n\
             #2=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));\n\
             #3=X('',$,$,$,#1);\n"
        );
    }

    #[test]
    fn the_kernel_parser_accepts_the_normalized_form() {
        let head = "ISO-10303-21;\nHEADER;\nFILE_NAME('');\nENDSEC;\nDATA;\n";
        let tail = "ENDSEC;\nEND-ISO-10303-21;\n";
        // (instance, accepted as written) – one empty list in a whole file
        // (the tessellated PMI of AP242 is full of them) and the kernel's
        // parser rejects the file
        for (body, raw_accepted) in [
            ("#1=(\nA()\nB()\n);", true),
            ("#1=X( 1 , 2 );", true),
            ("#1=X('',(),(1));", false),
            ("#1=X('it''s');", false),
            ("#1=X(\"0F\");", false),
            ("#1=X(1.e5);", false),
        ] {
            let raw = format!("{head}{body}\n{tail}");
            let parses = |text: &str| truck_stepio::r#in::ruststep::parser::parse(text).is_ok();
            assert_eq!(parses(&raw), raw_accepted, "{body}");
            let insts = scan(raw.as_bytes()).unwrap();
            let mut out = head.as_bytes().to_vec();
            write_normalized(raw.as_bytes(), &insts[0], &mut out).unwrap();
            out.extend_from_slice(tail.as_bytes());
            assert!(parses(&String::from_utf8(out).unwrap()), "{body}");
        }
    }

    #[test]
    fn reads_parameters_as_a_stream() {
        let insts = scan(FILE).unwrap();
        let mut p = Params::of(FILE, &insts[0]).unwrap();
        p.skip().unwrap();
        p.comma().unwrap();
        let mut v = Vec::new();
        p.list(|p| {
            v.push(p.real()?);
            Ok(())
        })
        .unwrap();
        assert_eq!(v, vec![1.0, -25.0, 3.0]);
        let mut p = Params::of(FILE, &insts[2]).unwrap();
        p.skip().unwrap();
        p.comma().unwrap();
        p.list(|_| panic!("empty list")).unwrap();
        p.comma().unwrap();
        p.skip().unwrap();
        p.comma().unwrap();
        assert_eq!(p.reference().unwrap(), None);
        p.comma().unwrap();
        assert_eq!(p.reference().unwrap(), Some(1));
        assert!(p.comma().is_err(), "')' follows");
        assert!(Params::of(FILE, &insts[1]).is_err(), "complex");
    }

    #[test]
    fn damaged_files_are_errors() {
        let bad: &[&[u8]] = &[
            b"ISO-10303-21;DATA;#1=A('unterminated);ENDSEC;",
            b"ISO-10303-21;DATA;#1=A(1,2;ENDSEC;",
            b"ISO-10303-21;DATA;#1=A(1));ENDSEC;",
            b"ISO-10303-21;DATA;#1=A(99999999999999999999);ENDSEC;",
            b"ISO-10303-21;DATA;#99999999999999999999999=A();ENDSEC;",
            b"ISO-10303-21;DATA;#1=A(1.E999);ENDSEC;",
            b"ISO-10303-21;DATA;#1=A(.T);ENDSEC;",
            b"ISO-10303-21;DATA;#1=A(\x01\xff);ENDSEC;",
            b"ISO-10303-21;DATA;/* open",
            b"ISO-10303-21;DATA;A();ENDSEC;",
            b"ISO-10303-21;DATA;#1 A();ENDSEC;",
        ];
        for b in bad {
            assert!(scan(b).is_err(), "{}", String::from_utf8_lossy(b));
        }
        let deep = format!(
            "ISO-10303-21;DATA;#1=A({}{});ENDSEC;",
            "(".repeat(100_000),
            ")".repeat(100_000)
        );
        assert!(scan(deep.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("nested"));
        // Latin-1 inside strings is fine
        let latin = b"ISO-10303-21;DATA;#1=PRODUCT('Geh\xe4use');ENDSEC;END-ISO-10303-21;";
        assert_eq!(scan(latin).unwrap().len(), 1);
        assert!(scan(b"").unwrap().is_empty());
    }

    #[test]
    fn reals_are_written_as_the_parser_expects() {
        for (v, s) in [
            (0.0, "0.E0"),
            (-1.5, "-1.5E0"),
            (1e-5, "1.E-5"),
            (123456.789, "1.23456789E5"),
        ] {
            let mut out = Vec::new();
            write_real(v, &mut out);
            assert_eq!(String::from_utf8(out).unwrap(), s);
        }
    }
}
