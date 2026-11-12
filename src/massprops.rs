//! Mass properties of closed triangle meshes and their combination.
//!
//! * [`compute`] integrates a [`Mesh`] (the cached 3D preview of a file):
//!   volume, centroid and the inertia tensor about the centroid **per unit
//!   density, in model units** (Eberly, *Polyhedral Mass Properties
//!   (Revisited)*; the divergence theorem turns the volume integrals of
//!   1, x, y, z, x², y², z², xy, yz, zx into exact sums over the triangles).
//!   The integrals are taken relative to the centre of the bounding box, so a
//!   thin part far from the origin (a vehicle frame) keeps its digits.
//! * An edge check counts open, non-manifold and inconsistently oriented
//!   edges (vertices welded on a grid, like the shape signatures); a fully
//!   inverted mesh (negative volume) is flipped and reported.
//! * [`Geometry::body`] applies a density and a length unit and gives a
//!   [`Body`] in SI units; [`combine`] adds bodies by the parallel axis
//!   theorem and [`Body::inertia_about`] shifts the tensor to any point.
//!
//! Conventions: right-handed axes of the file (the common vehicle frame);
//! the inertia tensor is stored in tensor form – the off-diagonal entries
//! are the *negative* products of inertia (`ixy = −∫ xy dm`).
//!
//! Untrusted input: the server runs [`compute`] in the isolated preview
//! process (see `preview`); the function itself is bounded (one pass over the
//! triangles plus a sort of the edges, the edge check stops at
//! [`EDGE_CHECK_LIMIT`]) and returns errors, never panics.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;

/// Bump when the computation changes: cached results of another version are
/// ignored and computed again (the cache file name carries it).
pub const VERSION: u32 = 1;
/// Larger meshes get no edge check (their closedness is then unknown).
pub const EDGE_CHECK_LIMIT: usize = 2_000_000;
/// Vertices closer than this share of the bounding box diagonal are one for
/// the edge check (B-rep tessellations repeat edge vertices per face).
const WELD: f64 = 1e-6;
/// Coordinates beyond this are treated as broken (products must stay finite).
const MAX_COORD: f64 = 1e12;
/// A volume below this share of the bounding box volume is "no volume".
const MIN_RELATIVE_VOLUME: f64 = 1e-12;

pub type V3 = [f64; 3];
/// Symmetric 3×3 tensor `[xx, yy, zz, xy, yz, xz]`.
pub type Sym = [f64; 6];

/// The geometry of one mesh, per unit density in model units.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub version: u32,
    /// Enclosed volume (model units³), positive.
    pub volume: f64,
    /// Centroid (model units).
    pub centroid: V3,
    /// ∫ (|r|² E − r rᵀ) dV about the centroid (model units⁵), tensor form.
    pub inertia: Sym,
    /// Surface area (model units²).
    pub area: f64,
    /// Bounding box `[min x, min y, min z, max x, max y, max z]`.
    pub bounds: [f64; 6],
    pub triangles: u64,
    /// Every edge shared by exactly two consistently oriented triangles.
    /// `false` also when the mesh was too large to check.
    pub closed: bool,
    /// The edge check ran (at most [`EDGE_CHECK_LIMIT`] triangles).
    pub checked: bool,
    /// Edges used by one triangle only (holes).
    pub open_edges: u64,
    /// Edges used by three or more triangles.
    pub non_manifold_edges: u64,
    /// Edges whose two triangles run the same way (a flipped triangle).
    pub misoriented_edges: u64,
    /// Triangles without area (skipped).
    pub degenerate_triangles: u64,
    /// The mesh was inside out (negative volume) and was flipped.
    pub inverted_fixed: bool,
}

/// What the preview process writes: the geometry or why there is none.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Geometry(Geometry),
    Error(String),
}

impl Geometry {
    /// Plausible values (e.g. read back from the cache).
    pub fn is_valid(&self) -> bool {
        self.version == VERSION
            && self.volume.is_finite()
            && self.volume > 0.0
            && self.centroid.iter().all(|v| v.is_finite())
            && self.inertia.iter().all(|v| v.is_finite())
            && self.inertia[0] >= 0.0
            && self.inertia[1] >= 0.0
            && self.inertia[2] >= 0.0
            && self.area.is_finite()
    }

    /// Short reasons why the values may be wrong (empty = trustworthy).
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.checked {
            out.push(format!(
                "mesh too large for the closedness check ({} triangles)",
                self.triangles
            ));
        }
        if self.open_edges > 0 {
            out.push(format!(
                "open mesh: {} edge{} with one triangle only – the volume depends on the holes",
                self.open_edges,
                plural(self.open_edges)
            ));
        }
        if self.misoriented_edges > 0 {
            out.push(format!(
                "inconsistent orientation: {} edge{} between flipped triangles",
                self.misoriented_edges,
                plural(self.misoriented_edges)
            ));
        }
        if self.non_manifold_edges > 0 {
            out.push(format!(
                "{} non-manifold edge{} (bodies touching or duplicated faces)",
                self.non_manifold_edges,
                plural(self.non_manifold_edges)
            ));
        }
        if self.inverted_fixed {
            out.push("the mesh was inside out (all normals inwards) and was flipped".into());
        }
        out
    }

    /// The body at `density` (kg/m³) with `metres` per model unit.
    pub fn body(&self, density: f64, metres: f64) -> Body {
        let s3 = metres * metres * metres;
        let s5 = s3 * metres * metres;
        Body {
            mass: density * self.volume * s3,
            cg: self.centroid.map(|c| c * metres),
            inertia: self.inertia.map(|v| v * density * s5),
        }
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// A rigid body in SI units: mass (kg), centre of gravity (m) and inertia
/// tensor about the CG (kg·m², tensor form).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub mass: f64,
    pub cg: V3,
    pub inertia: Sym,
}

