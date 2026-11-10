//! 3MF and AMF: XML, zipped or plain, with components and build transforms.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{anyhow, bail, Context, Result};

use super::*;

type Affine = [f64; 12];
const IDENTITY: Affine = [1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0.];

/// 3MF transforms are row vectors: `p' = p · M` with the translation last.
fn parse_transform(s: Option<&str>) -> Result<Affine> {
    let Some(s) = s else { return Ok(IDENTITY) };
    let values: Vec<f64> = s
        .split_ascii_whitespace()
        .map(float)
        .collect::<Result<_>>()?;
    values
        .try_into()
        .map_err(|_| anyhow!("3MF transform with the wrong number of values"))
}

fn apply(m: &Affine, p: [f64; 3]) -> [f64; 3] {
    [
        p[0] * m[0] + p[1] * m[3] + p[2] * m[6] + m[9],
        p[0] * m[1] + p[1] * m[4] + p[2] * m[7] + m[10],
        p[0] * m[2] + p[1] * m[5] + p[2] * m[8] + m[11],
    ]
}

/// `a` first, then `b`.
// Fixed-size arrays: r < 4, c < 3 and k < 3 keep every index below 12.
#[allow(clippy::indexing_slicing)]
fn compose(a: &Affine, b: &Affine) -> Affine {
    let mut m = [0.0; 12];
    for r in 0..4 {
        for c in 0..3 {
            let mut v = if r == 3 { b[9 + c] } else { 0.0 };
            for k in 0..3 {
                v += a[r * 3 + k] * b[k * 3 + c];
            }
            m[r * 3 + c] = v;
        }
    }
    m
}

fn zip_entry(data: &[u8], wanted: impl Fn(&str) -> bool) -> Result<Option<Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).context("not a ZIP package")?;
    let names: Vec<String> = archive
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect();
    let Some(name) = names.into_iter().find(|n| wanted(n.as_str())) else {
        return Ok(None);
    };
    let mut entry = archive.by_name(&name)?;
    if entry.size() > 2 * 1024 * 1024 * 1024 {
        bail!("{name} is too large to preview");
    }
    let mut out = Vec::with_capacity(entry.size() as usize);
    entry
        .by_ref()
        .take(2 * 1024 * 1024 * 1024)
        .read_to_end(&mut out)?;
    Ok(Some(out))
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

#[derive(Default)]
struct ThreeMfObject {
    mesh: Mesh,
    components: Vec<(String, Affine)>,
}

pub(super) fn read_3mf(data: &[u8]) -> Result<Mesh> {
    // The start part is named in _rels/.rels; "3D/3dmodel.model" by convention.
    let rels = zip_entry(data, |n| n.eq_ignore_ascii_case("_rels/.rels"))?;
    let target = rels
        .as_deref()
        .and_then(|r| {
            let r = String::from_utf8_lossy(r).into_owned();
            let at = r.find("3dmodel")?;
            let start = r.get(..at)?.rfind("Target=\"")? + 8;
            let end = start + r.get(start..)?.find('"')?;
            Some(r.get(start..end)?.trim_start_matches('/').to_string())
        })
        .unwrap_or_else(|| "3D/3dmodel.model".to_string());
    let xml = zip_entry(data, |n| n.eq_ignore_ascii_case(&target))?
        .or(zip_entry(data, |n| {
            n.to_ascii_lowercase().ends_with(".model")
        })?)
        .context("the 3MF package contains no 3D model")?;
    three_mf_model(&xml)
}

