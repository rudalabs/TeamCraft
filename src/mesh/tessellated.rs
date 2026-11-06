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
use super::{check_triangles, Mesh, MAX_TRIANGLES};

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
    // a set used by many annotations is read once (no quadratic work)
    let mut sets = HashSet::new();
    for inst in insts {
        if !name_has(data, inst, &[b"ANNOTATION", b"OCCURRENCE"])? {
            continue;
        }
        for id in p21::references(data, inst)? {
            let Some((i, target)) = by_id.get(&id).and_then(|&i| Some((i, insts.get(i)?))) else {
                continue;
            };
            if is_face(target) {
                annotation.insert(i);
            } else if !sets.contains(&i) && name_has(data, target, &[b"GEOMETRIC_SET"])? {
                sets.insert(i);
                for inner in p21::references(data, target)? {
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
    let groups = groups(data, insts, faces)?;
    let mut bodies = HashSet::new();
    for &f in faces {
        let Some(inst) = insts.get(f) else { continue };
        let before = mesh.indices.len();
        match face(data, insts, by_id, inst, &mut mesh, &mut lists) {
            Ok(true) => {
                shown += 1;
                bodies.insert(groups.get(&f).copied().unwrap_or(f));
            }
            Ok(false) => {}
            // the model is too large – not a damaged face
            Err(e) if mesh.triangle_count() > MAX_TRIANGLES => return Err(e),
            Err(e) => {
                // a damaged face is left out, its triangles with it
                tracing::debug!(error = %e, "tessellated face skipped");
                mesh.indices.truncate(before);
            }
        }
    }
    mesh.parts = bodies.len().max(1) as u32;
    Ok((mesh, shown))
}

/// Face → the `TESSELLATED_SHELL` / `TESSELLATED_SOLID` it belongs to (a
/// body of the viewer; a face outside of them is a body of its own).
fn groups(data: &[u8], insts: &[Instance<'_>], faces: &[usize]) -> Result<HashMap<usize, usize>> {
    let face_of: HashMap<u64, usize> = faces
        .iter()
        .filter_map(|&f| Some((insts.get(f)?.id, f)))
        .collect();
    let mut out = HashMap::new();
    for (g, inst) in insts.iter().enumerate() {
        if inst.complex || !(inst.is("TESSELLATED_SHELL") || inst.is("TESSELLATED_SOLID")) {
            continue;
        }
        for id in p21::references(data, inst)? {
            if let Some(&f) = face_of.get(&id) {
                out.entry(f).or_insert(g);
            }
        }
    }
    Ok(out)
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
                .and_then(|&i| insts.get(i))
                .filter(|list| list.is("COORDINATES_LIST"))
                .map(|list| coordinates(data, list, mesh))
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
            let k = i.checked_sub(1).and_then(|k| usize::try_from(k).ok());
            match k.and_then(|k| pnindex.get(k)) {
                Some(&v) => v,
                None => bail!("index {i} out of range"),
            }
        };
        match point.checked_sub(1).map(usize::try_from) {
            Some(Ok(k)) if k < count => Ok(base + k as u32),
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
            for (j, w) in strip.windows(3).enumerate() {
                if let [x, y, c] = *w {
                    // every second triangle turns the other way
                    let (a, b) = if j % 2 == 0 { (x, y) } else { (y, x) };
                    add(mesh, a, b, c);
                }
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
            if let Some((&first, rest)) = strip.split_first() {
                for w in rest.windows(2) {
                    if let [b, c] = *w {
                        add(mesh, first, b, c);
                    }
                }
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
            let [a, b, c] = *strip.as_slice() else {
                bail!("a triangle needs three corners");
            };
            add(mesh, a, b, c);
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
            let [x, y, z] = *point.as_slice() else {
                bail!("a point needs three coordinates");
            };
            // the preview stores f32: 1E39 would become infinite and fail
            // the whole preview instead of this list
            if point.iter().any(|&v| !(v as f32).is_finite()) {
                bail!("coordinate out of range");
            }
            if mesh.vertex_count() >= u32::MAX as usize / 2 {
                bail!("too many points");
            }
            mesh.vertex([x, y, z]);
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

    #[test]
    fn extreme_indices_and_coordinates_skip_the_face() {
        let min = i64::MIN;
        let text = file(&format!(
            "#1=COORDINATES_LIST('',3,((0.,0.,0.),(1.,0.,0.),(1.,1.,0.)));\n\
             #2=TRIANGULATED_FACE('',#1,3,(),$,(),((1,2,3)));\n\
             #3=TRIANGULATED_FACE('',#1,3,(),$,(),(({min},2,3)));\n\
             #4=TRIANGULATED_FACE('',#1,3,(),$,(1,2,{min}),((1,2,3)));\n\
             #5=TRIANGULATED_FACE('',#1,3,(),$,(1,2,3),(({min},2,3)));\n\
             #6=COORDINATES_LIST('',3,((0.,0.,0.),(1.E39,0.,0.),(1.,1.,0.)));\n\
             #7=TRIANGULATED_FACE('',#6,3,(),$,(),((1,2,3)));"
        ));
        let (mesh, shown, total) = mesh_of(&text);
        assert_eq!((shown, total), (1, 5));
        mesh.check().unwrap();
    }

    #[test]
    fn bodies_are_shells_and_solids_not_coordinate_lists() {
        // one coordinate list per face, as exporters write it
        let mut body = String::new();
        for k in 0..4 {
            let (c, f) = (10 + 2 * k, 11 + 2 * k);
            body += &format!(
                "#{c}=COORDINATES_LIST('',3,((0.,0.,{k}.),(1.,0.,{k}.),(1.,1.,{k}.)));\n\
                 #{f}=TRIANGULATED_FACE('',#{c},3,(),$,(),((1,2,3)));\n"
            );
        }
        body += "#1=TESSELLATED_SOLID('',(#11,#13),$);\n#2=TESSELLATED_SHELL('',(#15),$);";
        let (mesh, shown, _) = mesh_of(&file(&body));
        assert_eq!(shown, 4);
        // the solid, the shell and the face #17 outside of both
        assert_eq!(mesh.parts, 3);
    }

    #[test]
    fn the_triangle_limit_fails_the_preview() {
        // not a partial preview with the "damaged" faces left out
        let n = super::MAX_TRIANGLES / 2 + 1;
        let tris = vec!["(1,2,3)"; n].join(",");
        let text = file(&format!(
            "#1=COORDINATES_LIST('',3,((0.,0.,0.),(1.,0.,0.),(1.,1.,0.)));\n\
             #2=TRIANGULATED_FACE('',#1,3,(),$,(),({tris}));\n\
             #3=TRIANGULATED_FACE('',#1,3,(),$,(),({tris}));"
        ));
        let data = text.as_bytes();
        let insts = p21::scan(data).unwrap();
        let by_id = insts.iter().enumerate().map(|(i, x)| (x.id, i)).collect();
        let faces = shape_faces(data, &insts, &by_id).unwrap();
        let err = read(data, &insts, &by_id, &faces).unwrap_err();
        assert!(
            err.to_string().contains("more than the preview shows"),
            "{err}"
        );
    }
}
