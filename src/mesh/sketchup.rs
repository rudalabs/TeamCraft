//! SketchUp models (`.skp`) for the 3D preview.
//!
//! SketchUp is a polygon modeller: its planar faces – an outer loop of edges
//! and holes – *are* the geometry, so the preview triangulates them
//! ([`super::earcut`]) and places the component definitions and groups
//! through their instances. Hidden faces and instances, everything on a
//! hidden layer (tag) and image definitions are left out. Coordinates are
//! inches, converted to millimetres.
//!
//! SketchUp 2021 and later write a ZIP archive behind a UTF-16 header; its
//! `model.dat` is a tree of tag–length–value records (u16 tag, u32 length).
//! The tags were taken from the format description of OpenSKP
//! (github.com/iamahsanmehmood/openskp, MIT licence, Copyright (c) 2024-2026
//! Ahsan Mehmood) and checked against its test files; see
//! `docs/src/research/sketchup-preview.md`. Files of SketchUp 3 to 2020 (one
//! MFC object stream with many version quirks) are read by OpenSKP's own Rust
//! port, vendored unchanged in `vendor/openskp` (MIT), and placed and
//! triangulated here the same way.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};

use super::earcut::earcut;
use super::stl::Welder;
use super::{check_triangles, Mesh};

/// `FF FE FF 0E`, then "SketchUp Model" in UTF-16.
const MAGIC: &[u8] = b"\xff\xfe\xff\x0e";
/// Largest `model.dat` that is read (the archive may claim any size).
const MAX_MODEL_BYTES: u64 = 512 * 1024 * 1024;
/// Records nest about a dozen levels in real files.
const MAX_TLV_DEPTH: usize = 64;
/// Components inside components.
const MAX_NESTING: usize = 32;
/// Instances placed in total (nested instancing multiplies).
const MAX_PLACEMENTS: usize = 2_000_000;
const INCH: f64 = 25.4;

/// A tag as the two bytes the format description writes ("C409").
const fn t(a: u8, b: u8) -> u16 {
    u16::from_le_bytes([a, b])
}

const F401: u16 = t(0xF4, 0x01); // model
const F601: u16 = t(0xF6, 0x01); // geometry of the model itself
const DEFINITION: u16 = t(0x7C, 0x15);
const DEF_KIND: u16 = t(0x83, 0x15); // 0 component, 1 group, 2 image
const LAYERS: u16 = t(0x99, 0x3A);
const LAYER: u16 = t(0x8C, 0x3C);
const LAYER_HIDDEN: u16 = t(0x8E, 0x3C);
const VERTEX: u16 = t(0xC4, 0x09);
const POSITION: u16 = t(0xC5, 0x09);
const EDGE: u16 = t(0xB8, 0x0B);
const EDGE_START: u16 = t(0xB9, 0x0B);
const EDGE_END: u16 = t(0xBA, 0x0B);
const FACE: u16 = t(0xAC, 0x0D);
const FACE_NORMAL: u16 = t(0xAD, 0x0D);
const FACE_LOOPS: u16 = t(0xAE, 0x0D);
const LOOP: u16 = t(0x94, 0x11);
const COEDGES: u16 = t(0x95, 0x11);
const COEDGE: u16 = t(0xA0, 0x0F);
const COEDGE_EDGE: u16 = t(0xA1, 0x0F);
const COEDGE_REVERSED: u16 = t(0xA2, 0x0F);
const INSTANCE: u16 = t(0x64, 0x19);
const TRANSFORM: u16 = t(0x66, 0x19);
const INSTANCE_DEF: u16 = t(0x67, 0x19);
const DRAWBASE: u16 = t(0xD0, 0x07);
const LAYER_REF: u16 = t(0xD2, 0x07);
const DISPLAY_FLAGS: u16 = t(0xD3, 0x07);
const ID_WRAPPER: u16 = t(0xDC, 0x05);
const ID: u16 = t(0xDE, 0x05);