fn three_mf_model(xml: &[u8]) -> Result<Mesh> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut objects: HashMap<String, ThreeMfObject> = HashMap::new();
    let mut items: Vec<(String, Affine)> = Vec::new();
    let mut current: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .context("invalid 3MF XML")?;
        match &event {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "object" => {
                    let id = attr(e, "id").unwrap_or_default();
                    objects.entry(id.clone()).or_default();
                    current = Some(id);
                }
                "vertex" => {
                    if let Some(obj) = current.as_ref().and_then(|id| objects.get_mut(id)) {
                        let c = |n: &str| float(attr(e, n).as_deref().unwrap_or("0"));
                        obj.mesh.vertex([c("x")?, c("y")?, c("z")?]);
                    }
                }
                "triangle" => {
                    if let Some(obj) = current.as_ref().and_then(|id| objects.get_mut(id)) {
                        let n = obj.mesh.vertex_count() as u32;
                        let mut t = [0u32; 3];
                        for (slot, name) in t.iter_mut().zip(["v1", "v2", "v3"]) {
                            *slot = attr(e, name)
                                .and_then(|v| v.parse().ok())
                                .filter(|v| *v < n)
                                .context("3MF triangle refers to a missing vertex")?;
                        }
                        obj.mesh.indices.extend(t);
                        check_triangles(obj.mesh.triangle_count())?;
                    }
                }
                "component" => {
                    if let Some(obj) = current.as_ref().and_then(|id| objects.get_mut(id)) {
                        let target = attr(e, "objectid").unwrap_or_default();
                        obj.components
                            .push((target, parse_transform(attr(e, "transform").as_deref())?));
                    }
                }
                "item" => {
                    let target = attr(e, "objectid").unwrap_or_default();
                    items.push((target, parse_transform(attr(e, "transform").as_deref())?));
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "object" => current = None,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if items.is_empty() {
        // no build section: show every object that has geometry
        let mut ids: Vec<&String> = objects.keys().collect();
        ids.sort();
        items = ids.into_iter().map(|id| (id.clone(), IDENTITY)).collect();
    }
    let mut out = Mesh::default();
    for (id, transform) in &items {
        instantiate(&objects, id, transform, 0, &mut out)?;
    }
    Ok(out)
}

fn instantiate(
    objects: &HashMap<String, ThreeMfObject>,
    id: &str,
    transform: &Affine,
    depth: usize,
    out: &mut Mesh,
) -> Result<()> {
    if depth > 16 {
        bail!("3MF components nest too deeply (cycle?)");
    }
    let Some(obj) = objects.get(id) else {
        return Ok(()); // object in another model part (production extension)
    };
    if !obj.mesh.positions.is_empty() {
        let mut part = obj.mesh.clone();
        for p in part.positions.chunks_exact_mut(3) {
            if let [x, y, z] = p {
                let [qx, qy, qz] = apply(transform, [*x as f64, *y as f64, *z as f64]);
                (*x, *y, *z) = (qx as f32, qy as f32, qz as f32);
            }
        }
        part.parts = 1;
        out.append(part);
        check_triangles(out.triangle_count())?;
    }
    for (child, local) in &obj.components {
        instantiate(objects, child, &compose(local, transform), depth + 1, out)?;
    }
    Ok(())
}

pub(super) fn read_amf(data: &[u8]) -> Result<Mesh> {
    use quick_xml::events::Event;
    let unzipped;
    let xml = if data.starts_with(b"PK") {
        unzipped = zip_entry(data, |_| true)?.context("empty AMF archive")?;
        &unzipped[..]
    } else {
        data
    };
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut out = Mesh::default();
    let mut object = Mesh::default();
    let mut coord = [0f64; 3];
    let mut tri = [0u32; 3];
    let mut field: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .context("invalid AMF XML")?
        {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_owned();
                if name == "object" {
                    object = Mesh::default();
                }
                field = Some(name);
            }
            Event::Text(t) => {
                let value = t.xml_content(quick_xml::XmlVersion::Implicit1_0);
                let value = value.trim();
                match field.as_deref() {
                    Some("x") => coord[0] = float(value)?,
                    Some("y") => coord[1] = float(value)?,
                    Some("z") => coord[2] = float(value)?,
                    Some("v1") => tri[0] = value.parse().context("AMF triangle")?,
                    Some("v2") => tri[1] = value.parse().context("AMF triangle")?,
                    Some("v3") => tri[2] = value.parse().context("AMF triangle")?,
                    _ => {}
                }
            }
            Event::End(e) => {
                match e.local_name().as_ref() {
                    "coordinates" => {
                        object.vertex(coord);
                    }
                    "triangle" => {
                        let n = object.vertex_count() as u32;
                        if tri.iter().any(|i| *i >= n) {
                            bail!("AMF triangle refers to a missing vertex");
                        }
                        object.indices.extend(tri);
                        check_triangles(out.triangle_count() + object.triangle_count())?;
                    }
                    "object" => {
                        object.parts = 1;
                        out.append(std::mem::take(&mut object));
                    }
                    _ => {}
                }
                field = None;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, content) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn three_mf_with_components_and_build_transforms() {
        let model = r#"<?xml version="1.0"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model"><mesh>
   <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
   <triangles><triangle v1="0" v2="1" v3="2"/></triangles>
  </mesh></object>
  <object id="2" type="model"><components>
   <component objectid="1"/><component objectid="1" transform="1 0 0 0 1 0 0 0 1 10 0 0"/>
  </components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 0 5"/></build>
</model>"#;
        let package = zip_of(&[
            (
                "_rels/.rels",
                r#"<Relationships><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#,
            ),
            ("3D/3dmodel.model", model),
        ]);
        let mesh = crate::mesh::read("3mf", &package).unwrap();
        assert_eq!((mesh.triangle_count(), mesh.parts), (2, 2));
        let b = mesh.bounds();
        assert_eq!((b[0], b[3], b[2], b[5]), (0.0, 11.0, 5.0, 5.0));
        assert!(crate::mesh::read("3mf", b"PK not really").is_err());
    }

    #[test]
    fn amf_plain_and_zipped() {
        let amf = "<amf><object id=\"0\"><mesh><vertices>\
<vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex>\
<vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex>\
<vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex>\
</vertices><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object></amf>";
        assert_eq!(
            crate::mesh::read("amf", amf.as_bytes())
                .unwrap()
                .triangle_count(),
            1
        );
        let zipped = zip_of(&[("model.amf", amf)]);
        assert_eq!(
            crate::mesh::read("amf", &zipped).unwrap().triangle_count(),
            1
        );
        assert!(crate::mesh::read("amf", b"<amf><object><mesh><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object></amf>").is_err());
    }

    #[test]
    fn transforms_compose_like_3mf_matrices() {
        let t = parse_transform(Some("1 0 0 0 1 0 0 0 1 10 20 30")).unwrap();
        assert_eq!(apply(&t, [1.0, 2.0, 3.0]), [11.0, 22.0, 33.0]);
        let identity = parse_transform(None).unwrap();
        assert_eq!(apply(&identity, [1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]);
        let scale = parse_transform(Some("2 0 0 0 2 0 0 0 2 0 0 0")).unwrap();
        // row vectors as in 3MF: compose(a, b) applies a first, then b
        let translate_then_scale = compose(&t, &scale);
        assert_eq!(
            apply(&translate_then_scale, [1.0, 1.0, 1.0]),
            [22.0, 42.0, 62.0]
        );
        let scale_then_translate = compose(&scale, &t);
        assert_eq!(
            apply(&scale_then_translate, [1.0, 1.0, 1.0]),
            [12.0, 22.0, 32.0]
        );
        assert!(parse_transform(Some("1 0 0")).is_err(), "too few numbers");
        assert!(parse_transform(Some("1 0 0 0 1 0 0 0 1 x 0 0")).is_err());
    }
}