impl Body {
    /// A point mass.
    pub fn point(mass: f64, cg: V3) -> Body {
        Body {
            mass,
            cg,
            inertia: [0.0; 6],
        }
    }

    /// `factor` copies at the same place (a BOM quantity).
    pub fn times(&self, factor: f64) -> Body {
        Body {
            mass: self.mass * factor,
            cg: self.cg,
            inertia: self.inertia.map(|v| v * factor),
        }
    }

    /// The inertia tensor about `point` (parallel axis theorem).
    pub fn inertia_about(&self, point: V3) -> Sym {
        add(self.inertia, steiner(self.mass, sub(self.cg, point)))
    }

    /// All values finite.
    pub fn is_finite(&self) -> bool {
        self.mass.is_finite()
            && self.cg.iter().all(|v| v.is_finite())
            && self.inertia.iter().all(|v| v.is_finite())
    }
}

/// `m (|d|² E − d dᵀ)` in tensor form.
fn steiner(m: f64, d: V3) -> Sym {
    let [x, y, z] = d;
    [
        m * (y * y + z * z),
        m * (x * x + z * z),
        m * (x * x + y * y),
        -m * x * y,
        -m * y * z,
        -m * x * z,
    ]
}

fn add(a: Sym, b: Sym) -> Sym {
    [
        a[0] + b[0],
        a[1] + b[1],
        a[2] + b[2],
        a[3] + b[3],
        a[4] + b[4],
        a[5] + b[5],
    ]
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Adds bodies by the parallel axis theorem: total mass, common CG and the
/// tensor about it. Bodies without mass are skipped; no mass at all gives
/// the empty body (mass 0 at the origin).
pub fn combine<'a>(bodies: impl IntoIterator<Item = &'a Body> + Clone) -> Body {
    let mut mass = 0.0;
    let mut moment = [0.0; 3];
    for b in bodies.clone() {
        if b.mass > 0.0 {
            mass += b.mass;
            for (m, c) in moment.iter_mut().zip(b.cg) {
                *m += b.mass * c;
            }
        }
    }
    if mass <= 0.0 || !mass.is_finite() {
        return Body::default();
    }
    let cg = moment.map(|m| m / mass);
    let mut inertia = [0.0; 6];
    for b in bodies {
        if b.mass > 0.0 {
            inertia = add(inertia, b.inertia_about(cg));
        }
    }
    Body { mass, cg, inertia }
}

/// Principal moments of a tensor, largest first (cyclic Jacobi).
// The indices are the pairs (0, 1), (0, 2), (1, 2) of a 3 × 3 array.
#[allow(clippy::indexing_slicing)]
pub fn principal_moments(t: Sym) -> V3 {
    let mut a = [[t[0], t[3], t[5]], [t[3], t[1], t[4]], [t[5], t[4], t[2]]];
    for _ in 0..64 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        let scale = a[0][0].abs() + a[1][1].abs() + a[2][2].abs();
        if off.is_nan() || off <= 1e-15 * scale.max(f64::MIN_POSITIVE) {
            break;
        }
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            if a[p][q].abs() <= f64::MIN_POSITIVE {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = if theta == 0.0 {
                1.0
            } else {
                theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt())
            };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for row in a.iter_mut() {
                let (akp, akq) = (row[p], row[q]);
                row[p] = c * akp - s * akq;
                row[q] = s * akp + c * akq;
            }
            let (row_p, row_q) = (a[p], a[q]);
            a[p] = [0, 1, 2].map(|k| c * row_p[k] - s * row_q[k]);
            a[q] = [0, 1, 2].map(|k| s * row_p[k] + c * row_q[k]);
        }
    }
    let mut d = [a[0][0], a[1][1], a[2][2]];
    d.sort_by(|x, y| y.total_cmp(x));
    d
}

/// Eberly's per-triangle sub-expressions of one coordinate.
#[inline]
fn subexpressions(w0: f64, w1: f64, w2: f64) -> (f64, f64, f64, f64, f64, f64) {
    let temp0 = w0 + w1;
    let f1 = temp0 + w2;
    let temp1 = w0 * w0;
    let temp2 = temp1 + w1 * temp0;
    let f2 = temp2 + w2 * f1;
    let f3 = w0 * temp1 + w1 * temp2 + w2 * f2;
    let g0 = f2 + w0 * (f1 + w0);
    let g1 = f2 + w1 * (f1 + w1);
    let g2 = f2 + w2 * (f1 + w2);
    (f1, f2, f3, g0, g1, g2)
}

/// The ten volume integrals of a closed surface, before the final factors.
#[derive(Default)]
struct Integrals([f64; 10]);

