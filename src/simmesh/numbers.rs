//! Numbers in result files: the binary number types of VTK, ASCII and binary number lists, indices and counts, and how tensors and vectors become one scalar per point.

use anyhow::{anyhow, bail, Context, Result};

/// Binary number types of VTK files.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Num {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Num {
    pub(super) fn size(self) -> usize {
        match self {
            Num::I8 | Num::U8 => 1,
            Num::I16 | Num::U16 => 2,
            Num::I32 | Num::U32 | Num::F32 => 4,
            Num::I64 | Num::U64 | Num::F64 => 8,
        }
    }

    /// Type names of legacy files (`float`, `vtktypeint64`, …).
    pub(super) fn legacy(name: Option<&str>) -> Result<Num> {
        let name = name.context("the data type is missing")?;
        Ok(match name.to_ascii_lowercase().as_str() {
            "char" | "vtktypeint8" | "signed_char" => Num::I8,
            "unsigned_char" | "vtktypeuint8" => Num::U8,
            "short" | "vtktypeint16" => Num::I16,
            "unsigned_short" | "vtktypeuint16" => Num::U16,
            "int" | "vtktypeint32" => Num::I32,
            "unsigned_int" | "vtktypeuint32" => Num::U32,
            "long" | "vtktypeint64" | "vtkidtype" => Num::I64,
            "unsigned_long" | "vtktypeuint64" => Num::U64,
            "float" | "vtktypefloat32" => Num::F32,
            "double" | "vtktypefloat64" => Num::F64,
            other => bail!("unsupported VTK data type '{}'", short(other)),
        })
    }

    /// Type names of XML files (`Float32`, `Int64`, …).
    pub(super) fn xml(name: Option<&str>) -> Result<Num> {
        let name = name.context("a DataArray has no type")?;
        Ok(match name {
            "Int8" | "Char" => Num::I8,
            "UInt8" => Num::U8,
            "Int16" => Num::I16,
            "UInt16" => Num::U16,
            "Int32" => Num::I32,
            "UInt32" => Num::U32,
            "Int64" => Num::I64,
            "UInt64" => Num::U64,
            "Float32" => Num::F32,
            "Float64" => Num::F64,
            other => bail!("unsupported VTK data type '{}'", short(other)),
        })
    }

    /// The value of `b` (exactly [`Num::size`] bytes; NaN if fewer).
    pub(super) fn get(self, b: &[u8], big_endian: bool) -> f64 {
        macro_rules! conv {
            ($t:ty, $n:expr) => {{
                let Some(a) = b.get(..$n).and_then(|s| <[u8; $n]>::try_from(s).ok()) else {
                    return f64::NAN;
                };
                (if big_endian {
                    <$t>::from_be_bytes(a)
                } else {
                    <$t>::from_le_bytes(a)
                }) as f64
            }};
        }
        let first = || b.first().copied();
        match self {
            Num::I8 => first().map_or(f64::NAN, |v| v as i8 as f64),
            Num::U8 => first().map_or(f64::NAN, f64::from),
            Num::I16 => conv!(i16, 2),
            Num::U16 => conv!(u16, 2),
            Num::I32 => conv!(i32, 4),
            Num::U32 => conv!(u32, 4),
            Num::I64 => conv!(i64, 8),
            Num::U64 => conv!(u64, 8),
            Num::F32 => conv!(f32, 4),
            Num::F64 => conv!(f64, 8),
        }
    }
}

pub(super) fn short(s: &str) -> String {
    s.chars().take(32).collect()
}

