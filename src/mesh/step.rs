//! STEP (ISO 10303-21: AP203, AP214, AP242) for the 3D preview.
//!
//! Two sources of triangles, tried in this order:
//!
//! 1. **B-rep** (`CLOSED_SHELL` / `OPEN_SHELL` with `ADVANCED_FACE`s),
//!    tessellated with the `truck` kernel. The file is first scanned by the
//!    own reader in `p21.rs`; only the geometry and topology entities the
//!    kernel knows are handed to it, re-written in the compact form its
//!    parser accepts (every other instance becomes a stub, so references
//!    still resolve). PMI, presentation and product data – where real
//!    AP242 files use syntax the kernel's parser rejects – never reach it.
//!    Each shell is tessellated on its own; when the kernel panics on a
//!    shell, its faces are tessellated one by one, so one bad face costs
//!    that face, not the model.
//! 2. **Tessellated geometry** of AP242 (`TRIANGULATED_FACE`,
//!    `COMPLEX_TRIANGULATED_FACE`, `…_SURFACE_SET`, see `tessellated.rs`)
//!    when there is no B-rep or it is incomplete.
//!
//! A preview that shows fewer faces than the file has is marked
//! ([`Mesh::faces`]); the viewer says "partial preview: N of M faces".
//! Units and assembly placements are not applied (the kernel reads every
//! shell in its own coordinates and the file's length unit).

use std::collections::HashMap;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};

use anyhow::{anyhow, bail, Context, Result};

use super::p21::{self, Instance};
use super::{check_triangles, fan, tessellated, Mesh, MAX_TRIANGLES};

/// Larger STEP files are not previewed (the kernel needs several times the
/// file size in memory and would not finish within the time limit).
pub const MAX_STEP_BYTES: usize = 256 * 1024 * 1024;

/// Simple entities the B-rep reader of `truck-stepio` understands.
const BREP_ENTITIES: &[&str] = &[
    "CARTESIAN_POINT",
    "DIRECTION",
    "VECTOR",
    "PLACEMENT",
    "AXIS1_PLACEMENT",
    "AXIS2_PLACEMENT_2D",
    "AXIS2_PLACEMENT_3D",
    "LINE",
    "POLYLINE",
    "B_SPLINE_CURVE_WITH_KNOTS",
    "BEZIER_CURVE",
    "QUASI_UNIFORM_CURVE",
    "UNIFORM_CURVE",
    "CIRCLE",
    "ELLIPSE",
    "HYPERBOLA",
    "PARABOLA",
    "PCURVE",
    "SURFACE_CURVE",
    "SEAM_CURVE",
    "PLANE",
    "SPHERICAL_SURFACE",
    "CYLINDRICAL_SURFACE",
    "TOROIDAL_SURFACE",
    "CONICAL_SURFACE",
    "B_SPLINE_SURFACE_WITH_KNOTS",
    "UNIFORM_SURFACE",
    "QUASI_UNIFORM_SURFACE",
    "BEZIER_SURFACE",
    "SURFACE_OF_LINEAR_EXTRUSION",
    "SURFACE_OF_REVOLUTION",
    "VERTEX_POINT",
    "EDGE_CURVE",
    "ORIENTED_EDGE",
    "EDGE_LOOP",
    "FACE_BOUND",
    "FACE_OUTER_BOUND",
    "FACE_SURFACE",
    "ADVANCED_FACE",
    "ORIENTED_FACE",
    "OPEN_SHELL",
    "CLOSED_SHELL",
    "ORIENTED_OPEN_SHELL",
    "ORIENTED_CLOSED_SHELL",
    "DEFINITIONAL_REPRESENTATION",
];

/// Records that make a complex instance a (rational) B-spline the kernel reads.
const BREP_COMPLEX: [&str; 2] = ["B_SPLINE_CURVE", "B_SPLINE_SURFACE"];

const SHELLS: [&str; 4] = [
    "OPEN_SHELL",
    "CLOSED_SHELL",
    "ORIENTED_OPEN_SHELL",
    "ORIENTED_CLOSED_SHELL",
];

