//! PTC Creo parts (`.prt.N`): a preview mesh tessellated from the exact
//! B-rep the file stores – Creo keeps no display tessellation of the body.
//! See `docs/src/research/creo-brep-preview.md`. Assemblies (`.asm.N`) are
//! put together from their parts (`asm`, `services::assemblies`).
//!
//! * `asm`: the components of an assembly and their placements;
//! * `container`: the text table of contents and the LZW-compressed sections;
//! * `tagged`: the self-describing binary format of a section and its
//!   compact numbers;
//! * `brep`: faces (plane, cylinder, cone, torus) with their contours and the
//!   edges sampled in both faces' parameter spaces; the per-face
//!   parametrisation is resolved across shared edges;
//! * `tess`: regions per face, ear clipping in the parameter plane, welding,
//!   PN refinement along the analytic normals.

pub mod asm;
pub mod brep;
pub mod container;
pub mod freeform;
pub mod tagged;
pub mod tess;

use anyhow::{anyhow, bail, Result};

use crate::mesh::Mesh;
use tagged::Node;

/// The section with the visible B-rep.
pub const VISIBLE_GEOMETRY: &str = "VisibGeom";
/// The section with the unit system (parts and assemblies).
pub const UNIT_SYSTEM: &str = "ND:0:UnitSystemDef_L03:1";

/// Millimetres per length unit of the file (1 when the file names none or
/// one not known): the unit's name follows `Offset` in the names
/// (`string_xar`) of the `UnitSystemDef` section – `mm`, `in`, …; other
/// systems give the metres per unit in `double_xar[0]`.
pub fn unit_mm(data: &[u8], toc: &[container::Entry]) -> f64 {
    let prefix = UNIT_SYSTEM.split('_').next().unwrap_or(UNIT_SYSTEM);
    let Some(entry) = toc.iter().find(|e| e.name.starts_with(prefix)) else {
        return 1.0;
    };
    let Ok(Some(section)) = container::section(data, toc, &entry.name) else {
        return 1.0;
    };
    let Ok(tree) = tagged::parse(&section) else {
        return 1.0;
    };
    let root = tree.root();
    let names: Vec<&str> = tree
        .child(root, "string_xar")
        .map(|l| {
            tree.list(l)
                .iter()
                .filter_map(|&i| match tree.node(i) {
                    Node::Str(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let by_name = names
        .iter()
        .position(|&s| s == "Offset")
        .and_then(|i| names.get(i + 1))
        .and_then(|u| mm_per_unit(u));
    let by_scale = tree
        .child(root, "double_xar")
        .and_then(|l| tree.dbls(l).first().copied())
        .filter(|m| m.is_finite() && *m > 1e-9 && *m < 1e6)
        .map(|m| m * 1000.0);
    by_name.or(by_scale).unwrap_or(1.0)
}

/// Millimetres per unit of a Creo length unit name.
fn mm_per_unit(name: &str) -> Option<f64> {
    Some(match name.to_ascii_lowercase().as_str() {
        "mm" => 1.0,
        "cm" => 10.0,
        "m" => 1000.0,
        "in" | "inch" => 25.4,
        "ft" | "foot" => 304.8,
        "um" | "micron" => 0.001,
        _ => return None,
    })
}

/// The preview mesh of a Creo part (bytes of the whole file), in millimetres
/// (the NIST parts are stored in millimetres or inches).
pub(crate) fn read_part(data: &[u8]) -> Result<Mesh> {
    if !container::is_creo(data) {
        bail!("not a Creo file (the file does not start with '#UGC:')");
    }
    if data
        .get(..24)
        .is_some_and(|h| h.windows(8).any(|w| w == b"ASSEMBLY"))
    {
        bail!("A Creo assembly is shown with the previews of its parts (found by file name).");
    }
    let toc = container::toc(data)?;
    let section = container::section(data, &toc, VISIBLE_GEOMETRY)?.ok_or_else(|| {
        anyhow!("This Creo file stores no visible geometry – the preview image is shown instead.")
    })?;
    let tree = tagged::parse(&section)?;
    let mut brep = brep::extract(&tree);
    // faces of solid bodies only (loose and cosmetic surfaces have list_id -1)
    brep.surfaces
        .retain(|s| s.list_type.is_none_or(|t| t == 98));
    if brep.surfaces.is_empty() {
        bail!("The Creo part contains no solid body.");
    }
    tess::add_twins(&mut brep);
    let mut fixed = brep::resolve(&brep);
    if tess::repair_twins(&mut brep, &fixed) > 0 {
        fixed = brep::resolve(&brep);
    }
    let coarse = tess::tessellate(&brep, &fixed)?;
    let mut mesh = tess::build(&coarse)?;
    let mm = unit_mm(data, &toc);
    if mm != 1.0 {
        for p in &mut mesh.positions {
            *p = (f64::from(*p) * mm) as f32;
        }
    }
    Ok(mesh)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn ctc_01_becomes_a_closed_mesh_with_the_step_box() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cad/nist/ctc-01/creo-10/nist_ctc_01_asme1_rd_cr10030.prt.5"
        ))
        .unwrap();
        let mesh = read_part(&data).unwrap();
        mesh.check().unwrap();
        let b = mesh.bounds();
        for (got, want) in b.iter().zip([-400.0, -225.0, -100.0, 400.0, 225.0, 50.0]) {
            assert!((got - want).abs() < 0.05, "{b:?}");
        }
        assert!(mesh.triangle_count() > 10_000, "{}", mesh.triangle_count());
        assert_eq!(open_edges(&mesh), 0);
        let v = volume(&mesh);
        assert!(v > 1.4e7 && v < 1.5e7, "{v}");
    }

    /// Directed edges without their reverse.
    fn open_edges(mesh: &Mesh) -> usize {
        let mut edges = std::collections::HashMap::<(u32, u32), i32>::new();
        for t in mesh.indices.chunks(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a.min(b), a.max(b))).or_default() += if a < b { 1 } else { -1 };
            }
        }
        edges.values().filter(|&&n| n != 0).count()
    }

    fn volume(mesh: &Mesh) -> f64 {
        let p = |i: u32| {
            let k = i as usize * 3;
            [
                f64::from(mesh.positions[k]),
                f64::from(mesh.positions[k + 1]),
                f64::from(mesh.positions[k + 2]),
            ]
        };
        mesh.indices
            .chunks(3)
            .map(|t| brep::dot(p(t[0]), brep::cross(p(t[1]), p(t[2]))))
            .sum::<f64>()
            / 6.0
    }

    #[test]
    fn assemblies_and_other_files_are_refused() {
        assert!(read_part(b"#UGC:2 ASSEMBLY 1 2 3 \\\n").is_err());
        assert!(read_part(b"solid stl").is_err());
        assert!(read_part(&[]).is_err());
    }
}