impl Integrals {
    /// Adds the triangle `p0 p1 p2` (counter-clockwise seen from outside).
    fn add(&mut self, p0: V3, p1: V3, p2: V3) {
        let [x0, y0, z0] = p0;
        let [x1, y1, z1] = p1;
        let [x2, y2, z2] = p2;
        let (a1, b1, c1) = (x1 - x0, y1 - y0, z1 - z0);
        let (a2, b2, c2) = (x2 - x0, y2 - y0, z2 - z0);
        let d0 = b1 * c2 - b2 * c1;
        let d1 = a2 * c1 - a1 * c2;
        let d2 = a1 * b2 - a2 * b1;
        let (f1x, f2x, f3x, g0x, g1x, g2x) = subexpressions(x0, x1, x2);
        let (_, f2y, f3y, g0y, g1y, g2y) = subexpressions(y0, y1, y2);
        let (_, f2z, f3z, g0z, g1z, g2z) = subexpressions(z0, z1, z2);
        let s = &mut self.0;
        s[0] += d0 * f1x;
        s[1] += d0 * f2x;
        s[2] += d1 * f2y;
        s[3] += d2 * f2z;
        s[4] += d0 * f3x;
        s[5] += d1 * f3y;
        s[6] += d2 * f3z;
        s[7] += d0 * (y0 * g0x + y1 * g1x + y2 * g2x);
        s[8] += d1 * (z0 * g0y + z1 * g1y + z2 * g2y);
        s[9] += d2 * (x0 * g0z + x1 * g1z + x2 * g2z);
    }

    /// ∫1, ∫x, ∫y, ∫z, ∫x², ∫y², ∫z², ∫xy, ∫yz, ∫zx.
    fn finish(&self) -> [f64; 10] {
        const MULT: [f64; 10] = [
            1.0 / 6.0,
            1.0 / 24.0,
            1.0 / 24.0,
            1.0 / 24.0,
            1.0 / 60.0,
            1.0 / 60.0,
            1.0 / 60.0,
            1.0 / 120.0,
            1.0 / 120.0,
            1.0 / 120.0,
        ];
        let mut out = [0.0; 10];
        for ((o, v), m) in out.iter_mut().zip(self.0).zip(MULT) {
            *o = v * m;
        }
        out
    }
}

/// Volume, centroid and inertia about the centroid from finished integrals
/// taken relative to `origin`; `None` without volume. A negative volume
/// (inside out) is flipped first; the flag says so.
fn moments(i: [f64; 10], origin: V3, min_volume: f64) -> Option<(f64, V3, Sym, bool)> {
    let flip = i[0] < 0.0;
    let i = if flip { i.map(|v| -v) } else { i };
    let volume = i[0];
    if !(volume.is_finite() && volume > min_volume) {
        return None;
    }
    let c = [i[1] / volume, i[2] / volume, i[3] / volume];
    let (xx, yy, zz) = (i[4], i[5], i[6]);
    let inertia = [
        yy + zz - volume * (c[1] * c[1] + c[2] * c[2]),
        xx + zz - volume * (c[0] * c[0] + c[2] * c[2]),
        xx + yy - volume * (c[0] * c[0] + c[1] * c[1]),
        -(i[7] - volume * c[0] * c[1]),
        -(i[8] - volume * c[1] * c[2]),
        -(i[9] - volume * c[2] * c[0]),
    ];
    let centroid = [c[0] + origin[0], c[1] + origin[1], c[2] + origin[2]];
    Some((volume, centroid, inertia, flip))
}

/// Mass properties of a triangle mesh (per unit density, model units).
/// Point clouds, meshes without volume and broken coordinates are errors.
pub fn compute(mesh: &Mesh) -> Result<Geometry> {
    if mesh.is_point_cloud() {
        bail!("A point cloud has no volume.");
    }
    if mesh.positions.len() % 3 != 0 || mesh.indices.len() % 3 != 0 {
        bail!("Incomplete vertices or triangles.");
    }
    if let Some((shown, total)) = mesh.faces {
        bail!(
            "The 3D preview shows only {shown} of {total} faces, so the volume cannot be computed."
        );
    }
    let vertex = |i: u32| -> Option<V3> {
        let at = (i as usize).checked_mul(3)?;
        let &[x, y, z] = mesh.positions.get(at..at + 3)? else {
            return None;
        };
        let v = [f64::from(x), f64::from(y), f64::from(z)];
        v.iter()
            .all(|c| c.is_finite() && c.abs() < MAX_COORD)
            .then_some(v)
    };
    let triangle = |t: &[u32]| -> Option<[V3; 3]> {
        match t {
            [a, b, c] => Some([vertex(*a)?, vertex(*b)?, vertex(*c)?]),
            _ => None,
        }
    };

    // Pass 1: bounding box of the used vertices.
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut invalid = 0u64;
    for t in mesh.indices.chunks_exact(3) {
        let Some(tri) = triangle(t) else {
            invalid += 1;
            continue;
        };
        for p in tri {
            for ((lo, hi), v) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
                *lo = lo.min(v);
                *hi = hi.max(v);
            }
        }
    }
    if invalid > 0 {
        bail!("The mesh has {invalid} triangles with invalid vertices.");
    }
    if !lo.iter().chain(&hi).all(|v| v.is_finite()) {
        bail!("The mesh has no triangles.");
    }
    let origin = [
        0.5 * (lo[0] + hi[0]),
        0.5 * (lo[1] + hi[1]),
        0.5 * (lo[2] + hi[2]),
    ];
    let extent = sub(hi, lo);
    let diagonal = (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2]).sqrt();
    if !(diagonal.is_finite() && diagonal > 0.0) {
        bail!("The model has no extent.");
    }

    // Pass 2: the integrals relative to the box centre, and the area.
    let mut sums = Integrals::default();
    let mut area = 0.0;
    let mut degenerate = 0u64;
    for t in mesh.indices.chunks_exact(3) {
        let Some([a, b, c]) = triangle(t) else {
            continue;
        };
        let (a, b, c) = (sub(a, origin), sub(b, origin), sub(c, origin));
        let (u, v) = (sub(b, a), sub(c, a));
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let w = 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if w.is_nan() || w <= 0.0 {
            degenerate += 1;
            continue;
        }
        area += w;
        sums.add(a, b, c);
    }
    let box_volume = extent[0].max(diagonal * 1e-9)
        * extent[1].max(diagonal * 1e-9)
        * extent[2].max(diagonal * 1e-9);
    let Some((volume, centroid, inertia, inverted)) =
        moments(sums.finish(), origin, box_volume * MIN_RELATIVE_VOLUME)
    else {
        bail!("The mesh encloses no volume (a surface or an open sheet).");
    };
    if !(area.is_finite() && inertia.iter().all(|v| v.is_finite())) {
        bail!("The model coordinates are out of range.");
    }

    let triangles = mesh.triangle_count();
    let checked = triangles <= EDGE_CHECK_LIMIT;
    let edges = if checked {
        check_edges(mesh, origin, diagonal * WELD)
    } else {
        EdgeCheck::default()
    };
    let closed = checked
        && edges.open == 0
        && edges.non_manifold == 0
        && edges.misoriented == 0
        && edges.used > 0;
    Ok(Geometry {
        version: VERSION,
        volume,
        centroid,
        inertia,
        area,
        bounds: [lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]],
        triangles: triangles as u64,
        closed,
        checked,
        open_edges: edges.open,
        non_manifold_edges: edges.non_manifold,
        misoriented_edges: edges.misoriented,
        degenerate_triangles: degenerate,
        inverted_fixed: inverted,
    })
}