/// The minimal header the kernel's parser expects.
const HEADER: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('','',(''),(''),'','','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n";

/// Triangles of one source and how many of the file's faces they show.
struct Found {
    mesh: Mesh,
    shown: usize,
    total: usize,
}

impl Found {
    fn into_mesh(mut self) -> Mesh {
        if self.shown < self.total {
            self.mesh.faces = Some((self.shown as u32, self.total as u32));
        }
        self.mesh
    }
}

pub fn read(data: &[u8]) -> Result<Mesh> {
    if data.len() > MAX_STEP_BYTES {
        bail!(
            "The STEP file is too large to preview ({} MB, at most {} MB) – download it instead.",
            data.len() / (1024 * 1024),
            MAX_STEP_BYTES / (1024 * 1024)
        );
    }
    let start = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    let start = start.trim_ascii_start();
    if !start.starts_with(b"ISO-10303-21") {
        bail!("Not a STEP file (ISO-10303-21 header missing).");
    }
    let insts = p21::scan(data)?;
    let by_id: HashMap<u64, usize> = insts.iter().enumerate().map(|(i, x)| (x.id, i)).collect();

    let mut partial = None;
    let mut brep_error = None;
    if insts.iter().any(|i| SHELLS.iter().any(|s| i.is(s))) {
        match brep(data, &insts) {
            Ok(found) if found.shown >= found.total => return Ok(found.into_mesh()),
            Ok(found) => partial = Some(found),
            Err(e) => brep_error = Some(e),
        }
    }
    let faces = tessellated::shape_faces(data, &insts, &by_id)?;
    if !faces.is_empty() {
        let (mesh, shown) = match tessellated::read(data, &insts, &by_id, &faces) {
            Ok(read) => read,
            // too many triangles: the partial B-rep is still worth showing
            Err(_) if partial.is_some() => (Mesh::default(), 0),
            Err(e) => return Err(e),
        };
        let found = Found {
            mesh,
            shown,
            total: faces.len(),
        };
        let better = partial
            .as_ref()
            .is_none_or(|b| found.shown * b.total >= b.shown * found.total);
        if !found.mesh.indices.is_empty() && better {
            return Ok(found.into_mesh());
        }
    }
    if let Some(found) = partial {
        return Ok(found.into_mesh());
    }
    Err(brep_error.unwrap_or_else(|| {
        anyhow!("The STEP file contains no solids, surfaces or tessellated geometry the preview can show.")
    }))
}

/// Whether the B-rep reader gets the instance.
fn for_brep(data: &[u8], inst: &Instance<'_>) -> Result<bool> {
    if !inst.complex {
        return Ok(BREP_ENTITIES.iter().any(|e| inst.is(e)));
    }
    Ok(p21::record_names(data, inst)?.iter().any(|n| {
        BREP_COMPLEX
            .iter()
            .any(|e| n.eq_ignore_ascii_case(e.as_bytes()))
    }))
}

/// The B-rep geometry as the kernel's parser reads it.
fn brep_text(data: &[u8], insts: &[Instance<'_>]) -> Result<String> {
    let mut text = Vec::with_capacity(data.len() / 2 + HEADER.len() + 32);
    text.extend_from_slice(HEADER);
    for inst in insts {
        if for_brep(data, inst)? {
            p21::write_normalized(data, inst, &mut text)?;
        } else {
            writeln!(text, "#{}=X();", inst.id)?;
        }
    }
    text.extend_from_slice(b"ENDSEC;\nEND-ISO-10303-21;\n");
    String::from_utf8(text).context("Internal error: STEP text not ASCII")
}

fn brep(data: &[u8], insts: &[Instance<'_>]) -> Result<Found> {
    use truck_stepio::r#in::Table;

    let text = brep_text(data, insts)?;
    let table = catch_unwind(|| Table::from_step(&text))
        .ok()
        .flatten()
        .context("The STEP file could not be parsed by the B-rep reader.")?;
    drop(text);
    let mut shells: Vec<_> = table.shell.iter().collect();
    shells.sort_by_key(|(id, _)| **id);
    let mut out = Mesh::default();
    let (mut shown, mut total) = (0usize, 0usize);
    for (_, holder) in shells {
        let faces = holder.cfs_faces.len();
        total += faces;
        // the kernel may panic on geometry it does not support
        match catch_unwind(AssertUnwindSafe(|| shell_mesh(&table, holder))) {
            Ok(Ok((part, n))) if !part.indices.is_empty() => {
                shown += n.min(faces);
                out.append(part);
                check_triangles(out.triangle_count())?;
            }
            Ok(Err(e)) if e.is::<TooManyTriangles>() => return Err(e),
            _ => {}
        }
    }
    if out.indices.is_empty() {
        bail!("The B-rep reader could not tessellate the {total} face(s) of this STEP file (unsupported surface types?).");
    }
    Ok(Found {
        mesh: out,
        shown,
        total,
    })
}

/// One shell as a mesh, and how many of its faces it shows.
fn shell_mesh(
    table: &truck_stepio::r#in::Table,
    holder: &truck_stepio::r#in::ShellHolder,
) -> Result<(Mesh, usize)> {
    use truck_meshalgo::prelude::*;

    // the whole shell at once (the kernel tessellates its faces in parallel)
    let whole = catch_unwind(AssertUnwindSafe(|| -> Result<Vec<PolygonMesh>> {
        let shell = table
            .to_compressed_shell(holder)
            .map_err(|e| anyhow!("{e}"))?;
        let tolerance = tolerance([shell.vertices.as_slice()])?;
        let meshed = shell.robust_triangulation(tolerance);
        Ok(meshed
            .faces
            .into_iter()
            .filter_map(|f| oriented(f.surface, f.orientation))
            .collect())
    }));
    let polygons = match whole {
        Ok(Ok(polygons)) => polygons,
        _ => {
            // the kernel panicked on one of the faces: face by face, so that
            // only the faces it cannot handle are missing
            #[cfg(test)]
            tests::FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut single = holder.clone();
            let faces = std::mem::take(&mut single.cfs_faces);
            let shells: Vec<_> = faces
                .into_iter()
                .filter_map(|face| {
                    single.cfs_faces = vec![face];
                    catch_unwind(AssertUnwindSafe(|| table.to_compressed_shell(&single)))
                        .ok()?
                        .ok()
                })
                .collect();
            let tolerance = tolerance(shells.iter().map(|s| s.vertices.as_slice()))?;
            let mut polygons = Vec::new();
            for shell in &shells {
                if let Ok(meshed) =
                    catch_unwind(AssertUnwindSafe(|| shell.robust_triangulation(tolerance)))
                {
                    polygons.extend(
                        meshed
                            .faces
                            .into_iter()
                            .filter_map(|f| oriented(f.surface, f.orientation)),
                    );
                }
            }
            polygons
        }
    };
    let shown = polygons.len();
    let triangles = polygons
        .iter()
        .flat_map(|p| p.face_iter())
        .map(|f| f.len().saturating_sub(2))
        .sum();
    if triangles > MAX_TRIANGLES {
        return Err(TooManyTriangles(triangles).into());
    }
    let mut polygon = PolygonMesh::default();
    for p in polygons {
        polygon.merge(p);
    }
    polygon
        .put_together_same_attrs(TOLERANCE * 50.0)
        .remove_degenerate_faces()
        .remove_unused_attrs();
    let mut part = Mesh {
        parts: 1,
        ..Mesh::default()
    };
    for p in polygon.positions() {
        part.vertex([p[0], p[1], p[2]]);
    }
    let n = part.vertex_count();
    let mut corners = Vec::new();
    for face in polygon.face_iter() {
        corners.clear();
        for v in face {
            if v.pos >= n {
                bail!("Internal error: tessellation index out of range.");
            }
            corners.push(v.pos as u32);
        }
        fan(&mut part, &corners);
    }
    Ok((part, shown))
}

/// Tessellation tolerance: 0.2 % of the size of the shell.
fn tolerance<'a>(
    vertices: impl IntoIterator<Item = &'a [truck_meshalgo::prelude::Point3]>,
) -> Result<f64> {
    let mut min = [f64::MAX; 3];
    let mut max = [f64::MIN; 3];
    let mut any = false;
    for v in vertices.into_iter().flatten() {
        any = true;
        for ((lo, hi), c) in min.iter_mut().zip(max.iter_mut()).zip([v.x, v.y, v.z]) {
            *lo = lo.min(c);
            *hi = hi.max(c);
        }
    }
    let diameter = if any {
        ((max[0] - min[0]).powi(2) + (max[1] - min[1]).powi(2) + (max[2] - min[2]).powi(2)).sqrt()
    } else {
        1.0
    };
    let tolerance = (diameter * 0.002).max(truck_meshalgo::prelude::TOLERANCE * 10.0);
    if !tolerance.is_finite() {
        bail!("invalid coordinates");
    }
    Ok(tolerance)
}

/// The triangles of one tessellated face in its orientation – `None` when
/// the kernel gave up on the face or left only degenerate triangles.
fn oriented(
    surface: Option<truck_meshalgo::prelude::PolygonMesh>,
    orientation: bool,
) -> Option<truck_meshalgo::prelude::PolygonMesh> {
    use truck_meshalgo::prelude::*;
    let p = surface?;
    let pos = p.positions();
    let area = p.face_iter().any(|f| {
        (2..f.len()).any(|k| {
            let at = |i: usize| f.get(i).and_then(|v| <[Point3]>::get(pos, v.pos)).copied();
            let (Some(a), Some(b), Some(c)) = (at(0), at(k - 1), at(k)) else {
                return false;
            };
            let n = (b - a).cross(c - a).magnitude2();
            n.is_finite() && n > 0.0
        })
    });
    area.then(|| if orientation { p } else { p.inverse() })
}

/// A shell beyond [`MAX_TRIANGLES`] – the whole preview fails, not the shell.
#[derive(Debug)]
struct TooManyTriangles(usize);

impl std::fmt::Display for TooManyTriangles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The model has {} triangles – more than the preview shows ({MAX_TRIANGLES}).",
            self.0
        )
    }
}

