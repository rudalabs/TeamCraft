//! Tessellated geometry of STEP AP242 (ISO 10303-42 tessellated
//! representations): triangles the CAD program already computed, so no
//! B-rep kernel is needed.
//!
//! Read entities (all coordinates come from a `COORDINATES_LIST`):
//!
//! ```text
//! COORDINATES_LIST(name, npoints, ((x,y,z), …))
//! TRIANGULATED_FACE(name, coordinates, pnmax, normals, geometric_link, pnindex, triangles)
//! COMPLEX_TRIANGULATED_FACE(name, coordinates, pnmax, normals, geometric_link, pnindex, strips, fans)
//! TRIANGULATED_SURFACE_SET(name, coordinates, pnmax, normals, pnindex, triangles)
//! COMPLEX_TRIANGULATED_SURFACE_SET(name, coordinates, pnmax, normals, pnindex, strips, fans)
//! ```
//!
//! Indices are 1-based; with a non-empty `pnindex` they index `pnindex`,
//! which indexes the coordinates. `TESSELLATED_SHELL` / `TESSELLATED_SOLID`
//! only group faces, so the faces are read directly.
//!
//! AP242 files also carry PMI (dimensions, notes) as tessellated text and
//! lines. Faces used by an annotation (`…ANNOTATION…OCCURRENCE`, directly or
//! through a `…GEOMETRIC_SET`) are not part of the shape and are left out.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};

use super::p21::{self, Instance, Params};
use super::{check_triangles, Mesh};

const FACES: [&str; 4] = [
    "TRIANGULATED_FACE",
    "COMPLEX_TRIANGULATED_FACE",
    "TRIANGULATED_SURFACE_SET",
    "COMPLEX_TRIANGULATED_SURFACE_SET",
];

fn is_face(inst: &Instance<'_>) -> bool {
    !inst.complex && FACES.iter().any(|f| inst.is(f))
}

fn has(name: &[u8], part: &[u8]) -> bool {
    name.windows(part.len())
        .any(|w| w.eq_ignore_ascii_case(part))
}

/// Whether one of the entity names of the instance (all records of a
/// complex instance) contains every one of `parts`.
fn name_has(data: &[u8], inst: &Instance<'_>, parts: &[&[u8]]) -> Result<bool> {
    let matches = |name: &[u8]| parts.iter().all(|p| has(name, p));
    if !inst.complex {
        return Ok(matches(inst.name));
    }
    Ok(p21::record_names(data, inst)?.into_iter().any(matches))
}

/// The tessellated faces of the shape (without annotations).
pub fn shape_faces<'a>(
    data: &'a [u8],
    insts: &[Instance<'a>],
    by_id: &HashMap<u64, usize>,
) -> Result<Vec<usize>> {
    if !insts.iter().any(is_face) {
        return Ok(Vec::new());
    }
    let mut annotation = HashSet::new();
    for inst in insts {
        if !name_has(data, inst, &[b"ANNOTATION", b"OCCURRENCE"])? {
            continue;
        }
        for id in p21::references(data, inst)? {
            let Some(&i) = by_id.get(&id) else { continue };
            if is_face(&insts[i]) {
                annotation.insert(i);
            } else if name_has(data, &insts[i], &[b"GEOMETRIC_SET"])? {
                for inner in p21::references(data, &insts[i])? {
                    if let Some(&j) = by_id.get(&inner) {
                        annotation.insert(j);
                    }
                }
            }
        }
    }
    Ok(insts
        .iter()
        .enumerate()
        .filter(|(i, inst)| is_face(inst) && !annotation.contains(i))
        .map(|(i, _)| i)
        .collect())
}