/// Tags whose payload is again a list of records.
const CONTAINERS: [u16; 41] = [
    t(0xF4, 0x01),
    t(0xF7, 0x01),
    t(0xD4, 0x30),
    t(0xD5, 0x30),
    t(0xC8, 0x32),
    t(0x7C, 0x15),
    t(0x88, 0x13),
    t(0x89, 0x13),
    t(0x8A, 0x13),
    t(0x8B, 0x13),
    t(0x8C, 0x13),
    t(0x8D, 0x13),
    t(0x4C, 0x1D),
    t(0x64, 0x19),
    t(0xF9, 0x01),
    t(0x70, 0x17),
    t(0x71, 0x17),
    t(0xD0, 0x07),
    t(0xC4, 0x09),
    t(0x94, 0x11),
    t(0x95, 0x11),
    t(0x0F, 0x01),
    t(0x38, 0x4A),
    t(0xB8, 0x0B),
    t(0x97, 0x13),
    t(0x2C, 0x4C),
    t(0xAC, 0x0D),
    t(0xAE, 0x0D),
    t(0xF6, 0x01),
    t(0xF8, 0x01),
    t(0x98, 0x3A),
    t(0x99, 0x3A),
    t(0x8C, 0x3C),
    t(0x8D, 0x3C),
    t(0x90, 0x13),
    t(0x40, 0x1F),
    t(0x91, 0x13),
    t(0x92, 0x13),
    t(0x69, 0x42),
    t(0x6C, 0x42),
    t(0x68, 0x42),
];

fn is_container(tag: u16) -> bool {
    CONTAINERS.contains(&tag)
}

pub(crate) fn read(data: &[u8]) -> Result<Mesh> {
    if !data.starts_with(MAGIC) {
        bail!("Not a SketchUp file.");
    }
    let head = data.get(..4096).unwrap_or(data);
    let Some(zip_at) = find(head, b"PK\x03\x04") else {
        if find(data.get(..512).unwrap_or(data), b"CVersionMap").is_some() {
            return classic(data);
        }
        bail!("Not a SketchUp file (no model archive).");
    };
    let model = model_dat(data.get(zip_at..).unwrap_or_default())?;
    let scene = Scene::parse(&model)?;
    scene.mesh()
}

/// SketchUp 3 to 2020: one MFC object stream, read by the vendored OpenSKP
/// walker (`vendor/openskp`, MIT). A panic in it is caught and reported as
/// a damaged file, like every other reader error.
fn classic(data: &[u8]) -> Result<Mesh> {
    let version = major_version(data).map_or("2020 or older".to_string(), |v| match v {
        13..=20 => format!("20{v}"),
        v => v.to_string(),
    });
    let parsed = std::panic::catch_unwind(|| {
        openskp::parse_skp(data, &openskp::ParseOptions::new()).map_err(|e| e.to_string())
    });
    let model = match parsed {
        Ok(Ok(model)) => model,
        Ok(Err(e)) => bail!("This SketchUp {version} file cannot be read: {e}"),
        Err(_) => bail!("This SketchUp {version} file is damaged."),
    };
    Scene::from_openskp(&model).mesh()
}

/// An OpenSKP entity id as the key of [`Scene`].
fn key(id: openskp::EntityId) -> u64 {
    u64::from_le_bytes(id.to_le_bytes())
}