impl std::error::Error for TooManyTriangles {}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &[u8] = include_bytes!("../../tests/fixtures/cube.step");

    /// How often a shell was tessellated face by face.
    pub(super) static FALLBACKS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn fallbacks() -> usize {
        FALLBACKS.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[test]
    fn noise_around_the_brep_does_not_matter() {
        // AP242 style: PMI with escaped quotes, empty lists, binary values,
        // comments and complex instances over several lines – none of it
        // is understood by the kernel's parser, all of it is skipped.
        let text = String::from_utf8_lossy(CUBE).replace(
            "DATA;\n",
            "DATA;\n/* exported by a CAD program */\n\
             #9001=DRAUGHTING_MODEL('it''s',(),#9002);\n\
             #9002=(\n  GEOMETRIC_REPRESENTATION_CONTEXT(3)\n  GLOBAL_UNIT_ASSIGNED_CONTEXT((#9003))\n  REPRESENTATION_CONTEXT('','')\n);\n\
             #9003=(LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.));\n\
             #9004=PROPERTY('Geh\u{e4}use',\"0F\",@1);\n",
        );
        let mesh = read(text.as_bytes()).unwrap();
        assert_eq!(mesh.faces, None, "complete");
        assert!(mesh.triangle_count() >= 12);
        let b = mesh.bounds();
        assert!((b[3] - b[0] - 10.0).abs() < 1e-3, "{b:?}");
    }

    #[test]
    fn a_bad_face_costs_that_face_not_the_model() {
        // the bottom face of the cube gets a surface the kernel cannot
        // tessellate: the other five faces are shown
        let cube = String::from_utf8_lossy(CUBE).into_owned();
        let plane = "#108=PLANE('',#107);";
        let knots = |degree: u8, mult: &str, knots: &str| {
            format!("#108=B_SPLINE_SURFACE_WITH_KNOTS('',{degree},{degree},((#1,#2),(#3,#4)),.UNSPECIFIED.,.F.,.F.,.F.,{mult},{mult},{knots},{knots},.UNSPECIFIED.);")
        };
        for (from, to) in [
            (plane, "#108=UNKNOWN_SURFACE('',#107);".to_string()),
            (plane, "#108=CYLINDRICAL_SURFACE('',#107,0.);".into()),
            (plane, "#108=SPHERICAL_SURFACE('',#107,-5.);".into()),
            (plane, "#108=CONICAL_SURFACE('',#107,0.,0.);".into()),
            (
                plane,
                "#108=SURFACE_OF_LINEAR_EXTRUSION('',#41,#31);".into(),
            ),
            (plane, knots(3, "(2,2)", "(0.,1.)")),
            (plane, knots(1, "(1,1)", "(0.,0.)")),
            (
                "#107=AXIS2_PLACEMENT_3D('',#1,#26,#21);",
                "#107=AXIS2_PLACEMENT_3D('',#1,#999,#21);\n#999=DIRECTION('',(0.,0.,0.));".into(),
            ),
            (
                "#109=ADVANCED_FACE('bottom',(#106),#108,.T.);",
                "#109=ADVANCED_FACE('bottom',(),#108,.T.);".into(),
            ),
        ] {
            let mesh =
                read(cube.replace(from, &to).as_bytes()).unwrap_or_else(|e| panic!("{to}: {e}"));
            assert_eq!(mesh.faces, Some((5, 6)), "{to}");
            assert_eq!(mesh.triangle_count(), 10, "{to}");
            mesh.check().unwrap();
        }
        // unsorted knots make the kernel panic while it reads the shell:
        // the faces are then read and tessellated one by one
        let before = fallbacks();
        let mesh = read(
            cube.replace(plane, &knots(1, "(2,2)", "(1.,0.)"))
                .as_bytes(),
        )
        .unwrap();
        assert!(fallbacks() > before, "face by face");
        assert_eq!(mesh.faces, Some((5, 6)));
        assert_eq!(mesh.triangle_count(), 10);
    }

    #[test]
    fn tessellated_geometry_without_brep() {
        let text = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n\
            #1=COORDINATES_LIST('',4,((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)));\n\
            #2=TRIANGULATED_FACE('',#1,4,(),$,(),((1,2,3),(1,3,4)));\n\
            #3=TESSELLATED_SHELL('',(#2),$);\n\
            ENDSEC;\nEND-ISO-10303-21;\n";
        let mesh = read(text).unwrap();
        assert_eq!(mesh.triangle_count(), 2);
        assert_eq!(mesh.faces, None);
    }

    #[test]
    fn hostile_files_are_errors_not_panics() {
        let cube = String::from_utf8_lossy(CUBE).into_owned();
        let inputs: Vec<Vec<u8>> = vec![
            b"not a step file".to_vec(),
            b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n".to_vec(),
            // a huge integer (the kernel's parser would panic on it)
            cube.replacen("(0.,0.,0.)", "(99999999999999999999,0.,0.)", 1).into_bytes(),
            // deep nesting (the kernel's parser would overflow the stack)
            cube.replacen(
                "(0.,0.,0.)",
                &format!("{}0.{}", "(".repeat(10_000), ")".repeat(10_000)),
                1,
            )
            .into_bytes(),
            // references to nothing, wrong types, cycles
            cube.replace("VERTEX_POINT('',#", "VERTEX_POINT('',#7").into_bytes(),
            cube.replace("#1=CARTESIAN_POINT", "#1=EDGE_LOOP").into_bytes(),
            // truncated in the middle
            CUBE[..CUBE.len() / 2].to_vec(),
            // a shell with no faces, a face list that is no list
            b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=CLOSED_SHELL('',());\n#2=CLOSED_SHELL('',#1);\nENDSEC;\nEND-ISO-10303-21;\n".to_vec(),
            // coordinates beyond any tolerance, broken edges and loops
            cube.replacen("(10.,0.,0.)", "(1.E308,1.E308,-1.E308)", 1).into_bytes(),
            cube.replace("#41=LINE('',#1,#31);", "#41=CIRCLE('',#107,0.);").into_bytes(),
            cube.replace("#41=LINE('',#1,#31);", "#41=ELLIPSE('',#107,0.,0.);").into_bytes(),
            cube.replace("#31=VECTOR('',#21,1.);", "#31=VECTOR('',#21,0.);").into_bytes(),
            cube.replace("(#101,#102,#103,#104)", "(#101,#101,#113,#104)").into_bytes(),
            cube.replace("(#101,#102,#103,#104)", "(#101)").into_bytes(),
            cube.replace("('bottom',(#106)", "('bottom',(#106,#106,#116)").into_bytes(),
            cube.replace("#1=CARTESIAN_POINT('',(0.,0.,0.))", "#1=CARTESIAN_POINT('',(0.,0.))").into_bytes(),
        ];
        for input in inputs {
            let result = std::panic::catch_unwind(|| read(&input));
            assert!(
                result.is_ok(),
                "panicked: {}",
                String::from_utf8_lossy(&input[..input.len().min(200)])
            );
            if let Ok(Ok(mesh)) = result {
                mesh.check().unwrap();
            }
        }
    }
}
