//! PTC Creo parts (`.prt.N`): a preview mesh tessellated from the exact
//! B-rep the file stores – Creo keeps no display tessellation of the body.
//! See `docs/src/research/creo-brep-preview.md`.
//!
//! * `container`: the text table of contents and the LZW-compressed sections;
//! * `tagged`: the self-describing binary format of a section and its
//!   compact numbers;
//! * `brep`: faces (plane, cylinder, cone, torus) with their contours and the
//!   edges sampled in both faces' parameter spaces; the per-face
//!   parametrisation is resolved across shared edges;
//! * `tess`: regions per face, ear clipping in the parameter plane, welding,
//!   PN refinement along the analytic normals.

pub mod brep;
pub mod container;
pub mod freeform;
pub mod tagged;
pub mod tess;

use anyhow::{anyhow, bail, Result};

use crate::mesh::Mesh;

/// The section with the visible B-rep.
pub const VISIBLE_GEOMETRY: &str = "VisibGeom";

/// The preview mesh of a Creo part (bytes of the whole file), in the model's
/// own length unit (the NIST parts are millimetres or inches).
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
    tess::build(&coarse)
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