/// A definition of OpenSKP's model: faces with their loops resolved to
/// points, instances with layer ids from `layers` (OpenSKP names layers).
fn from_openskp_definition(
    d: &openskp::Definition,
    image: bool,
    layers: &HashMap<&str, u64>,
) -> Definition {
    let layer = |name: &str| layers.get(name).copied();
    let faces = d
        .faces
        .values()
        .filter_map(|f| {
            let loops: Option<Vec<Vec<[f64; 3]>>> = f
                .loops
                .iter()
                .map(|lp| {
                    lp.iter()
                        .map(|ce| {
                            let e = d.edges.get(&ce.edge_id)?;
                            let v = d.vertices.get(if ce.orientation >= 0 {
                                &e.v1_id
                            } else {
                                &e.v2_id
                            })?;
                            [v.x, v.y, v.z]
                                .iter()
                                .all(|c| c.is_finite())
                                .then_some([v.x, v.y, v.z])
                        })
                        .collect()
                })
                .collect();
            let loops = loops?;
            loops.first().is_some_and(|l| l.len() >= 3).then(|| Face {
                loops,
                normal: f.normal.unwrap_or([0.0; 3]),
                hidden: f.hidden,
                layer: layer(&f.layer),
            })
        })
        .collect();
    let instances = d
        .instances
        .iter()
        .filter_map(|i| {
            let mut matrix = [
                1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0,
            ];
            if i.matrix.len() >= 13 && i.matrix.iter().all(|v| v.is_finite()) {
                for (m, v) in matrix.iter_mut().zip(&i.matrix) {
                    *m = *v;
                }
            }
            Some(Instance {
                def: key(i.ref_idx?),
                matrix,
                hidden: i.hidden,
                layer: layer(&i.layer),
            })
        })
        .collect();
    Definition {
        image,
        faces,
        instances,
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The SketchUp version of the header (`{25.0.575}` → 25, `{3.0.1}` → 3).
fn major_version(data: &[u8]) -> Option<u32> {
    let text: String = data
        .get(4..data.len().min(200))?
        .chunks_exact(2)
        .filter_map(|c| match *c {
            [b, 0] if b.is_ascii() => Some(b as char),
            _ => None,
        })
        .collect();
    let start = text.find('{')? + 1;
    let digits: String = text
        .get(start..)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn model_dat(zip: &[u8]) -> Result<Vec<u8>> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(zip)).context("The SketchUp model archive is damaged.")?;
    let mut entry = archive
        .by_name("model.dat")
        .context("The SketchUp file contains no model.")?;
    if entry.size() > MAX_MODEL_BYTES {
        bail!("The SketchUp model is too large to preview.");
    }
    let mut out = Vec::new();
    entry
        .by_ref()
        .take(MAX_MODEL_BYTES + 1)
        .read_to_end(&mut out)
        .context("The SketchUp model archive is damaged.")?;
    if out.len() as u64 > MAX_MODEL_BYTES {
        bail!("The SketchUp model is too large to preview.");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// tag–length–value records
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rec<'a> {
    tag: u16,
    body: &'a [u8],
}

/// The records of one level; stops at the first one that runs past the end
/// (container payloads may end with bytes that are no records).
struct Records<'a> {
    data: &'a [u8],
    at: usize,
}

fn records(data: &[u8]) -> Records<'_> {
    Records { data, at: 0 }
}

impl<'a> Iterator for Records<'a> {
    type Item = Rec<'a>;

    fn next(&mut self) -> Option<Rec<'a>> {
        let tag = super::u16_le(self.data, self.at).ok()?;
        let len = usize::try_from(super::u32_le(self.data, self.at + 2).ok()?).ok()?;
        let start = self.at.checked_add(6)?;
        let end = start.checked_add(len)?;
        let body = self.data.get(start..end)?;
        self.at = end;
        Some(Rec { tag, body })
    }
}

fn child(body: &[u8], tag: u16) -> Option<&[u8]> {
    records(body).find(|r| r.tag == tag).map(|r| r.body)
}

/// A little-endian integer of 1 to 8 bytes.
fn int(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    let mut b = [0u8; 8];
    b.get_mut(..bytes.len())?.copy_from_slice(bytes);
    Some(u64::from_le_bytes(b))
}

fn f64s<const N: usize>(bytes: &[u8]) -> Option<[f64; N]> {
    let mut out = [0f64; N];
    for (k, v) in out.iter_mut().enumerate() {
        *v = f64::from_le_bytes(super::bytes_at(bytes, k * 8).ok()?);
        if !v.is_finite() {
            return None;
        }
    }
    Some(out)
}

/// The entity id: `DE05`, or `DC05` wrapping `DE05` – on this level first,
/// then depth-first in the containers below.
fn entity_id(body: &[u8], depth: usize) -> Option<u64> {
    for r in records(body) {
        if r.tag == ID {
            return int(r.body);
        }
        if r.tag == ID_WRAPPER && r.body.starts_with(&ID.to_le_bytes()) {
            let len = usize::try_from(super::u32_le(r.body, 2).ok()?).ok()?;
            return int(r.body.get(6..6usize.checked_add(len)?)?);
        }
    }
    if depth >= MAX_TLV_DEPTH {
        return None;
    }
    records(body)
        .filter(|r| is_container(r.tag))
        .find_map(|r| entity_id(r.body, depth + 1))
}