/// `count` binary numbers from the start of `bytes`.
pub(super) fn each_binary(
    bytes: &[u8],
    ty: Num,
    big_endian: bool,
    count: usize,
    f: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<()> {
    let need = count
        .checked_mul(ty.size())
        .context("data array too large")?;
    let data = bytes
        .get(..need)
        .context("the result file is truncated (data array shorter than declared)")?;
    for c in data.chunks_exact(ty.size()) {
        f(ty.get(c, big_endian))?;
    }
    Ok(())
}

/// `count` whitespace-separated numbers of `text`.
pub(super) fn each_ascii(
    text: &[u8],
    count: usize,
    f: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<()> {
    let mut tokens = text
        .split(|b| b.is_ascii_whitespace())
        .filter(|t| !t.is_empty());
    for _ in 0..count {
        let token = tokens
            .next()
            .context("the result file is truncated (fewer values than declared)")?;
        f(number(token)?)?;
    }
    Ok(())
}

pub(super) fn number(token: &[u8]) -> Result<f64> {
    std::str::from_utf8(token)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| {
            anyhow!(
                "'{}' is not a number",
                short(&String::from_utf8_lossy(token))
            )
        })
}

/// A point index (non-negative integer that fits `u32`).
pub(super) fn index(v: f64) -> Result<u32> {
    if v >= 0.0 && v <= u32::MAX as f64 && v.fract() == 0.0 {
        Ok(v as u32)
    } else {
        bail!("invalid point index or count in a cell")
    }
}

pub(super) fn count(v: Option<&str>, what: &str) -> Result<usize> {
    v.and_then(|s| s.parse::<usize>().ok())
        .with_context(|| format!("invalid {what}"))
}

/// Reduces the components of a tuple to one scalar.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Reduce {
    Value,
    Magnitude,
    VonMises6,
    VonMises9,
}

impl Reduce {
    pub(super) fn of(components: usize) -> Reduce {
        match components {
            0 | 1 => Reduce::Value,
            6 => Reduce::VonMises6,
            9 => Reduce::VonMises9,
            _ => Reduce::Magnitude,
        }
    }

    pub(super) fn label(self, name: &str) -> String {
        match self {
            Reduce::Value => name.to_string(),
            Reduce::Magnitude => format!("{name} (magnitude)"),
            Reduce::VonMises6 | Reduce::VonMises9 => format!("{name} (von Mises)"),
        }
    }

    pub(super) fn apply(self, c: &[f64]) -> f32 {
        let vm = |xx: f64, yy: f64, zz: f64, xy: f64, yz: f64, xz: f64| {
            (0.5 * ((xx - yy).powi(2) + (yy - zz).powi(2) + (zz - xx).powi(2))
                + 3.0 * (xy * xy + yz * yz + xz * xz))
                .sqrt()
        };
        (match (self, c) {
            (Reduce::Value, _) => c.first().copied().unwrap_or(f64::NAN),
            (Reduce::Magnitude, _) => c.iter().map(|v| v * v).sum::<f64>().sqrt(),
            (Reduce::VonMises6, &[xx, yy, zz, xy, yz, xz, ..]) => vm(xx, yy, zz, xy, yz, xz),
            // a full 3 × 3 tensor, row by row
            (Reduce::VonMises9, &[c0, c1, c2, c3, c4, c5, c6, c7, c8, ..]) => vm(
                c0,
                c4,
                c8,
                0.5 * (c1 + c3),
                0.5 * (c5 + c7),
                0.5 * (c2 + c6),
            ),
            _ => f64::NAN,
        }) as f32
    }
}

/// Collects `tuples × components` numbers into one scalar per tuple.
pub(super) struct Tuples {
    pub(super) components: usize,
    pub(super) reduce: Reduce,
    pub(super) buf: Vec<f64>,
    pub(super) out: Vec<f32>,
}

impl Tuples {
    /// `max_reserve` limits the up-front allocation (the count may lie).
    pub(super) fn new(tuples: usize, components: usize, max_reserve: usize) -> Tuples {
        Tuples {
            components: components.max(1),
            reduce: Reduce::of(components),
            buf: Vec::with_capacity(components.max(1)),
            out: Vec::with_capacity(tuples.min(max_reserve)),
        }
    }

