//! Triangle meshes and point clouds for the 3D preview.
//!
//! Pure-Rust readers for the common mesh, 3D-printing, visualisation and
//! point-cloud formats, plus STEP B-rep tessellation with the `truck` CAD
//! kernel (Apache-2.0). Every reader produces one [`Mesh`] in model
//! coordinates; [`Mesh::encode`] writes the compact binary form that
//! `shared/js/viewer.js` draws (`TCMESH01`, little endian).
//!
//! Simulation results (VTK, VTU/VTP, CalculiX FRD, see [`crate::simmesh`])
//! additionally carry per-vertex scalar [`Field`]s that the viewer colours
//! with a colour map; the section is optional and flagged, so older viewers
//! and cached meshes keep working.
//!
//! The readers are defensive: sizes are limited, indices are checked and a
//! malformed file is an error, never a panic or an endless loop. The server
//! still runs them in a separate process with a time limit (see `preview`).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use anyhow::{bail, Result};

pub mod creo;
mod gltf;
mod helpers;
mod inventor;
mod jt;
mod model;
mod obj;
mod off;
mod p21;
mod ply;
mod pn;
mod points;
pub mod render;
pub mod rhino;
mod step;
mod stl;
mod sw;
mod tessellated;
#[cfg(test)]
mod test_support;
mod three_ds;
mod three_mf;

pub(crate) use helpers::*;
pub use model::{color_map, Field, Mesh, MAX_FIELDS, MAX_POINTS, MAX_TRIANGLES};
pub use step::MAX_STEP_BYTES;

/// Mesh formats the readers understand (ids as in `cadformat`).
pub fn read(format: &str, data: &[u8]) -> Result<Mesh> {
    let mesh = match format {
        "stl" => stl::read(data)?,
        "obj" => obj::read(data)?,
        "ply" => ply::read(data)?,
        "off" => off::read(data)?,
        "3mf" => three_mf::read_3mf(data)?,
        "amf" => three_mf::read_amf(data)?,
        "gltf" | "glb" => gltf::read(data)?,
        "3ds" => three_ds::read(data)?,
        "pts" | "xyz" => points::read_text(data)?,
        "las" => points::read_las(data)?,
        "step" => step::read(data)?,
        "nx-part" => jt::read_nx(data)?,
        "jt" => jt::read_file(data)?,
        "solidworks-part" => sw::read_part(data)?,
        "creo-part" => creo::read_part(data)?,
        "solidworks-assembly" => bail!(
            "A SOLIDWORKS assembly is shown with the previews of its parts (found by file name)."
        ),
        "rhino" => rhino::read(data)?,
        "inventor-part" => inventor::read_part(data)?,
        "inventor-assembly" => bail!(
            "An Inventor assembly is shown with the previews of its parts (found by file name)."
        ),
        "vtk-legacy" => crate::simmesh::vtk_legacy(data)?,
        "vtk-unstructured" | "vtk-polydata" => crate::simmesh::vtk_xml(data)?,
        "calculix-frd" => crate::simmesh::frd(data)?,
        other => bail!("No 3D preview for the format '{other}'."),
    };
    mesh.check()?;
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_box_is_tessellated() {
        let mesh = read("step", include_bytes!("../tests/fixtures/cube.step")).unwrap();
        assert!(
            mesh.triangle_count() >= 12,
            "{} triangles",
            mesh.triangle_count()
        );
        let b = mesh.bounds();
        assert!((b[3] - b[0] - 10.0).abs() < 1e-3, "{b:?}");
        assert!(read(
            "step",
            b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n"
        )
        .is_err());
        assert!(read("step", b"not a step file").is_err());
    }

    #[test]
    fn read_dispatches_by_format_and_checks_the_result() {
        let err = read("dwg", b"").unwrap_err().to_string();
        assert!(err.contains("No 3D preview for the format 'dwg'"), "{err}");
        let err = read("solidworks-assembly", b"").unwrap_err().to_string();
        assert!(err.contains("previews of its parts"), "{err}");
        // glTF and GLB share a reader
        assert_eq!(
            read("glb", b"{ not json").unwrap_err().to_string(),
            read("gltf", b"{ not json").unwrap_err().to_string()
        );
        // every reader's mesh goes through `check`: a header-only PLY has no
        // geometry
        let ply = b"ply\nformat ascii 1.0\nelement vertex 0\nproperty float x\nproperty float y\nproperty float z\nend_header\n";
        let err = read("ply", ply).unwrap_err().to_string();
        assert!(err.contains("no geometry"), "{err}");
    }
}