/// Hidden flag and layer id from the drawbase record `D007`.
fn drawbase(body: &[u8]) -> (bool, Option<u64>) {
    let Some(d) = child(body, DRAWBASE) else {
        return (false, None);
    };
    let hidden = child(d, DISPLAY_FLAGS)
        .and_then(|f| f.first())
        .is_some_and(|f| f & 1 == 1);
    (hidden, child(d, LAYER_REF).and_then(int))
}

// ---------------------------------------------------------------------------
// model
// ---------------------------------------------------------------------------

struct Face {
    /// Outer loop first, then the holes; inches.
    loops: Vec<Vec<[f64; 3]>>,
    normal: [f64; 3],
    hidden: bool,
    layer: Option<u64>,
}

struct Instance {
    def: u64,
    matrix: [f64; 13],
    hidden: bool,
    layer: Option<u64>,
}

#[derive(Default)]
struct Definition {
    image: bool,
    faces: Vec<Face>,
    instances: Vec<Instance>,
}

struct RawFace {
    normal: [f64; 3],
    /// (edge id, reversed) per coedge, one list per loop.
    loops: Vec<Vec<(u64, bool)>>,
    hidden: bool,
    layer: Option<u64>,
}

#[derive(Default)]
struct Raw {
    vertices: HashMap<u64, [f64; 3]>,
    edges: HashMap<u64, (u64, u64)>,
    faces: Vec<RawFace>,
    instances: Vec<Instance>,
}

impl Raw {
    fn collect(&mut self, body: &[u8], depth: usize) {
        for r in records(body) {
            match r.tag {
                VERTEX => {
                    let id = entity_id(r.body, depth + 1);
                    let p = child(r.body, POSITION).and_then(f64s::<3>);
                    if let (Some(id), Some(p)) = (id, p) {
                        self.vertices.insert(id, p);
                    }
                }
                EDGE => {
                    let id = entity_id(r.body, depth + 1);
                    let a = child(r.body, EDGE_START).and_then(int);
                    let b = child(r.body, EDGE_END).and_then(int);
                    if let (Some(id), Some(a), Some(b)) = (id, a, b) {
                        self.edges.insert(id, (a, b));
                    }
                }
                FACE => self.faces.push(raw_face(r.body)),
                INSTANCE => {
                    let def = child(r.body, INSTANCE_DEF).and_then(int);
                    let matrix = child(r.body, TRANSFORM).and_then(f64s::<13>);
                    if let (Some(def), Some(matrix)) = (def, matrix) {
                        let (hidden, layer) = drawbase(r.body);
                        self.instances.push(Instance {
                            def,
                            matrix,
                            hidden,
                            layer,
                        });
                    }
                }
                DEFINITION => {}
                tag if is_container(tag) && depth < MAX_TLV_DEPTH => {
                    self.collect(r.body, depth + 1)
                }
                _ => {}
            }
        }
    }

    /// Faces with their loops as points; faces referring to missing edges
    /// or vertices are dropped.
    fn into_definition(self, image: bool) -> Definition {
        let mut faces = Vec::with_capacity(self.faces.len());
        'faces: for f in self.faces {
            let mut loops = Vec::with_capacity(f.loops.len());
            for coedges in &f.loops {
                let mut points = Vec::with_capacity(coedges.len());
                for &(edge, reversed) in coedges {
                    let Some(&(a, b)) = self.edges.get(&edge) else {
                        continue 'faces;
                    };
                    let Some(&p) = self.vertices.get(if reversed { &b } else { &a }) else {
                        continue 'faces;
                    };
                    points.push(p);
                }
                loops.push(points);
            }
            if loops.first().is_some_and(|l| l.len() >= 3) {
                faces.push(Face {
                    loops,
                    normal: f.normal,
                    hidden: f.hidden,
                    layer: f.layer,
                });
            }
        }
        Definition {
            image,
            faces,
            instances: self.instances,
        }
    }
}