#[derive(Default)]
struct EdgeCheck {
    used: u64,
    open: u64,
    non_manifold: u64,
    misoriented: u64,
}

/// Counts the edges by how many triangles use them and in which direction.
fn check_edges(mesh: &Mesh, origin: V3, tolerance: f64) -> EdgeCheck {
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let mut welded: Vec<u32> = vec![u32::MAX; mesh.vertex_count()];
    let mut weld = |i: u32| -> Option<u32> {
        let slot = welded.get_mut(i as usize)?;
        if *slot == u32::MAX {
            let at = i as usize * 3;
            let &[x, y, z] = mesh.positions.get(at..at + 3)? else {
                return None;
            };
            let p = [x, y, z].map(f64::from);
            let key = sub(p, origin).map(|v| (v / tolerance).round().clamp(-9e15, 9e15) as i64);
            let next = ids.len() as u32;
            *slot = *ids.entry(key).or_insert(next);
        }
        Some(*slot)
    };
    let mut edges: Vec<(u64, i8)> = Vec::with_capacity(mesh.indices.len());
    for t in mesh.indices.chunks_exact(3) {
        let &[a, b, c] = t else { continue };
        let (Some(a), Some(b), Some(c)) = (weld(a), weld(b), weld(c)) else {
            continue;
        };
        if a == b || b == c || a == c {
            continue; // collapsed sliver
        }
        for (p, q) in [(a, b), (b, c), (c, a)] {
            let (lo, hi, dir) = if p < q { (p, q, 1) } else { (q, p, -1) };
            edges.push(((u64::from(lo) << 32) | u64::from(hi), dir));
        }
    }
    edges.sort_unstable_by_key(|e| e.0);
    let mut out = EdgeCheck::default();
    for run in edges.chunk_by(|a, b| a.0 == b.0) {
        out.used += 1;
        match run.len() {
            1 => out.open += 1,
            2 => {
                if run.iter().map(|e| i32::from(e.1)).sum::<i32>() != 0 {
                    out.misoriented += 1;
                }
            }
            _ => out.non_manifold += 1,
        }
    }
    out
}

/// Length units of mesh files and their size in metres.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LengthUnit {
    Mm,
    M,
    In,
}

impl LengthUnit {
    pub const ALL: [LengthUnit; 3] = [LengthUnit::Mm, LengthUnit::M, LengthUnit::In];