    pub(super) fn push(&mut self, v: f64) -> Result<()> {
        self.buf.push(v);
        if self.buf.len() == self.components {
            self.out.push(self.reduce.apply(&self.buf));
            self.buf.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    const ALL: [Num; 10] = [
        Num::I8,
        Num::U8,
        Num::I16,
        Num::U16,
        Num::I32,
        Num::U32,
        Num::I64,
        Num::U64,
        Num::F32,
        Num::F64,
    ];

    #[test]
    fn type_names_of_both_file_kinds() {
        let legacy = [
            "char",
            "unsigned_char",
            "short",
            "unsigned_short",
            "int",
            "unsigned_int",
            "long",
            "unsigned_long",
            "float",
            "double",
        ];
        let xml = [
            "Int8", "UInt8", "Int16", "UInt16", "Int32", "UInt32", "Int64", "UInt64", "Float32",
            "Float64",
        ];
        for ((num, l), x) in ALL.iter().zip(legacy).zip(xml) {
            assert_eq!(Num::legacy(Some(l)).unwrap(), *num, "{l}");
            assert_eq!(Num::legacy(Some(&l.to_uppercase())).unwrap(), *num, "case");
            assert_eq!(Num::xml(Some(x)).unwrap(), *num, "{x}");
        }
        assert_eq!(Num::legacy(Some("vtkIdType")).unwrap(), Num::I64);
        assert_eq!(Num::legacy(Some("signed_char")).unwrap(), Num::I8);
        assert_eq!(Num::xml(Some("Char")).unwrap(), Num::I8);
        assert!(
            Num::xml(Some("float32")).is_err(),
            "XML names are case sensitive"
        );
        assert!(Num::legacy(None).is_err());
        assert!(Num::xml(None).is_err());
        // the message quotes at most 32 characters of the name
        let err = Num::legacy(Some(&"x".repeat(80))).unwrap_err().to_string();
        assert!(
            err.contains(&"x".repeat(32)) && !err.contains(&"x".repeat(33)),
            "{err}"
        );
        let sizes: Vec<usize> = ALL.iter().map(|n| n.size()).collect();
        assert_eq!(sizes, [1, 1, 2, 2, 4, 4, 8, 8, 4, 8]);
    }

    #[test]
    fn values_in_both_byte_orders() {
        assert_eq!(Num::I8.get(&[0xFF], false), -1.0);
        assert_eq!(Num::U8.get(&[0xFF], true), 255.0);
        assert_eq!(Num::I16.get(&(-300i16).to_be_bytes(), true), -300.0);
        assert_eq!(Num::U16.get(&300u16.to_le_bytes(), false), 300.0);
        assert_eq!(Num::I32.get(&(-7i32).to_le_bytes(), false), -7.0);
        assert_eq!(Num::U32.get(&7u32.to_be_bytes(), true), 7.0);
        assert_eq!(Num::I64.get(&(-9i64).to_be_bytes(), true), -9.0);
        assert_eq!(Num::U64.get(&9u64.to_le_bytes(), false), 9.0);
        assert_eq!(Num::F32.get(&1.5f32.to_be_bytes(), true), 1.5);
        assert_eq!(Num::F64.get(&(-2.25f64).to_le_bytes(), false), -2.25);
        // the wrong byte order gives another value, too few bytes give NaN
        assert_ne!(Num::U16.get(&300u16.to_le_bytes(), true), 300.0);
        for num in ALL {
            assert!(num.get(&[], false).is_nan(), "{num:?}");
        }
        assert!(Num::F64.get(&[0; 7], false).is_nan());
    }

    #[test]
    fn number_lists() {
        let mut got = Vec::new();
        let mut push = |v: f64| {
            got.push(v);
            Ok(())
        };
        let mut bytes = Vec::new();
        for v in [1i16, -2, 3] {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        each_binary(&bytes, Num::I16, true, 3, &mut push).unwrap();
        each_ascii(b"  4.5\n-6e1\t7 ", 3, &mut push).unwrap();
        assert_eq!(got, [1.0, -2.0, 3.0, 4.5, -60.0, 7.0]);
        let mut ignore = |_: f64| Ok(());
        let err = each_binary(&bytes, Num::I16, true, 4, &mut ignore).unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
        assert!(each_binary(&bytes, Num::F64, false, usize::MAX, &mut ignore).is_err());
        assert!(each_ascii(b"1 2", 3, &mut ignore).is_err(), "fewer values");
        assert!(
            each_ascii(b"1 x 3", 3, &mut ignore).is_err(),
            "not a number"
        );
        // the callback's error stops the list
        let mut stop = |v: f64| {
            if v > 1.0 {
                bail!("stop at {v}")
            } else {
                Ok(())
            }
        };
        assert!(each_ascii(b"1 2 3", 3, &mut stop)
            .unwrap_err()
            .to_string()
            .contains("stop at 2"));
        assert!(number(b"\xFF").is_err(), "not UTF-8");
    }

    #[test]
    fn indices_and_counts() {
        assert_eq!(index(0.0).unwrap(), 0);
        assert_eq!(index(u32::MAX as f64).unwrap(), u32::MAX);
        for bad in [-1.0, 1.5, u32::MAX as f64 + 1.0, f64::NAN, f64::INFINITY] {
            assert!(index(bad).is_err(), "{bad}");
        }
        assert_eq!(count(Some("12"), "cells").unwrap(), 12);
        let err = count(Some("-1"), "cells").unwrap_err().to_string();
        assert_eq!(err, "invalid cells");
        assert!(count(None, "points").is_err());
    }

    #[test]
    fn components_become_one_scalar() {
        assert_eq!(Reduce::of(0), Reduce::Value);
        assert_eq!(Reduce::of(1), Reduce::Value);
        assert_eq!(Reduce::of(3), Reduce::Magnitude);
        assert_eq!(Reduce::of(6), Reduce::VonMises6);
        assert_eq!(Reduce::of(9), Reduce::VonMises9);
        assert_eq!(Reduce::of(2), Reduce::Magnitude);
        assert_eq!(Reduce::Value.label("T"), "T");
        assert_eq!(Reduce::Magnitude.label("U"), "U (magnitude)");
        assert_eq!(Reduce::VonMises9.label("S"), "S (von Mises)");

        assert_eq!(Reduce::Magnitude.apply(&[3.0, 4.0]), 5.0);
        // uniaxial stress: von Mises equals the stress
        assert_eq!(
            Reduce::VonMises6.apply(&[100.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            100.0
        );
        // pure shear τ: von Mises is √3·τ
        let shear = Reduce::VonMises6.apply(&[0.0, 0.0, 0.0, 10.0, 0.0, 0.0]);
        assert!((shear - 10.0 * 3f32.sqrt()).abs() < 1e-4, "{shear}");
        // hydrostatic pressure has no von Mises stress
        assert_eq!(
            Reduce::VonMises6.apply(&[5.0, 5.0, 5.0, 0.0, 0.0, 0.0]),
            0.0
        );
        // the full 3×3 tensor gives the same as its six independent values
        let full = Reduce::VonMises9.apply(&[1.0, 4.0, 6.0, 4.0, 2.0, 5.0, 6.0, 5.0, 3.0]);
        let six = Reduce::VonMises6.apply(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert!((full - six).abs() < 1e-4, "{full} vs {six}");
        // too few components for the tensor: NaN, not a panic
        assert!(Reduce::VonMises6.apply(&[1.0, 2.0]).is_nan());
        assert!(Reduce::Value.apply(&[]).is_nan());
    }

    #[test]
    fn tuples_collect_one_value_per_tuple() {
        let mut t = Tuples::new(2, 3, 10);
        for v in [3.0, 4.0, 0.0, 0.0, 0.0, 2.0] {
            t.push(v).unwrap();
        }
        assert_eq!(t.out, [5.0, 2.0]);
        // a count of 0 components is treated as one value per tuple
        let mut single = Tuples::new(1, 0, 10);
        single.push(7.0).unwrap();
        assert_eq!(single.out, [7.0]);
        // the reservation is capped: a lying count allocates nothing big
        assert!(Tuples::new(usize::MAX, 1, 16).out.capacity() < 1024);
    }
}