fn raw_face(body: &[u8]) -> RawFace {
    let normal = child(body, FACE_NORMAL)
        .and_then(f64s::<3>)
        .unwrap_or([0.0; 3]);
    let mut loops = Vec::new();
    for lp in child(body, FACE_LOOPS)
        .map(records)
        .into_iter()
        .flatten()
        .filter(|r| r.tag == LOOP)
    {
        let mut coedges = Vec::new();
        for list in records(lp.body).filter(|r| r.tag == COEDGES) {
            for ce in records(list.body).filter(|r| r.tag == COEDGE) {
                // a leaf whose payload is again records
                let edge = child(ce.body, COEDGE_EDGE).and_then(int);
                let reversed = child(ce.body, COEDGE_REVERSED).and_then(int);
                if let (Some(edge), Some(reversed)) = (edge, reversed) {
                    coedges.push((edge, reversed != 0));
                }
            }
        }
        loops.push(coedges);
    }
    let (hidden, layer) = drawbase(body);
    RawFace {
        normal,
        loops,
        hidden,
        layer,
    }
}

struct Scene {
    definitions: HashMap<u64, Definition>,
    root: Definition,
    hidden_layers: HashSet<u64>,
}

impl Scene {
    /// The scene of a model read by OpenSKP (SketchUp 3 to 2020).
    fn from_openskp(model: &openskp::SkpModel) -> Scene {
        let layers: HashMap<&str, u64> = model
            .layers
            .iter()
            .enumerate()
            .map(|(k, l)| (l.name.as_str(), k as u64))
            .collect();
        let hidden_layers = model
            .layers
            .iter()
            .filter(|l| l.hidden)
            .filter_map(|l| layers.get(l.name.as_str()).copied())
            .collect();
        Scene {
            definitions: model
                .definitions
                .iter()
                .map(|(id, d)| (key(*id), from_openskp_definition(d, d.is_image, &layers)))
                .collect(),
            root: from_openskp_definition(model.root(), false, &layers),
            hidden_layers,
        }
    }

    fn parse(model: &[u8]) -> Result<Scene> {
        let mut scene = Scene {
            definitions: HashMap::new(),
            root: Definition::default(),
            hidden_layers: HashSet::new(),
        };
        let mut top = records(model).peekable();
        let first = top.peek().copied();
        match first {
            Some(r) if r.tag == F401 => scene.walk(r.body, 1),
            Some(_) => scene.walk(model, 0),
            None => bail!("The SketchUp model is empty or damaged."),
        }
        Ok(scene)
    }

    fn walk(&mut self, body: &[u8], depth: usize) {
        for r in records(body) {
            match r.tag {
                DEFINITION => {
                    let Some(id) = entity_id(r.body, depth + 1) else {
                        continue;
                    };
                    let image = child(r.body, DEF_KIND).and_then(int) == Some(2);
                    let mut raw = Raw::default();
                    raw.collect(r.body, depth + 1);
                    self.definitions.insert(id, raw.into_definition(image));
                }
                LAYERS => {
                    for layer in records(r.body).filter(|l| l.tag == LAYER) {
                        let hidden = child(layer.body, LAYER_HIDDEN)
                            .and_then(|h| h.first().copied())
                            == Some(1);
                        if let (true, Some(id)) = (hidden, entity_id(layer.body, depth + 2)) {
                            self.hidden_layers.insert(id);
                        }
                    }
                }
                F601 => {
                    let mut raw = Raw::default();
                    raw.collect(r.body, depth + 1);
                    let mut root = raw.into_definition(false);
                    self.root.faces.append(&mut root.faces);
                    self.root.instances.append(&mut root.instances);
                }
                tag if is_container(tag) && depth < MAX_TLV_DEPTH => self.walk(r.body, depth + 1),
                _ => {}
            }
        }
    }

    fn hidden(&self, hidden: bool, layer: Option<u64>) -> bool {
        hidden || layer.is_some_and(|l| self.hidden_layers.contains(&l))
    }

    fn mesh(&self) -> Result<Mesh> {
        let mut out = Builder {
            scene: self,
            local: HashMap::new(),
            welder: Welder::new(1024),
            triangles: 0,
            placements: 0,
            parts: 0,
        };
        out.place(None, &IDENTITY, &mut Vec::new())?;
        let mut mesh = out.welder.mesh;
        if mesh.indices.is_empty() {
            bail!("The SketchUp model contains no visible faces.");
        }
        mesh.parts = out.parts.max(1);
        Ok(mesh)
    }
}

// ---------------------------------------------------------------------------
// placement and triangulation
// ---------------------------------------------------------------------------

