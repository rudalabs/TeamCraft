//! Creo assemblies (`.asm.N`): the components and their placements, read
//! from section `NeuAsmSld` (`docs/src/research/creo-assembly-preview.md`).
//!
//! An assembly has no geometry of its own: `services::assemblies` finds each
//! component file by name (`<name>.prt` / `<name>.asm`; TeamCraft stores Creo
//! files without the iteration suffix) and puts the parts' previews
//! together. Only the direct components are listed; a sub-assembly is read
//! from its own file.

use std::io::{self, Read};
use std::path::Path;

use anyhow::{anyhow, bail, Result};

use super::{container, tagged};
use crate::nxasm::{Component, Kind};

/// The section with the component list.
pub const COMPONENTS: &str = "NeuAsmSld";
/// Largest assembly file read (the samples have 0.1–0.4 MiB).
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// Components read from one assembly at most.
const MAX_COMPONENTS: usize = 100_000;
/// Longest model name taken (Creo allows 31, newer versions 80 characters).
const MAX_NAME: usize = 255;

/// The direct components of a Creo assembly (bytes of the whole file),
/// placed in the assembly's frame, in millimetres.
pub fn components(data: &[u8]) -> Result<Vec<Component>> {
    if !container::is_creo(data) {
        bail!("not a Creo file (the file does not start with '#UGC:')");
    }
    let toc = container::toc(data)?;
    let section = container::section(data, &toc, COMPONENTS)?
        .ok_or_else(|| anyhow!("The Creo file has no component list ({COMPONENTS})."))?;
    let tree = tagged::parse(&section)?;
    let mm = super::unit_mm(data, &toc);
    let root = tree.root();
    let top = tree.child(root, "neut_assem").unwrap_or(root);
    let Some(members) = tree.child(top, "members") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for &m in tree.list(members).iter().take(MAX_COMPONENTS) {
        let Some(name) = tree.str_at(m, "name").filter(|n| is_model_name(n)) else {
            continue;
        };
        // 2: a part, 1: an assembly (skeleton assemblies too)
        let ext = match tree.int_at(m, "type") {
            Some(2) => "prt",
            Some(1) => "asm",
            _ => continue,
        };
        let vector = |field: &str| -> Option<[f64; 3]> {
            let v = tree.dbls(tree.child(m, field)?);
            <[f64; 3]>::try_from(v.as_slice())
                .ok()
                .filter(|v| v.iter().all(|x| x.is_finite()))
        };
        let (Some(e1), Some(e2), Some(e3), Some(origin)) =
            (vector("e1"), vector("e2"), vector("e3"), vector("origin"))
        else {
            continue;
        };
        out.push(Component {
            name: format!("{name}.{ext}"),
            // e1–e3 are the images of the component's axes: p lands at
            // origin + p.x·e1 + p.y·e2 + p.z·e3, the row convention
            rotation: [e1, e2, e3],
            translation: origin.map(|x| x * mm),
            layer: None,
            occurrence: None,
            document: None,
        });
    }
    Ok(out)
}

/// A model name as Creo writes it: no path, no control characters.
fn is_model_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// [`components`] of a file: an assembly when it lists any.
pub fn kind_of(path: &Path) -> io::Result<Kind> {
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_FILE)
        .read_to_end(&mut data)?;
    Ok(match components(&data) {
        Ok(c) if !c.is_empty() => Kind::Assembly(c),
        _ => Kind::Other,
    })
}

/// A member of [`sample`]: model name, sub-assembly (else part), axes
/// `e1`–`e3` and origin in the assembly's unit.
pub type SampleMember<'a> = (&'a str, bool, [[f64; 3]; 3], [f64; 3]);

/// A Creo assembly reduced to what [`components`] reads: the container with
/// a `NeuAsmSld` section for the members and a `UnitSystemDef` section
/// for the length unit (`"mm"`, `"in"`, …). For tests and the fuzz corpus.
#[doc(hidden)]
pub fn sample(members: &[SampleMember], unit: &str) -> Vec<u8> {
    let mut w = tagged::Writer::new("neut_assem");
    w.begin("assem_top")
        .int("model_class", 0)
        .end()
        .begin_array("members", members.len());
    for (k, (name, assembly, axes, origin)) in members.iter().enumerate() {
        let [e1, e2, e3] = axes;
        w.element("members")
            .int("id", 40 + k as i64)
            .int("type", if *assembly { 1 } else { 2 })
            .string("name", name)
            .string("family_name", name)
            .dbls("e1", e1)
            .dbls("e2", e2)
            .dbls("e3", e3)
            .dbls("origin", origin)
            .end();
    }
    if !members.is_empty() {
        w.end();
    }
    let structure = w.finish();
    let units = unit_section(unit);
    sample_file(
        "ASSEMBLY",
        &[
            (super::UNIT_SYSTEM, units.as_slice()),
            (COMPONENTS, structure.as_slice()),
        ],
    )
}

/// A `UnitSystemDef` section naming `unit` as the length unit, laid out like
/// Creo's: the names in order of first use, the length unit after `Offset`.
#[doc(hidden)]
pub fn unit_section(unit: &str) -> Vec<u8> {
    let mut w = tagged::Writer::new("nd_unitsystemdef_l03");
    w.int("format", 0).strings(
        "string_xar",
        &[
            "nd_unitsystemdef_l03::UnitSystemDef_L03",
            "Unit_Length",
            "Length",
            "Name",
            "length",
            "System",
            "Scale",
            "Offset",
            unit,
            "Expression",
        ],
    );
    w.finish()
}