    pub fn as_str(self) -> &'static str {
        match self {
            LengthUnit::Mm => "mm",
            LengthUnit::M => "m",
            LengthUnit::In => "in",
        }
    }

    pub fn parse(s: &str) -> Option<LengthUnit> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mm" => Some(LengthUnit::Mm),
            "m" => Some(LengthUnit::M),
            "in" | "inch" => Some(LengthUnit::In),
            _ => None,
        }
    }

    /// Metres per unit.
    pub fn metres(self) -> f64 {
        match self {
            LengthUnit::Mm => 1e-3,
            LengthUnit::M => 1.0,
            LengthUnit::In => 0.0254,
        }
    }

    /// The unit a format means when nothing else is said: glTF is metres by
    /// specification, everything else is taken as millimetres.
    pub fn default_for(format: &str) -> LengthUnit {
        match format {
            "gltf" | "glb" => LengthUnit::M,
            _ => LengthUnit::Mm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mesh from triangles given as corner coordinates.
    fn soup(tris: &[[V3; 3]]) -> Mesh {
        let mut m = Mesh::default();
        for t in tris {
            for p in t {
                let i = m.vertex(*p);
                m.indices.push(i);
            }
        }
        m.parts = 1;
        m
    }

    /// An axis-aligned box from `lo` to `hi`, outward normals; welded.
    fn cuboid(lo: V3, hi: V3) -> Vec<[V3; 3]> {
        let c = |i: usize| -> V3 {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        // faces as quads (counter-clockwise from outside)
        let quads = [
            [0, 2, 3, 1], // z = lo
            [4, 5, 7, 6], // z = hi
            [0, 1, 5, 4], // y = lo
            [2, 6, 7, 3], // y = hi
            [0, 4, 6, 2], // x = lo
            [1, 3, 7, 5], // x = hi
        ];
        let mut out = Vec::new();
        for q in quads {
            out.push([c(q[0]), c(q[1]), c(q[2])]);
            out.push([c(q[0]), c(q[2]), c(q[3])]);
        }
        out
    }

    fn welded(tris: &[[V3; 3]]) -> Mesh {
        // weld exact duplicates like the STL reader does
        let mut m = Mesh::default();
        let mut ids: HashMap<[u64; 3], u32> = HashMap::new();
        for t in tris {
            for p in t {
                let key = p.map(|v| (v as f32).to_bits() as u64);
                let next = m.vertex_count() as u32;
                let i = *ids.entry(key).or_insert(next);
                if i == next {
                    m.vertex(*p);
                }
                m.indices.push(i);
            }
        }
        m.parts = 1;
        m
    }

    fn flipped(tris: &[[V3; 3]]) -> Vec<[V3; 3]> {
        tris.iter().map(|[a, b, c]| [*a, *c, *b]).collect()
    }

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * a.abs().max(b.abs()).max(1e-30)
    }

    fn assert_sym(got: Sym, want: Sym, rel: f64) {
        let scale = want.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-30);
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() <= rel * scale, "{got:?} vs {want:?}");
        }
    }

    /// Box inertia per unit density about its centre.
    fn box_inertia(a: f64, b: f64, c: f64) -> Sym {
        let v = a * b * c;
        [
            v * (b * b + c * c) / 12.0,
            v * (a * a + c * c) / 12.0,
            v * (a * a + b * b) / 12.0,
            0.0,
            0.0,
            0.0,
        ]
    }

    #[test]
    fn unit_cube() {
        let g = compute(&welded(&cuboid([0.0; 3], [1.0; 3]))).unwrap();
        assert!(close(g.volume, 1.0, 1e-12), "{}", g.volume);
        assert!(g.centroid.iter().all(|c| close(*c, 0.5, 1e-12)));
        assert_sym(g.inertia, box_inertia(1.0, 1.0, 1.0), 1e-12);
        assert!(g.closed && g.checked && !g.inverted_fixed);
        assert_eq!(
            (g.open_edges, g.misoriented_edges, g.non_manifold_edges),
            (0, 0, 0)
        );
        assert!(close(g.area, 6.0, 1e-12));
        assert!(g.warnings().is_empty());
        assert!(g.is_valid());
    }

    #[test]
    fn box_far_from_the_origin_keeps_its_digits() {
        // a 2 × 1 × 0.5 m slab, in mm, 3.2 km from the origin
        let lo = [3_200_000.0, -40_000.0, 12_000.0];
        let (a, b, c) = (2000.0, 1000.0, 500.0);
        let g = compute(&welded(&cuboid(lo, [lo[0] + a, lo[1] + b, lo[2] + c]))).unwrap();
        assert!(close(g.volume, a * b * c, 1e-9));
        let want = [lo[0] + a / 2.0, lo[1] + b / 2.0, lo[2] + c / 2.0];
        for (g, w) in g.centroid.iter().zip(want) {
            assert!((g - w).abs() < 1e-6, "{g} vs {w}");
        }
        // f32 positions are exact here; the tensor must be too
        assert_sym(g.inertia, box_inertia(a, b, c), 1e-9);
        // and in SI: aluminium 2700 kg/m³, mm
        let body = g.body(2700.0, 1e-3);
        assert!(close(body.mass, 2700.0, 1e-9));
        assert!(close(body.cg[0], 3201.0, 1e-12));
        assert!(close(body.inertia[0], 2700.0 * (1.0 + 0.25) / 12.0, 1e-9));
    }

    #[test]
    fn tetrahedron_matches_tonon() {
        // Tonon (2004), the worked example: vertices in metres, density 1
        let p = [
            [8.33220, -11.86875, 0.93355],
            [0.75523, 5.00000, 16.37072],
            [52.61236, 5.00000, -5.38580],
            [2.00000, 5.00000, 3.00000],
        ];
        // orient the faces outward: check the sign afterwards
        let faces = [[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2]];
        let mut tris: Vec<[V3; 3]> = faces.iter().map(|f| [p[f[0]], p[f[1]], p[f[2]]]).collect();
        let g = compute(&welded(&tris)).unwrap();
        if g.inverted_fixed {
            tris = flipped(&tris);
        }
        let g = compute(&welded(&tris)).unwrap();
        assert!(!g.inverted_fixed);
        // volume |det| / 6 and the published results (f32 input: ~1e-6)
        let d = |a: V3, b: V3| sub(a, b);
        let (u, v, w) = (d(p[1], p[0]), d(p[2], p[0]), d(p[3], p[0]));
        let det = u[0] * (v[1] * w[2] - v[2] * w[1]) - u[1] * (v[0] * w[2] - v[2] * w[0])
            + u[2] * (v[0] * w[1] - v[1] * w[0]);
        assert!(close(g.volume, det.abs() / 6.0, 1e-6), "{}", g.volume);
        let centroid = [15.92492, 0.78281, 3.72962];
        for (g, w) in g.centroid.iter().zip(centroid) {
            assert!((g - w).abs() < 1e-4, "{g} vs {w}");
        }
        // Tonon: a = 43520.33257, b = 194711.28938, c = 191168.76173,
        // a' = 4417.66150, b' = −46343.16662, c' = 11996.20119 (with his
        // products of inertia defined positive)
        // his tensor is [[a, −c', −b'], [−c', b, −a'], [−b', −a', c]], so
        // [xx, yy, zz, xy, yz, xz] = [a, b, c, −c', −a', −b']
        let want = [
            43520.33257,
            194711.28938,
            191168.76173,
            -11996.20119,
            -4417.66150,
            46343.16662,
        ];
        assert_sym(g.inertia, want, 1e-5);
    }

    #[test]
    fn rotated_box_has_the_same_principal_moments() {
        let (a, b, c) = (4.0, 2.0, 1.0);
        let base = cuboid([-a / 2.0, -b / 2.0, -c / 2.0], [a / 2.0, b / 2.0, c / 2.0]);
        // rotate about z by 30° and about x by 40°, then shift
        let (sz, cz) = 30f64.to_radians().sin_cos();
        let (sx, cx) = 40f64.to_radians().sin_cos();
        let rot = |p: V3| -> V3 {
            let q = [cz * p[0] - sz * p[1], sz * p[0] + cz * p[1], p[2]];
            [
                q[0] + 10.0,
                cx * q[1] - sx * q[2] - 5.0,
                sx * q[1] + cx * q[2] + 2.0,
            ]
        };
        let tris: Vec<[V3; 3]> = base.iter().map(|t| t.map(rot)).collect();
        let g = compute(&welded(&tris)).unwrap();
        assert!(close(g.volume, a * b * c, 1e-6));
        for (g, w) in g.centroid.iter().zip([10.0, -5.0, 2.0]) {
            assert!((g - w).abs() < 1e-5);
        }
        let want = box_inertia(a, b, c);
        let mut want = [want[0], want[1], want[2]];
        want.sort_by(|x, y| y.total_cmp(x));
        let got = principal_moments(g.inertia);
        for (g, w) in got.iter().zip(want) {
            assert!(close(*g, w, 1e-5), "{got:?} vs {want:?}");
        }
        // rotated: the products of inertia are not zero any more
        assert!(g.inertia[3].abs() > 0.1);
        // the trace is invariant
        let trace = g.inertia[0] + g.inertia[1] + g.inertia[2];
        assert!(close(trace, want.iter().sum(), 1e-6));
    }

    /// Icosphere of radius `r` with `level` subdivisions (outward).
    fn icosphere(r: f64, level: u32, centre: V3) -> Vec<[V3; 3]> {
        let t = (1.0 + 5f64.sqrt()) / 2.0;
        let norm = |p: V3| -> V3 {
            let l = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            [p[0] / l, p[1] / l, p[2] / l]
        };
        let v: Vec<V3> = [
            [-1.0, t, 0.0],
            [1.0, t, 0.0],
            [-1.0, -t, 0.0],
            [1.0, -t, 0.0],
            [0.0, -1.0, t],
            [0.0, 1.0, t],
            [0.0, -1.0, -t],
            [0.0, 1.0, -t],
            [t, 0.0, -1.0],
            [t, 0.0, 1.0],
            [-t, 0.0, -1.0],
            [-t, 0.0, 1.0],
        ]
        .into_iter()
        .map(norm)
        .collect();
        let f = [
            [0, 11, 5],
            [0, 5, 1],
            [0, 1, 7],
            [0, 7, 10],
            [0, 10, 11],
            [1, 5, 9],
            [5, 11, 4],
            [11, 10, 2],
            [10, 7, 6],
            [7, 1, 8],
            [3, 9, 4],
            [3, 4, 2],
            [3, 2, 6],
            [3, 6, 8],
            [3, 8, 9],
            [4, 9, 5],
            [2, 4, 11],
            [6, 2, 10],
            [8, 6, 7],
            [9, 8, 1],
        ];
        let mut tris: Vec<[V3; 3]> = f.iter().map(|f| [v[f[0]], v[f[1]], v[f[2]]]).collect();
        let mid = |a: V3, b: V3| {
            norm([
                (a[0] + b[0]) / 2.0,
                (a[1] + b[1]) / 2.0,
                (a[2] + b[2]) / 2.0,
            ])
        };
        for _ in 0..level {
            tris = tris
                .iter()
                .flat_map(|&[a, b, c]| {
                    let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
                    [[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]
                })
                .collect();
        }
        tris.iter()
            .map(|t| {
                t.map(|p| {
                    [
                        p[0] * r + centre[0],
                        p[1] * r + centre[1],
                        p[2] * r + centre[2],
                    ]
                })
            })
            .collect()
    }

    #[test]
    fn sphere_approximation_converges() {
        let r = 50.0;
        let exact_v = 4.0 / 3.0 * std::f64::consts::PI * r * r * r;
        let exact_i = 0.4 * exact_v * r * r;
        let mut last = (f64::INFINITY, f64::INFINITY);
        let mut errors = Vec::new();
        for level in 1..=5 {
            let g = compute(&welded(&icosphere(r, level, [120.0, 0.0, -30.0]))).unwrap();
            assert!(g.closed, "level {level}");
            let ev = (exact_v - g.volume) / exact_v;
            let ei = (exact_i - g.inertia[0]) / exact_i;
            // inscribed: always a little too small, and better each level
            assert!(ev > 0.0 && ev < last.0, "level {level}: {ev}");
            assert!(ei > 0.0 && ei < last.1, "level {level}: {ei}");
            // the tensor stays isotropic
            assert!(close(g.inertia[0], g.inertia[1], 1e-3));
            assert!(close(g.inertia[0], g.inertia[2], 1e-3));
            assert!(g.inertia[3].abs() < 1e-3 * g.inertia[0]);
            last = (ev, ei);
            errors.push((g.triangles, ev, ei));
        }
        // quadratic convergence: each level quarters the error (roughly)
        for w in errors.windows(2) {
            let ratio = w[0].1 / w[1].1;
            assert!((3.0..5.0).contains(&ratio), "{errors:?}");
        }
        // 1 280 triangles: 0.9 % / 1.4 %; 20 480 triangles: volume within
        // 0.06 %, inertia within 0.1 %
        let (_, ev, ei) = errors[2];
        assert!(ev < 1e-2 && ei < 1.5e-2, "{errors:?}");
        let (_, ev, ei) = errors[errors.len() - 1];
        assert!(ev < 6e-4 && ei < 1e-3, "{errors:?}");
        eprintln!("sphere convergence (triangles, volume error, inertia error): {errors:?}");
    }

    #[test]
    fn hollow_shell_from_an_outer_and_an_inverted_inner_surface() {
        let (r_out, r_in) = (10.0, 8.0);
        let mut tris = cuboid([-r_out; 3], [r_out; 3]);
        tris.extend(flipped(&cuboid([-r_in; 3], [r_in; 3])));
        let g = compute(&welded(&tris)).unwrap();
        let (a, b) = (2.0 * r_out, 2.0 * r_in);
        assert!(close(g.volume, a.powi(3) - b.powi(3), 1e-9));
        let want = box_inertia(a, a, a)[0] - box_inertia(b, b, b)[0];
        assert!(close(g.inertia[0], want, 1e-9));
        assert!(g.closed && !g.inverted_fixed);
        // a sphere shell too: (2/5) m (R⁵ − r⁵) / (R³ − r³)
        let mut s = icosphere(1.0, 4, [0.0; 3]);
        s.extend(flipped(&icosphere(0.9, 4, [0.0; 3])));
        let g = compute(&welded(&s)).unwrap();
        let v = 4.0 / 3.0 * std::f64::consts::PI * (1.0 - 0.9f64.powi(3));
        assert!(close(g.volume, v, 3e-3), "{} vs {v}", g.volume);
    }

    #[test]
    fn inverted_mesh_is_flipped_and_reported() {
        let tris = flipped(&cuboid([0.0; 3], [2.0, 1.0, 1.0]));
        let g = compute(&welded(&tris)).unwrap();
        assert!(g.inverted_fixed);
        assert!(close(g.volume, 2.0, 1e-12));
        assert_sym(g.inertia, box_inertia(2.0, 1.0, 1.0), 1e-12);
        assert!(g.closed);
        assert!(g.warnings().iter().any(|w| w.contains("inside out")));
    }

    #[test]
    fn open_and_misoriented_meshes_are_flagged() {
        let mut tris = cuboid([0.0; 3], [1.0; 3]);
        tris.pop(); // one triangle missing: three open edges
        let g = compute(&welded(&tris)).unwrap();
        assert!(!g.closed);
        assert_eq!(g.open_edges, 3);
        assert!(g.warnings().iter().any(|w| w.contains("open mesh")));

        let mut tris = cuboid([0.0; 3], [1.0; 3]);
        tris[0] = [tris[0][0], tris[0][2], tris[0][1]];
        let g = compute(&welded(&tris)).unwrap();
        assert!(!g.closed);
        assert_eq!(g.misoriented_edges, 3);
        assert!(g.warnings().iter().any(|w| w.contains("orientation")));
    }

    #[test]
    fn unwelded_soup_is_welded_for_the_check() {
        // per-face vertices (like a B-rep tessellation) are still closed
        let g = compute(&soup(&cuboid([0.0; 3], [1.0; 3]))).unwrap();
        assert!(g.closed, "{g:?}");
    }

    #[test]
    fn two_bodies_touching_on_an_edge_are_non_manifold_but_correct() {
        let mut tris = cuboid([0.0; 3], [1.0; 3]);
        tris.extend(cuboid([1.0, 1.0, 0.0], [2.0, 2.0, 1.0]));
        let g = compute(&welded(&tris)).unwrap();
        assert!(close(g.volume, 2.0, 1e-12));
        assert!(g.non_manifold_edges > 0 && !g.closed);
    }

    #[test]
    fn degenerate_input_is_an_error_not_a_panic() {
        // empty, a point cloud, a flat sheet, NaN, huge, broken indices
        assert!(compute(&Mesh::default()).is_err());
        let mut cloud = Mesh::default();
        cloud.vertex([1.0, 2.0, 3.0]);
        assert!(compute(&cloud).is_err());
        let sheet = welded(&[[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]);
        assert!(compute(&sheet)
            .unwrap_err()
            .to_string()
            .contains("no volume"));
        let mut nan = welded(&cuboid([0.0; 3], [1.0; 3]));
        nan.positions[0] = f32::NAN;
        assert!(compute(&nan).is_err());
        let mut huge = welded(&cuboid([0.0; 3], [1.0; 3]));
        huge.positions[0] = 1e30;
        assert!(compute(&huge).is_err());
        let mut bad = welded(&cuboid([0.0; 3], [1.0; 3]));
        bad.indices[0] = 9999;
        assert!(compute(&bad).is_err());
        let mut odd = welded(&cuboid([0.0; 3], [1.0; 3]));
        odd.indices.pop();
        assert!(compute(&odd).is_err());
        // all triangles collapsed
        let line = welded(&[[[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]]);
        assert!(compute(&line).is_err());
        // a partial STEP preview
        let mut partial = welded(&cuboid([0.0; 3], [1.0; 3]));
        partial.faces = Some((5, 6));
        assert!(compute(&partial)
            .unwrap_err()
            .to_string()
            .contains("5 of 6"));
    }

    #[test]
    fn units_scale_mass_cg_and_inertia() {
        let g = compute(&welded(&cuboid([0.0; 3], [1.0; 3]))).unwrap();
        let mm = g.body(1000.0, LengthUnit::Mm.metres());
        let inch = g.body(1000.0, LengthUnit::In.metres());
        assert!(close(mm.mass, 1e-6, 1e-12));
        assert!(close(inch.mass, 1000.0 * 0.0254f64.powi(3), 1e-12));
        assert!(close(inch.cg[0], 0.0127, 1e-12));
        assert!(close(
            inch.inertia[0] / mm.inertia[0],
            25.4f64.powi(5),
            1e-9
        ));
        assert_eq!(LengthUnit::parse(" IN "), Some(LengthUnit::In));
        assert_eq!(LengthUnit::parse("ft"), None);
        assert_eq!(LengthUnit::default_for("glb"), LengthUnit::M);
        assert_eq!(LengthUnit::default_for("stl"), LengthUnit::Mm);
        for u in LengthUnit::ALL {
            assert_eq!(LengthUnit::parse(u.as_str()), Some(u));
        }
    }

    #[test]
    fn composite_by_parallel_axis_equals_the_merged_mesh() {
        // a cube and an offset box, computed apart and combined, against
        // one mesh with both
        let a = cuboid([0.0; 3], [100.0; 3]);
        let b = cuboid([300.0, 0.0, 0.0], [500.0, 50.0, 20.0]);
        let ga = compute(&welded(&a)).unwrap();
        let gb = compute(&welded(&b)).unwrap();
        let mut both = a.clone();
        both.extend(b.iter().copied());
        let gab = compute(&welded(&both)).unwrap();
        let (ba, bb, bab) = (
            ga.body(2700.0, 1e-3),
            gb.body(7850.0, 1e-3),
            gab.body(1.0, 1e-3),
        );
        let total = combine([&ba, &bb]);
        assert!(close(total.mass, 2700.0 * 1e-3 + 7850.0 * 2e-4, 1e-9));
        // same density: identical to the merged mesh
        let same = combine([&ga.body(1.0, 1e-3), &gb.body(1.0, 1e-3)]);
        assert!(close(same.mass, bab.mass, 1e-12));
        for (x, y) in same.cg.iter().zip(bab.cg) {
            assert!((x - y).abs() < 1e-12);
        }
        assert_sym(same.inertia, bab.inertia, 1e-9);
        // the CG lies on the line between the two
        let w = 2.7 / (2.7 + 1.57);
        assert!(close(total.cg[0], w * 0.05 + (1.0 - w) * 0.4, 1e-9));
        // about a reference point: parallel axis again
        let about = total.inertia_about([0.0; 3]);
        let direct = add(ba.inertia_about([0.0; 3]), bb.inertia_about([0.0; 3]));
        assert_sym(about, direct, 1e-12);
    }

    #[test]
    fn combine_handles_empty_and_quantities() {
        assert_eq!(combine([] as [&Body; 0]), Body::default());
        let p = Body::point(0.0, [1.0, 2.0, 3.0]);
        assert_eq!(combine([&p]), Body::default());
        // four point masses on a ring: CG on the axis, Izz = m r²
        let ring: Vec<Body> = (0..4)
            .map(|k| {
                let a = k as f64 * std::f64::consts::FRAC_PI_2;
                Body::point(0.25, [a.cos(), a.sin(), 2.0])
            })
            .collect();
        let total = combine(ring.iter());
        assert!(close(total.mass, 1.0, 1e-12));
        assert!(total.cg[0].abs() < 1e-12 && close(total.cg[2], 2.0, 1e-12));
        assert!(close(total.inertia[2], 1.0, 1e-12));
        assert!(close(total.inertia[0], 0.5, 1e-12));
        // the same mass stacked four times at one point: no ring inertia
        let stacked = ring[0].times(4.0);
        assert!(close(stacked.mass, 1.0, 1e-12));
        assert_eq!(stacked.inertia, [0.0; 6]);
    }

    #[test]
    fn outcome_round_trips_as_json() {
        let g = compute(&welded(&cuboid([0.0; 3], [1.0; 3]))).unwrap();
        let json = serde_json::to_string(&Outcome::Geometry(g.clone())).unwrap();
        match serde_json::from_str::<Outcome>(&json).unwrap() {
            Outcome::Geometry(back) => assert_eq!(back, g),
            Outcome::Error(e) => panic!("{e}"),
        }
        let mut stale = g;
        stale.version = VERSION + 1;
        assert!(!stale.is_valid());
    }
}