/// Row-major 3 × 4: rotation/scale rows and translation.
type Affine = [[f64; 4]; 3];

const IDENTITY: Affine = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

/// SketchUp's 13 doubles: `p' = s · (R p + t)` with R's rows
/// (m0 m1 m2) (m3 m4 m5) (m6 m7 m8), t = (m9 m10 m11), s = m12.
fn affine(m: &[f64; 13]) -> Affine {
    let s = match m[12] {
        s if s.is_finite() && s != 0.0 => s,
        _ => 1.0,
    };
    [
        [m[0] * s, m[1] * s, m[2] * s, m[9] * s],
        [m[3] * s, m[4] * s, m[5] * s, m[10] * s],
        [m[6] * s, m[7] * s, m[8] * s, m[11] * s],
    ]
}

/// `outer ∘ inner`.
fn compose(outer: &Affine, inner: &Affine) -> Affine {
    let mut r = [[0.0; 4]; 3];
    for (row, o) in r.iter_mut().zip(outer) {
        for (c, v) in row.iter_mut().enumerate() {
            let col = |k: usize| inner.get(k).and_then(|i| i.get(c)).copied().unwrap_or(0.0);
            *v = o[0] * col(0) + o[1] * col(1) + o[2] * col(2) + if c == 3 { o[3] } else { 0.0 };
        }
    }
    r
}

fn apply(m: &Affine, p: [f64; 3]) -> [f64; 3] {
    m.map(|r| r[0] * p[0] + r[1] * p[1] + r[2] * p[2] + r[3])
}

fn det(m: &Affine) -> f64 {
    let [a, b, c] = m;
    a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
        + a[2] * (b[0] * c[1] - b[1] * c[0])
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = dot(a, a).sqrt();
    (l > 1e-300 && l.is_finite()).then(|| a.map(|c| c / l))
}

/// Newell's normal of a loop.
fn newell(points: &[[f64; 3]]) -> [f64; 3] {
    let mut n = [0.0; 3];
    for (k, a) in points.iter().enumerate() {
        let b = points.get(k + 1).or(points.first()).copied().unwrap_or(*a);
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

type Triangle = [[f64; 3]; 3];

/// Triangles of one face, wound counter-clockwise around its normal.
fn triangulate(face: &Face) -> Vec<Triangle> {
    let loops: Vec<&Vec<[f64; 3]>> = face.loops.iter().filter(|l| l.len() >= 3).collect();
    let Some(outer) = loops.first() else {
        return Vec::new();
    };
    let Some(n) = normalize(face.normal).or_else(|| normalize(newell(outer))) else {
        return Vec::new();
    };
    let u = normalize(cross(
        n,
        if n[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        },
    ))
    .unwrap_or([1.0, 0.0, 0.0]);
    let v = cross(n, u);
    let points: Vec<[f64; 3]> = loops.iter().flat_map(|l| l.iter().copied()).collect();
    let flat: Vec<[f64; 2]> = points.iter().map(|&p| [dot(p, u), dot(p, v)]).collect();
    let mut holes = Vec::new();
    let mut at = outer.len();
    for l in loops.iter().skip(1) {
        holes.push(at);
        at += l.len();
    }
    let rings: Vec<&[[f64; 2]]> = ring_slices(&flat, &holes);
    let index_triangles = if rings.len() > 2 && holes_overlap(rings.get(1..).unwrap_or_default()) {
        // Inner loops that overlap each other: clip the outer loop alone and
        // drop the triangles whose centre lies in an inner loop.
        earcut(flat.get(..outer.len()).unwrap_or_default(), &[])
            .into_iter()
            .filter(|t| {
                let c = t.iter().fold([0.0, 0.0], |c, &i| {
                    let p = flat.get(i).copied().unwrap_or_default();
                    [c[0] + p[0] / 3.0, c[1] + p[1] / 3.0]
                });
                !rings.iter().skip(1).any(|h| inside(c, h))
            })
            .collect()
    } else {
        earcut(&flat, &holes)
    };
    index_triangles
        .into_iter()
        .filter_map(|t| {
            let [a, b, c] = t.map(|i| points.get(i).copied());
            let (a, b, c) = (a?, b?, c?);
            Some(if dot(cross(sub(b, a), sub(c, a)), n) < 0.0 {
                [a, c, b]
            } else {
                [a, b, c]
            })
        })
        .collect()
}

/// The outer ring and the holes of `flat` (holes = start of each hole).
fn ring_slices<'a>(flat: &'a [[f64; 2]], holes: &[usize]) -> Vec<&'a [[f64; 2]]> {
    let mut starts = vec![0];
    starts.extend(holes.iter().copied());
    let mut ends: Vec<usize> = holes.to_vec();
    ends.push(flat.len());
    starts
        .into_iter()
        .zip(ends)
        .filter_map(|(s, e)| flat.get(s..e))
        .collect()
}