/// A Creo container (`#UGC:2 <kind> …`, table of contents, uncompressed
/// sections) around `sections`. For tests and the fuzz corpus.
#[doc(hidden)]
pub fn sample_file(kind: &str, sections: &[(&str, &[u8])]) -> Vec<u8> {
    const ENTRY: usize = 81;
    let header = format!("#UGC:2 {kind} 1966 1560 800 1 1 15 3800 2020281 0 \\\n");
    let toc_head = format!("#UGC_TOC 1 {} {ENTRY}\n", sections.len());
    let mut offset = header.len() + toc_head.len() + ENTRY * sections.len();
    let mut table = Vec::new();
    let mut bodies = Vec::new();
    for (name, body) in sections {
        let line = format!("#{name}\n");
        let size = line.len() + body.len();
        let mut rec = format!("{name} {offset:x} {size:x} {:x} 1966 9 -1", body.len());
        rec.truncate(ENTRY - 1);
        while rec.len() < ENTRY - 1 {
            rec.push(' ');
        }
        rec.push('\n');
        table.extend(rec.into_bytes());
        bodies.extend(line.into_bytes());
        bodies.extend(*body);
        offset += size;
    }
    let mut out = header.into_bytes();
    out.extend(toc_head.into_bytes());
    out.extend(table);
    out.extend(bodies);
    out
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    const IDENTITY: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    /// A quarter turn about z: x along y, y along -x.
    const QUARTER: [[f64; 3]; 3] = [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];

    #[test]
    fn members_become_components_with_their_files_and_placements() {
        let data = sample(
            &[
                ("MS_TIP", false, IDENTITY, [0.0; 3]),
                ("MS_MID", false, QUARTER, [34.5, 0.0, -2.0]),
                ("BODY_SUB", true, IDENTITY, [0.0, 10.0, 0.0]),
            ],
            "mm",
        );
        let c = components(&data).unwrap();
        let names: Vec<_> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["MS_TIP.prt", "MS_MID.prt", "BODY_SUB.asm"]);
        // the component's x axis lands on the assembly's y axis
        assert_eq!(c[1].place([1.0, 0.0, 0.0]), [34.5, 1.0, -2.0]);
        assert_eq!(c[1].place([0.0, 2.0, 0.0]), [32.5, 0.0, -2.0]);
        assert_eq!(c[2].translation, [0.0, 10.0, 0.0]);
    }

    #[test]
    fn placements_are_converted_to_millimetres() {
        let data = sample(&[("BOLT", false, IDENTITY, [1.0, 2.0, 0.5])], "in");
        let c = components(&data).unwrap();
        assert_eq!(c[0].translation, [25.4, 50.8, 12.7]);
        let toc = container::toc(&data).unwrap();
        assert_eq!(super::super::unit_mm(&data, &toc), 25.4);
        // no unit section: millimetres
        let bare = sample_file("ASSEMBLY", &[]);
        assert_eq!(super::super::unit_mm(&bare, &[]), 1.0);
    }

    #[test]
    fn unusable_members_are_skipped() {
        let mut w = tagged::Writer::new("neut_assem");
        w.begin_array("members", 4);
        // a datum (type 3), a path as name, missing axes, one good part
        for (ty, name, axes) in [
            (3, "ASM_DEF_CSYS", true),
            (2, "C:\\x", true),
            (2, "NO_AXES", false),
            (2, "GOOD", true),
        ] {
            w.element("members").int("type", ty).string("name", name);
            if axes {
                w.dbls("e1", &[1.0, 0.0, 0.0])
                    .dbls("e2", &[0.0, 1.0, 0.0])
                    .dbls("e3", &[0.0, 0.0, 1.0]);
            }
            w.dbls("origin", &[0.0; 3]).end();
        }
        w.end();
        let data = sample_file("ASSEMBLY", &[(COMPONENTS, w.finish().as_slice())]);
        let c = components(&data).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "GOOD.prt");
    }

    #[test]
    fn other_files_and_damage_are_errors_not_panics() {
        assert!(components(b"solid stl").is_err());
        assert!(components(&[]).is_err());
        // a part has no component list
        assert!(components(&sample_file("PART", &[])).is_err());
        let data = sample(
            &[
                ("A", false, IDENTITY, [1.0; 3]),
                ("B", true, QUARTER, [2.0; 3]),
            ],
            "in",
        );
        for cut in 0..data.len() {
            let _ = components(&data[..cut]);
        }
        let mut damaged = data.clone();
        for i in (0..damaged.len()).step_by(7) {
            damaged[i] ^= 0x5a;
            let _ = components(&damaged);
        }
    }

    #[test]
    fn kind_of_reads_the_file() {
        let dir = std::env::temp_dir().join(format!("creo-asm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asm = dir.join("frame.asm");
        std::fs::write(&asm, sample(&[("A", false, IDENTITY, [0.0; 3])], "mm")).unwrap();
        assert!(matches!(kind_of(&asm).unwrap(), Kind::Assembly(c) if c.len() == 1));
        let empty = dir.join("empty.asm");
        std::fs::write(&empty, sample(&[], "mm")).unwrap();
        assert!(matches!(kind_of(&empty).unwrap(), Kind::Other));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