/// Reads the given faces into one mesh; faces that cannot be read are
/// skipped. Returns the mesh and the number of faces read.
pub fn read(
    data: &[u8],
    insts: &[Instance<'_>],
    by_id: &HashMap<u64, usize>,
    faces: &[usize],
) -> Result<(Mesh, usize)> {
    let mut mesh = Mesh::default();
    // coordinates list → index of its first vertex and its point count
    let mut lists: HashMap<u64, Option<(u32, usize)>> = HashMap::new();
    let mut shown = 0usize;
    for &f in faces {
        let before = mesh.indices.len();
        match face(data, insts, by_id, &insts[f], &mut mesh, &mut lists) {
            Ok(true) => shown += 1,
            Ok(false) => {}
            Err(e) => {
                // a damaged face is left out, its triangles with it
                tracing::debug!(error = %e, "tessellated face skipped");
                mesh.indices.truncate(before);
            }
        }
        check_triangles(mesh.triangle_count())?;
    }
    mesh.parts = lists.values().filter(|l| l.is_some()).count().max(1) as u32;
    Ok((mesh, shown))
}

/// One face: `Ok(true)` when it added triangles.
fn face(
    data: &[u8],
    insts: &[Instance<'_>],
    by_id: &HashMap<u64, usize>,
    inst: &Instance<'_>,
    mesh: &mut Mesh,
    lists: &mut HashMap<u64, Option<(u32, usize)>>,
) -> Result<bool> {
    let link = inst.is("TRIANGULATED_FACE") || inst.is("COMPLEX_TRIANGULATED_FACE");
    let complex =
        inst.is("COMPLEX_TRIANGULATED_FACE") || inst.is("COMPLEX_TRIANGULATED_SURFACE_SET");
    let mut p = Params::of(data, inst)?;
    p.skip()?; // name
    p.comma()?;
    let Some(coords) = p.reference()? else {
        bail!("#{}: no coordinates", inst.id);
    };
    p.comma()?;
    p.skip()?; // pnmax
    p.comma()?;
    p.skip()?; // normals
    p.comma()?;
    if link {
        p.skip()?; // geometric_link
        p.comma()?;
    }
    let mut pnindex = Vec::new();
    p.list(|p| {
        pnindex.push(p.int()?);
        Ok(())
    })?;
    p.comma()?;

    let (base, count) = match lists.get(&coords) {
        Some(known) => *known,
        None => {
            let read = by_id
                .get(&coords)
                .filter(|&&i| insts[i].is("COORDINATES_LIST"))
                .map(|&i| coordinates(data, &insts[i], mesh))
                .transpose()
                .unwrap_or(None);
            lists.insert(coords, read);
            read
        }
    }
    .ok_or_else(|| anyhow::anyhow!("#{}: coordinates #{coords} missing", inst.id))?;

    // 1-based index of the face → vertex of the mesh
    let vertex = |i: i64| -> Result<u32> {
        let point = if pnindex.is_empty() {
            i
        } else {
            match usize::try_from(i - 1).ok().and_then(|k| pnindex.get(k)) {
                Some(&v) => v,
                None => bail!("index {i} out of range"),
            }
        };
        match usize::try_from(point - 1) {
            Ok(k) if k < count => Ok(base + k as u32),
            _ => bail!("point {point} out of range"),
        }
    };
    let before = mesh.indices.len();
    let add = |mesh: &mut Mesh, a: u32, b: u32, c: u32| {
        if a != b && b != c && a != c {
            mesh.indices.extend([a, b, c]);
        }
    };
    let mut strip = Vec::new();
    if complex {
        // triangle strips: (a b c), (c b d), (c d e), …
        p.list(|p| {
            strip.clear();
            p.list(|p| {
                strip.push(vertex(p.int()?)?);
                Ok(())
            })?;
            for k in 2..strip.len() {
                let (a, b, c) = if k % 2 == 0 {
                    (strip[k - 2], strip[k - 1], strip[k])
                } else {
                    (strip[k - 1], strip[k - 2], strip[k])
                };
                add(mesh, a, b, c);
            }
            check_triangles(mesh.triangle_count())
        })?;
        p.comma()?;
        // triangle fans: (a b c), (a c d), …
        p.list(|p| {
            strip.clear();
            p.list(|p| {
                strip.push(vertex(p.int()?)?);
                Ok(())
            })?;
            for k in 2..strip.len() {
                add(mesh, strip[0], strip[k - 1], strip[k]);
            }
            check_triangles(mesh.triangle_count())
        })?;
    } else {
        p.list(|p| {
            strip.clear();
            p.list(|p| {
                strip.push(vertex(p.int()?)?);
                Ok(())
            })?;
            if strip.len() != 3 {
                bail!("a triangle needs three corners");
            }
            add(mesh, strip[0], strip[1], strip[2]);
            check_triangles(mesh.triangle_count())
        })?;
    }
    Ok(mesh.indices.len() > before)
}

/// Appends the points of a `COORDINATES_LIST` to the mesh: (first vertex, count).
fn coordinates(data: &[u8], inst: &Instance<'_>, mesh: &mut Mesh) -> Result<(u32, usize)> {
    let base = mesh.vertex_count();
    let result = (|| -> Result<()> {
        let mut p = Params::of(data, inst)?;
        p.skip()?; // name
        p.comma()?;
        p.skip()?; // npoints (the list is authoritative)
        p.comma()?;
        let mut point = Vec::with_capacity(3);
        p.list(|p| {
            point.clear();
            p.list(|p| {
                point.push(p.real()?);
                Ok(())
            })?;
            if point.len() != 3 {
                bail!("a point needs three coordinates");
            }
            if mesh.vertex_count() >= u32::MAX as usize / 2 {
                bail!("too many points");
            }
            mesh.vertex([point[0], point[1], point[2]]);
            Ok(())
        })
    })();
    if let Err(e) = result {
        mesh.positions.truncate(base * 3);
        return Err(e);
    }
    Ok((base as u32, mesh.vertex_count() - base))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh_of(text: &str) -> (Mesh, usize, usize) {
        let data = text.as_bytes();
        let insts = p21::scan(data).unwrap();
        let by_id = insts.iter().enumerate().map(|(i, x)| (x.id, i)).collect();
        let faces = shape_faces(data, &insts, &by_id).unwrap();
        let (mesh, shown) = read(data, &insts, &by_id, &faces).unwrap();
        (mesh, shown, faces.len())
    }

    fn file(body: &str) -> String {
        format!("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n{body}\nENDSEC;\nEND-ISO-10303-21;\n")
    }

    #[test]
    fn triangles_strips_fans_and_pnindex() {
        let text = file(
            "#1=COORDINATES_LIST('',4,((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)));\n\
             #2=TRIANGULATED_FACE('',#1,4,(),$,(),((1,2,3),(1,3,4)));\n\
             #3=COMPLEX_TRIANGULATED_FACE('',#1,4,(),$,(4,3,2,1),((1,2,3,4)),());\n\
             #4=COMPLEX_TRIANGULATED_SURFACE_SET('',#1,4,(),(),(),((1,2,3,4),(1,1,2)));\n\
             #5=TRIANGULATED_SURFACE_SET('',#1,4,(),(),((2,3,4)));\n\
             #6=TESSELLATED_SHELL('',(#2,#3,#4,#5),$);",
        );
        let (mesh, shown, total) = mesh_of(&text);
        assert_eq!((shown, total), (4, 4));
        assert_eq!(mesh.vertex_count(), 4, "the coordinates are read once");
        // 2 + 2 (strip through pnindex) + 2 (fan; the degenerate fan is skipped) + 1
        assert_eq!(mesh.triangle_count(), 7);
        // the strip via pnindex: points 4,3,2,1 → (4 3 2), (2 3 1)
        assert_eq!(&mesh.indices[6..12], &[3, 2, 1, 1, 2, 0]);
        mesh.check().unwrap();
    }

    #[test]
    fn annotations_are_not_part_of_the_shape() {
        let text = file(
            "#1=COORDINATES_LIST('',3,((0.,0.,0.),(1.,0.,0.),(1.,1.,0.)));\n\
             #2=COMPLEX_TRIANGULATED_SURFACE_SET('note',#1,3,(),(),(),((1,2,3)));\n\
             #3=TESSELLATED_GEOMETRIC_SET('',(#2));\n\
             #4=TESSELLATED_ANNOTATION_OCCURRENCE('',(#9),#3);\n\
             #5=COORDINATES_LIST('',3,((0.,0.,5.),(1.,0.,5.),(1.,1.,5.)));\n\
             #6=TRIANGULATED_FACE('',#5,3,(),$,(),((1,2,3)));\n\
             #7=COMPLEX_TRIANGULATED_SURFACE_SET('arrow',#1,3,(),(),(),((1,2,3)));\n\
             #8=ANNOTATION_FILL_AREA_OCCURRENCE('',(#9),#7);",
        );
        let (mesh, shown, total) = mesh_of(&text);
        assert_eq!((shown, total), (1, 1));
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.vertex_count(), 3, "the annotation points are not read");
        assert!(mesh.positions.chunks(3).all(|p| p[2] == 5.0));
    }

    #[test]
    fn damaged_faces_are_skipped() {
        let text = file(
            "#1=COORDINATES_LIST('',3,((0.,0.,0.),(1.,0.,0.),(1.,1.,0.)));\n\
             #2=TRIANGULATED_FACE('',#1,3,(),$,(),((1,2,3)));\n\
             #3=TRIANGULATED_FACE('',#1,3,(),$,(),((1,2,9)));\n\
             #4=TRIANGULATED_FACE('',#1,3,(),$,(),((1,2,3),(-1,2,3)));\n\
             #5=TRIANGULATED_FACE('',#77,3,(),$,(),((1,2,3)));\n\
             #6=TRIANGULATED_FACE('',#1,3,(),$,(5),((1,1,1)));\n\
             #7=COORDINATES_LIST('',2,((0.,0.),(1.,0.,0.)));\n\
             #8=TRIANGULATED_FACE('',#7,3,(),$,(),((1,2,3)));\n\
             #9=TRIANGULATED_FACE('',#1,3,(),$,(),((1,2)));\n\
             #10=TRIANGULATED_FACE('',#1,3,(),$,(),'text');\n\
             #11=TRIANGULATED_FACE('',$,3,(),$,(),((1,2,3)));",
        );
        let (mesh, shown, total) = mesh_of(&text);
        assert_eq!((shown, total), (1, 9));
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.vertex_count(), 3, "the broken list adds no points");
        mesh.check().unwrap();
    }
}