fn inside(p: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut odd = false;
    let mut j = ring.len().wrapping_sub(1);
    for (i, a) in ring.iter().enumerate() {
        if let Some(b) = ring.get(j) {
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
            {
                odd = !odd;
            }
        }
        j = i;
    }
    odd
}

fn bounds(ring: &[[f64; 2]]) -> [f64; 4] {
    ring.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |b, p| {
            [
                b[0].min(p[0]),
                b[1].min(p[1]),
                b[2].max(p[0]),
                b[3].max(p[1]),
            ]
        },
    )
}

/// Whether a vertex of one inner loop lies inside another inner loop.
fn holes_overlap(holes: &[&[[f64; 2]]]) -> bool {
    let boxes: Vec<[f64; 4]> = holes.iter().map(|h| bounds(h)).collect();
    for (i, (a, ba)) in holes.iter().zip(&boxes).enumerate() {
        for (j, (b, bb)) in holes.iter().zip(&boxes).enumerate() {
            let apart = ba[2] < bb[0] || bb[2] < ba[0] || ba[3] < bb[1] || bb[3] < ba[1];
            if i != j && !apart && a.iter().any(|&p| inside(p, b)) {
                return true;
            }
        }
    }
    false
}

struct Builder<'a> {
    scene: &'a Scene,
    /// Triangles of each definition (`None` = the model itself) in its own
    /// coordinates, computed once.
    local: HashMap<Option<u64>, Vec<Triangle>>,
    welder: Welder,
    triangles: usize,
    placements: usize,
    parts: u32,
}

fn visible_triangles(scene: &Scene, def: &Definition) -> Vec<Triangle> {
    def.faces
        .iter()
        .filter(|f| !scene.hidden(f.hidden, f.layer))
        .flat_map(triangulate)
        .collect()
}

impl Builder<'_> {
    /// Places definition `id` (`None` = the model itself) with `m`.
    fn place(&mut self, id: Option<u64>, m: &Affine, stack: &mut Vec<u64>) -> Result<()> {
        let scene = self.scene;
        let def = match id {
            None => &scene.root,
            Some(id) => match scene.definitions.get(&id) {
                Some(d) => d,
                None => return Ok(()),
            },
        };
        if def.image {
            return Ok(());
        }
        let triangles: &[Triangle] = self
            .local
            .entry(id)
            .or_insert_with(|| visible_triangles(scene, def));
        if !triangles.is_empty() {
            self.triangles += triangles.len();
            check_triangles(self.triangles)?;
            self.parts = self.parts.saturating_add(1);
            let mirrored = det(m) < 0.0;
            for tri in triangles {
                let [a, b, c] = tri.map(|p| apply(m, p).map(|v| (v * INCH) as f32));
                let order = if mirrored { [a, c, b] } else { [a, b, c] };
                if order.iter().flatten().all(|v| v.is_finite()) {
                    for p in order {
                        self.welder.push(p);
                    }
                }
            }
        }
        if stack.len() >= MAX_NESTING {
            return Ok(());
        }
        for inst in &def.instances {
            if scene.hidden(inst.hidden, inst.layer) || stack.contains(&inst.def) {
                continue;
            }
            self.placements += 1;
            if self.placements > MAX_PLACEMENTS {
                bail!("The SketchUp model places too many components to preview.");
            }
            let world = compose(m, &affine(&inst.matrix));
            stack.push(inst.def);
            self.place(Some(inst.def), &world, stack)?;
            stack.pop();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(tag: u16, body: &[u8]) -> Vec<u8> {
        let mut out = tag.to_le_bytes().to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    fn id(n: u8) -> Vec<u8> {
        let mut inner = ID.to_le_bytes().to_vec();
        inner.extend(1u32.to_le_bytes());
        inner.push(n);
        rec(ID_WRAPPER, &inner)
    }

    #[test]
    fn records_stop_at_a_record_running_past_the_end() {
        let mut data = rec(VERTEX, &[1, 2]);
        data.extend([0xC5, 0x09, 0xFF, 0xFF, 0xFF, 0x7F, 0]);
        let all: Vec<_> = records(&data).collect();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn entity_ids_are_found_directly_and_in_the_drawbase() {
        assert_eq!(entity_id(&id(7), 0), Some(7));
        let wrapped = rec(DRAWBASE, &id(9));
        assert_eq!(entity_id(&wrapped, 0), Some(9));
        assert_eq!(entity_id(&[], 0), None);
    }

    #[test]
    fn transforms_scale_rotate_and_translate() {
        // 90° about z, translation (1, 2, 3), homogeneous scale 2
        let m = [
            0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 2.0, 3.0, 2.0,
        ];
        let p = apply(&affine(&m), [1.0, 0.0, 0.0]);
        assert_eq!(p, [2.0, 6.0, 6.0]);
        let twice = compose(&affine(&m), &affine(&m));
        assert_eq!(apply(&twice, [0.0, 0.0, 0.0]), [-6.0, 8.0, 18.0]);
        assert!(det(&affine(&m)) > 0.0);
    }

    #[test]
    fn faces_with_holes_are_triangulated_around_their_normal() {
        let square = |s: f64, z: f64| vec![[-s, -s, z], [s, -s, z], [s, s, z], [-s, s, z]];
        let face = Face {
            loops: vec![square(2.0, 1.0), square(1.0, 1.0)],
            normal: [0.0, 0.0, -1.0],
            hidden: false,
            layer: None,
        };
        let tris = triangulate(&face);
        let area: f64 = tris
            .iter()
            .map(|[a, b, c]| {
                let n = cross(sub(*b, *a), sub(*c, *a));
                assert!(n[2] < 0.0, "wound against the normal");
                dot(n, n).sqrt() / 2.0
            })
            .sum();
        assert!((area - 12.0).abs() < 1e-9, "{area}");
    }

    #[test]
    fn overlapping_holes_leave_their_union_open() {
        let rect = |x0: f64, x1: f64| {
            vec![
                [x0, 0.0, 0.0],
                [x1, 0.0, 0.0],
                [x1, 2.0, 0.0],
                [x0, 2.0, 0.0],
            ]
        };
        let face = Face {
            loops: vec![
                vec![
                    [-1.0, -1.0, 0.0],
                    [10.0, -1.0, 0.0],
                    [10.0, 3.0, 0.0],
                    [-1.0, 3.0, 0.0],
                ],
                rect(0.0, 4.0),
                rect(2.0, 6.0),
            ],
            normal: [0.0, 0.0, 1.0],
            hidden: false,
            layer: None,
        };
        assert!(holes_overlap(&[
            &[[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [0.0, 2.0]],
            &[[2.0, 0.0], [6.0, 0.0], [6.0, 2.0], [2.0, 2.0]],
        ]));
        let tris = triangulate(&face);
        assert!(!tris.is_empty());
        for [a, b, c] in &tris {
            let centre = [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0];
            assert!(!(centre[0] > 0.0 && centre[0] < 6.0 && centre[1] > 0.0 && centre[1] < 2.0));
        }
    }

    #[test]
    fn a_damaged_classic_file_names_its_version() {
        let mut data = MAGIC.to_vec();
        for c in "SketchUp Model".encode_utf16() {
            data.extend(c.to_le_bytes());
        }
        data.extend(b"\xff\xfe\xff\x07");
        for c in "{17.0.1}".encode_utf16() {
            data.extend(c.to_le_bytes());
        }
        data.extend(b"CVersionMap");
        let e = read(&data).unwrap_err().to_string();
        assert!(e.contains("SketchUp 2017"), "{e}");
        assert!(read(b"nonsense").is_err());
        assert!(read(MAGIC).is_err());
    }
}
