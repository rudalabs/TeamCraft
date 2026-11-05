//! Shape signatures for the similar part search – cheap, explainable, no ML.
//!
//! A [`Signature`] summarises the 3D preview mesh of a file (see `mesh`):
//!
//! * **principal extents** – the edge lengths of an oriented bounding box,
//!   largest first. The box starts from the principal axes of the surface
//!   (area-weighted covariance) and is then tightened plane by plane with
//!   the minimum-area rectangle of the projected convex hull, so it neither
//!   depends on how the part lies in the file nor on the tessellation;
//! * **surface area** and **enclosed volume** (sum of signed tetrahedra) –
//!   the volume is only meaningful when every edge is shared by exactly two
//!   consistently oriented triangles (`closed`);
//! * a **D2 shape distribution** (Osada et al. 2002): the histogram of the
//!   distances between random point pairs on the surface, divided by the
//!   diagonal of the box – a fingerprint that ignores position, rotation and
//!   scale. The points are drawn with a fixed seed, so a file always gets
//!   the same signature;
//! * the triangle count.
//!
//! [`compare`] ranks two signatures (see there for the formula) and
//! [`explain`] says why in one line. All work is bounded: the box search uses
//! at most [`AXIS_SAMPLE`] vertices, D2 draws [`D2_PAIRS`] pairs, the
//! closedness check stops at [`CLOSED_CHECK_LIMIT`] triangles. The server
//! computes signatures in the isolated preview process (see `preview`).

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;

/// Bump when the computation changes: stored signatures of another version
/// are ignored and computed again.
pub const VERSION: i16 = 1;
/// Bins of the D2 histogram.
pub const D2_BINS: usize = 32;
/// Point pairs drawn for the D2 histogram.
pub const D2_PAIRS: usize = 32_768;
/// Vertices used to orient the bounding box (evenly spaced subset).
pub const AXIS_SAMPLE: usize = 8_192;
/// Larger meshes are not checked for closedness (their volume is flagged
/// as unreliable instead).
pub const CLOSED_CHECK_LIMIT: usize = 2_000_000;
/// Hull edge directions tried per plane.
const MAX_ANGLES: usize = 720;
/// Vertices closer than this share of the diagonal are one for the
/// closedness check.
const WELD: f64 = 1e-6;
/// Coordinates beyond this are treated as broken (products must stay finite).
const MAX_COORD: f64 = 1e15;
/// Seed of the point sampling (any fixed value; changing it needs a new VERSION).
const SEED: u64 = 0x7EA3_C4AF_D25E_0001;

/// The shape signature of one mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub version: i16,
    /// Oriented bounding box edge lengths, largest first (model units).
    pub extents: [f64; 3],
    /// Surface area (model units²).
    pub area: f64,
    /// Enclosed volume (model units³); reliable only if `closed`.
    pub volume: f64,
    /// Watertight and consistently oriented.
    pub closed: bool,
    pub triangles: u64,
    /// D2 histogram, [`D2_BINS`] values that sum to 1.
    pub d2: Vec<f32>,
}

/// What the preview process writes: the signature or why there is none.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Signature(Signature),
    Error(String),
}

impl Signature {
    /// Middle and smallest extent relative to the largest (scale-free).
    pub fn proportions(&self) -> [f64; 2] {
        let e = self.extents;
        if e[0] > 0.0 {
            [e[1] / e[0], e[2] / e[0]]
        } else {
            [0.0, 0.0]
        }
    }

    /// Sphericity π^⅓·(6V)^⅔ / A (1 = sphere), only for closed meshes.
    pub fn sphericity(&self) -> Option<f64> {
        (self.closed && self.volume > 0.0 && self.area > 0.0).then(|| {
            (std::f64::consts::PI.cbrt() * (6.0 * self.volume).powf(2.0 / 3.0) / self.area).min(1.0)
        })
    }

    /// Plausible values (e.g. read back from the database).
    pub fn is_valid(&self) -> bool {
        self.version == VERSION
            && self.extents.iter().all(|e| e.is_finite() && *e >= 0.0)
            && self.extents[0] > 0.0
            && self.area.is_finite()
            && self.volume.is_finite()
            && self.d2.len() == D2_BINS
            && self.d2.iter().all(|v| v.is_finite() && *v >= 0.0)
    }
}

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// Deterministic random numbers (SplitMix64) – stable across crate versions,
/// unlike a library RNG, because signatures are stored.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// The valid triangles of a mesh: indices in range, finite coordinates.
struct Surface<'a> {
    mesh: &'a Mesh,
    /// Vertex usable (finite, not absurdly large).
    ok: Vec<bool>,
    /// Area-weighted centre; coordinates are taken relative to it.
    centre: V3,
}

impl Surface<'_> {
    fn raw(&self, i: u32) -> V3 {
        let i = i as usize * 3;
        let p = &self.mesh.positions[i..i + 3];
        [p[0] as f64, p[1] as f64, p[2] as f64]
    }

    fn point(&self, i: u32) -> V3 {
        sub(self.raw(i), self.centre)
    }

    /// The three corners of triangle `t`, if valid.
    fn triangle(&self, t: usize) -> Option<[u32; 3]> {
        let tri = self.mesh.indices.get(t * 3..t * 3 + 3)?;
        let tri = [tri[0], tri[1], tri[2]];
        tri.iter()
            .all(|&i| self.ok.get(i as usize).copied().unwrap_or(false))
            .then_some(tri)
    }
}

/// Computes the signature of a triangle mesh. Point clouds, meshes without
/// surface and broken coordinates are errors, never panics.
pub fn compute(mesh: &Mesh) -> Result<Signature> {
    if mesh.is_point_cloud() {
        bail!("Point clouds have no shape signature.");
    }
    if mesh.positions.len() % 3 != 0 || mesh.indices.len() % 3 != 0 {
        bail!("Incomplete vertices or triangles.");
    }
    let triangles = mesh.triangle_count();
    let ok: Vec<bool> = mesh
        .positions
        .chunks_exact(3)
        .map(|p| {
            p.iter()
                .all(|v| v.is_finite() && (*v as f64).abs() < MAX_COORD)
        })
        .collect();
    let mut surface = Surface {
        mesh,
        ok,
        centre: [0.0; 3],
    };

    // Pass 1: area and area-weighted centre (from raw coordinates).
    let mut area = 0.0;
    let mut weighted = [0.0; 3];
    for t in 0..triangles {
        let Some([a, b, c]) = surface.triangle(t) else {
            continue;
        };
        let (a, b, c) = (surface.raw(a), surface.raw(b), surface.raw(c));
        let w = 0.5 * norm(cross(sub(b, a), sub(c, a)));
        if !w.is_finite() {
            continue;
        }
        area += w;
        for k in 0..3 {
            weighted[k] += w * (a[k] + b[k] + c[k]) / 3.0;
        }
    }
    if !(area.is_finite() && area > 0.0) {
        bail!("The model has no surface (degenerate or empty triangles).");
    }
    surface.centre = weighted.map(|v| v / area);

    // Pass 2: second moments of the surface, volume, cumulative areas.
    let mut moment = [[0.0f64; 3]; 3];
    let mut volume = 0.0;
    let mut cumulative = Vec::with_capacity(triangles);
    let mut running = 0.0;
    let mut used = vec![false; mesh.vertex_count()];
    for t in 0..triangles {
        if let Some(tri) = surface.triangle(t) {
            let [a, b, c] = tri.map(|i| surface.point(i));
            let w = 0.5 * norm(cross(sub(b, a), sub(c, a)));
            if w.is_finite() {
                running += w;
                let m = [
                    (a[0] + b[0] + c[0]) / 3.0,
                    (a[1] + b[1] + c[1]) / 3.0,
                    (a[2] + b[2] + c[2]) / 3.0,
                ];
                // ∫ x xᵀ dA over a triangle = A/12 · (9 m mᵀ + a aᵀ + b bᵀ + c cᵀ)
                for (i, row) in moment.iter_mut().enumerate() {
                    for (j, cell) in row.iter_mut().enumerate() {
                        *cell += w / 12.0
                            * (9.0 * m[i] * m[j] + a[i] * a[j] + b[i] * b[j] + c[i] * c[j]);
                    }
                }
                volume += dot(a, cross(b, c)) / 6.0;
            }
            for i in tri {
                used[i as usize] = true;
            }
        }
        cumulative.push(running);
    }
    let covariance = moment.map(|row| row.map(|v| v / area));
    if covariance.iter().flatten().any(|v| !v.is_finite()) || !volume.is_finite() {
        bail!("The model coordinates are out of range.");
    }

    // Oriented bounding box.
    let points: Vec<u32> = (0..mesh.vertex_count() as u32)
        .filter(|&i| used[i as usize])
        .collect();
    let stride = points.len().div_ceil(AXIS_SAMPLE).max(1);
    let sample: Vec<V3> = points
        .iter()
        .step_by(stride)
        .map(|&i| surface.point(i))
        .collect();
    let axes = refine_axes(principal_axes(covariance), &sample);
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &i in &points {
        let p = surface.point(i);
        for k in 0..3 {
            let v = dot(p, axes[k]);
            lo[k] = lo[k].min(v);
            hi[k] = hi[k].max(v);
        }
    }
    let mut extents = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    extents.sort_by(|a, b| b.total_cmp(a));
    let diagonal = norm(extents);
    if !(diagonal.is_finite() && diagonal > 0.0) {
        bail!("The model has no extent.");
    }

    let closed = triangles <= CLOSED_CHECK_LIMIT && is_closed(&surface, triangles, diagonal * WELD);
    let d2 = d2_histogram(&surface, &cumulative, running, diagonal);
    Ok(Signature {
        version: VERSION,
        extents,
        area,
        volume: volume.abs(),
        closed,
        triangles: triangles as u64,
        d2,
    })
}

/// Eigenvectors of the symmetric covariance (cyclic Jacobi), as rows,
/// largest eigenvalue first.
fn principal_axes(mut a: [[f64; 3]; 3]) -> [V3; 3] {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        let scale = a[0][0].abs() + a[1][1].abs() + a[2][2].abs();
        if off <= 1e-15 * scale.max(f64::MIN_POSITIVE) {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() <= f64::MIN_POSITIVE {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            // A' = Jᵀ A J for the rotation J in the (p, q) plane
            for row in a.iter_mut() {
                let (akp, akq) = (row[p], row[q]);
                row[p] = c * akp - s * akq;
                row[q] = s * akp + c * akq;
            }
            let (row_p, row_q) = (a[p], a[q]);
            for k in 0..3 {
                a[p][k] = c * row_p[k] - s * row_q[k];
                a[q][k] = s * row_p[k] + c * row_q[k];
            }
            for row in v.iter_mut() {
                let (vp, vq) = (row[p], row[q]);
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    // columns of v are the eigenvectors
    let mut order = [0usize, 1, 2];
    order.sort_by(|&x, &y| a[y][y].total_cmp(&a[x][x]));
    let axes = order.map(|k| [v[0][k], v[1][k], v[2][k]]);
    if axes.iter().flatten().all(|x| x.is_finite()) {
        axes
    } else {
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    }
}

/// Tightens the box: per plane of two axes, rotates them to the
/// minimum-area rectangle of the projected points (coordinate descent on
/// the box volume, which never grows).
fn refine_axes(mut axes: [V3; 3], sample: &[V3]) -> [V3; 3] {
    if sample.len() < 3 {
        return axes;
    }
    for _ in 0..4 {
        let mut improved = false;
        for fixed in [2usize, 0, 1] {
            let (i, j) = match fixed {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let flat: Vec<[f64; 2]> = sample
                .iter()
                .map(|p| [dot(*p, axes[i]), dot(*p, axes[j])])
                .collect();
            let current = rect_area(&flat, 0.0);
            let (angle, best) = min_area_angle(&flat);
            if best < current * (1.0 - 1e-9) {
                let (c, s) = (angle.cos(), angle.sin());
                let (ai, aj) = (axes[i], axes[j]);
                axes[i] = [
                    c * ai[0] + s * aj[0],
                    c * ai[1] + s * aj[1],
                    c * ai[2] + s * aj[2],
                ];
                axes[j] = [
                    c * aj[0] - s * ai[0],
                    c * aj[1] - s * ai[1],
                    c * aj[2] - s * ai[2],
                ];
                improved = true;
            }
        }
        if !improved {
            break;
        }
    }
    axes
}

/// Area of the axis-aligned rectangle of the points after rotating the
/// frame by `angle`.
fn rect_area(points: &[[f64; 2]], angle: f64) -> f64 {
    let (c, s) = (angle.cos(), angle.sin());
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in points {
        let u = c * p[0] + s * p[1];
        let v = c * p[1] - s * p[0];
        lo = [lo[0].min(u), lo[1].min(v)];
        hi = [hi[0].max(u), hi[1].max(v)];
    }
    (hi[0] - lo[0]) * (hi[1] - lo[1])
}

/// The rotation (0 ≤ angle < 90°) with the smallest enclosing rectangle:
/// one of its sides lies on an edge of the convex hull.
fn min_area_angle(points: &[[f64; 2]]) -> (f64, f64) {
    let hull = convex_hull(points);
    let mut angles: Vec<f64> = vec![0.0];
    let quarter = std::f64::consts::FRAC_PI_2;
    for k in 0..hull.len() {
        let (a, b) = (hull[k], hull[(k + 1) % hull.len()]);
        let angle = (b[1] - a[1]).atan2(b[0] - a[0]).rem_euclid(quarter);
        if angle.is_finite() {
            angles.push(angle);
        }
    }
    angles.sort_by(f64::total_cmp);
    angles.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    if angles.len() > MAX_ANGLES {
        let step = angles.len().div_ceil(MAX_ANGLES);
        angles = angles.into_iter().step_by(step).collect();
    }
    let mut best = (0.0, rect_area(&hull, 0.0));
    for angle in angles {
        let area = rect_area(&hull, angle);
        if area < best.1 {
            best = (angle, area);
        }
    }
    best
}

/// Andrew's monotone chain; collinear points dropped.
fn convex_hull(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut p: Vec<[f64; 2]> = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let turn = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut hull: Vec<[f64; 2]> = Vec::with_capacity(p.len() * 2);
    for &q in p.iter().chain(p.iter().rev().skip(1)) {
        while hull.len() >= 2 && turn(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0 {
            hull.pop();
        }
        hull.push(q);
    }
    hull.pop();
    hull
}

/// Every edge (after welding equal positions) belongs to exactly two
/// triangles that run through it in opposite directions.
fn is_closed(surface: &Surface<'_>, triangles: usize, tolerance: f64) -> bool {
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let mut welded: Vec<u32> = vec![u32::MAX; surface.mesh.vertex_count()];
    let mut edges: Vec<(u64, i8)> = Vec::new();
    for t in 0..triangles {
        let Some(tri) = surface.triangle(t) else {
            return false;
        };
        let w = tri.map(|i| {
            let slot = &mut welded[i as usize];
            if *slot == u32::MAX {
                // positions on a grid of `tolerance` (B-rep tessellations
                // repeat edge vertices per face with tiny differences)
                let key = surface
                    .point(i)
                    .map(|v| (v / tolerance).round().clamp(-9e15, 9e15) as i64);
                let next = ids.len() as u32;
                *slot = *ids.entry(key).or_insert(next);
            }
            *slot
        });
        if w[0] == w[1] || w[1] == w[2] || w[0] == w[2] {
            continue; // collapsed sliver
        }
        for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
            let (lo, hi, dir) = if a < b { (a, b, 1) } else { (b, a, -1) };
            edges.push(((u64::from(lo) << 32) | u64::from(hi), dir));
        }
    }
    if edges.is_empty() {
        return false;
    }
    edges.sort_unstable_by_key(|e| e.0);
    edges
        .chunk_by(|a, b| a.0 == b.0)
        .all(|run| run.len() == 2 && run.iter().map(|e| i32::from(e.1)).sum::<i32>() == 0)
}

/// Histogram of point-pair distances on the surface relative to `diagonal`.
fn d2_histogram(surface: &Surface<'_>, cumulative: &[f64], total: f64, diagonal: f64) -> Vec<f32> {
    let mut rng = SplitMix(SEED);
    let last = cumulative.len().saturating_sub(1);
    let sample = |rng: &mut SplitMix| -> Option<V3> {
        let u = rng.unit() * total;
        let mut t = cumulative.partition_point(|&c| c <= u).min(last);
        // skip back over invalid or empty triangles at the very end
        while surface.triangle(t).is_none() && t > 0 {
            t -= 1;
        }
        let [a, b, c] = surface.triangle(t)?.map(|i| surface.point(i));
        let r1 = rng.unit().sqrt();
        let r2 = rng.unit();
        let (wa, wb, wc) = (1.0 - r1, r1 * (1.0 - r2), r1 * r2);
        Some([
            wa * a[0] + wb * b[0] + wc * c[0],
            wa * a[1] + wb * b[1] + wc * c[1],
            wa * a[2] + wb * b[2] + wc * c[2],
        ])
    };
    let mut bins = [0u32; D2_BINS];
    let mut n = 0u32;
    for _ in 0..D2_PAIRS {
        let (Some(p), Some(q)) = (sample(&mut rng), sample(&mut rng)) else {
            continue;
        };
        let r = norm(sub(p, q)) / diagonal;
        if r.is_finite() {
            let bin = ((r * D2_BINS as f64) as usize).min(D2_BINS - 1);
            bins[bin] += 1;
            n += 1;
        }
    }
    let n = n.max(1) as f32;
    bins.iter().map(|&b| b as f32 / n).collect()
}

/// How alike two signatures are.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Similarity {
    /// `shape × size`, 0…1 – the ranking key.
    pub score: f64,
    /// 1 − half the L1 distance of the D2 histograms (0…1).
    pub shape: f64,
    /// exp(−2·d) with d the mean |ln ratio| of the extents (or, ignoring
    /// scale, of the two proportions); 1 = same size.
    pub size: f64,
}

/// Extents below this fraction of the largest count as this fraction (flat
/// parts would otherwise divide by zero).
const THIN: f64 = 1e-3;

fn floor_extents(e: [f64; 3]) -> [f64; 3] {
    let min = (e[0] * THIN).max(f64::MIN_POSITIVE);
    e.map(|x| x.max(min))
}

/// Compares two signatures:
///
/// * `shape = 1 − ½·Σ|d2ₐ − d2ᵦ|` (the histograms sum to 1, so 0…1);
/// * `size = exp(−2·d)`, `d` = mean of `|ln(eₐ/eᵦ)|` over the three sorted
///   extents – or, with `ignore_scale`, over the two proportions
///   (middle/largest, smallest/largest), so a scaled copy scores 1;
/// * `score = shape × size`.
///
/// 5 % larger in every direction costs about 10 % of the score; twice the
/// size leaves a quarter.
pub fn compare(a: &Signature, b: &Signature, ignore_scale: bool) -> Similarity {
    let l1: f64 =
        a.d2.iter()
            .zip(&b.d2)
            .map(|(x, y)| f64::from((x - y).abs()))
            .sum();
    let shape = (1.0 - 0.5 * l1).clamp(0.0, 1.0);
    let (ea, eb) = (floor_extents(a.extents), floor_extents(b.extents));
    let d = if ignore_scale {
        ((ea[1] / ea[0]).ln() - (eb[1] / eb[0]).ln()).abs() / 2.0
            + ((ea[2] / ea[0]).ln() - (eb[2] / eb[0]).ln()).abs() / 2.0
    } else {
        (0..3).map(|k| (ea[k] / eb[k]).ln().abs()).sum::<f64>() / 3.0
    };
    let size = if d.is_finite() { (-2.0 * d).exp() } else { 0.0 };
    Similarity {
        score: shape * size,
        shape,
        size,
    }
}

/// A length with three significant digits at most: 120, 40.5, 8.25, 0.012.
pub fn fmt_len(x: f64) -> String {
    if !x.is_finite() {
        return "?".into();
    }
    let a = x.abs();
    let s = if a >= 1e6 {
        return format!("{:.3e}", x);
    } else if a >= 100.0 {
        format!("{x:.0}")
    } else if a >= 10.0 {
        format!("{x:.1}")
    } else if a >= 1.0 {
        format!("{x:.2}")
    } else if a >= 0.001 {
        format!("{x:.4}")
    } else if a == 0.0 {
        "0".into()
    } else {
        return format!("{:.2e}", x);
    };
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// `120 × 40 × 8`.
pub fn fmt_extents(e: [f64; 3]) -> String {
    format!("{} × {} × {}", fmt_len(e[0]), fmt_len(e[1]), fmt_len(e[2]))
}

/// One line for people: the extents (or proportions) of both, the shape
/// similarity and – when both are watertight – the volumes.
pub fn explain(source: &Signature, candidate: &Signature, ignore_scale: bool) -> String {
    let sim = compare(source, candidate, ignore_scale);
    let mut out = if ignore_scale {
        let (p, q) = (source.proportions(), candidate.proportions());
        let scale = candidate.extents[0] / source.extents[0];
        format!(
            "proportions 1 : {:.2} : {:.2} vs 1 : {:.2} : {:.2} (scale ×{})",
            q[0],
            q[1],
            p[0],
            p[1],
            fmt_len(scale)
        )
    } else {
        format!(
            "extents {} vs {}",
            fmt_extents(candidate.extents),
            fmt_extents(source.extents)
        )
    };
    out.push_str(&format!(
        ", shape distribution {:.0} % similar",
        (sim.shape * 100.0).floor()
    ));
    if source.closed && candidate.closed {
        out.push_str(&format!(
            ", volume {} vs {}",
            fmt_len(candidate.volume),
            fmt_len(source.volume)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-aligned box mesh (12 triangles, outward normals).
    fn cuboid(sx: f32, sy: f32, sz: f32) -> Mesh {
        let mut positions = Vec::new();
        for i in 0..8 {
            positions.extend([
                if i & 1 == 0 { 0.0 } else { sx },
                if i & 2 == 0 { 0.0 } else { sy },
                if i & 4 == 0 { 0.0 } else { sz },
            ]);
        }
        let quads = [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ];
        let mut indices = Vec::new();
        for q in quads {
            indices.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
        }
        Mesh {
            positions,
            indices,
            colors: Vec::new(),
            parts: 1,
        }
    }

    /// A closed cylinder (radius r, height h, n segments) along z.
    fn cylinder(r: f32, h: f32, n: u32) -> Mesh {
        let mut positions = vec![0.0, 0.0, 0.0, 0.0, 0.0, h];
        for k in 0..n {
            let a = std::f32::consts::TAU * k as f32 / n as f32;
            positions.extend([r * a.cos(), r * a.sin(), 0.0]);
            positions.extend([r * a.cos(), r * a.sin(), h]);
        }
        let mut indices = Vec::new();
        for k in 0..n {
            let (b0, t0) = (2 + 2 * k, 3 + 2 * k);
            let (b1, t1) = (2 + 2 * ((k + 1) % n), 3 + 2 * ((k + 1) % n));
            indices.extend([0, b1, b0, 1, t0, t1, b0, b1, t1, b0, t1, t0]);
        }
        Mesh {
            positions,
            indices,
            colors: Vec::new(),
            parts: 1,
        }
    }

    fn transform(mut m: Mesh, f: impl Fn([f64; 3]) -> [f64; 3]) -> Mesh {
        for p in m.positions.chunks_exact_mut(3) {
            let q = f([p[0] as f64, p[1] as f64, p[2] as f64]);
            p.copy_from_slice(&[q[0] as f32, q[1] as f32, q[2] as f32]);
        }
        m
    }

    /// Rotation by Euler angles (z, y, x) plus a shift.
    fn rotated(m: Mesh, (az, ay, ax): (f64, f64, f64), shift: [f64; 3]) -> Mesh {
        transform(m, |p| {
            let (s, c) = az.sin_cos();
            let p = [c * p[0] - s * p[1], s * p[0] + c * p[1], p[2]];
            let (s, c) = ay.sin_cos();
            let p = [c * p[0] + s * p[2], p[1], -s * p[0] + c * p[2]];
            let (s, c) = ax.sin_cos();
            let p = [p[0], c * p[1] - s * p[2], s * p[1] + c * p[2]];
            [p[0] + shift[0], p[1] + shift[1], p[2] + shift[2]]
        })
    }

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * a.abs().max(b.abs()).max(1e-12)
    }

    #[test]
    fn box_signature_has_its_extents_area_and_volume() {
        let s = compute(&cuboid(120.0, 40.0, 8.0)).unwrap();
        assert!(close(s.extents[0], 120.0, 1e-6), "{:?}", s.extents);
        assert!(close(s.extents[1], 40.0, 1e-6), "{:?}", s.extents);
        assert!(close(s.extents[2], 8.0, 1e-6), "{:?}", s.extents);
        assert!(close(
            s.area,
            2.0 * (120.0 * 40.0 + 120.0 * 8.0 + 40.0 * 8.0),
            1e-9
        ));
        assert!(close(s.volume, 120.0 * 40.0 * 8.0, 1e-9));
        assert!(s.closed);
        assert_eq!(s.triangles, 12);
        assert_eq!(s.d2.len(), D2_BINS);
        assert!(close(s.d2.iter().map(|&v| v as f64).sum(), 1.0, 1e-4));
        assert!(s.is_valid());
        // deterministic
        assert_eq!(s, compute(&cuboid(120.0, 40.0, 8.0)).unwrap());
    }

    #[test]
    fn rotation_and_position_do_not_change_the_signature() {
        let base = compute(&cuboid(10.0, 10.0, 10.0)).unwrap();
        let plate = compute(&cuboid(120.0, 40.0, 8.0)).unwrap();
        for angles in [(0.3, 0.0, 0.0), (0.7, 1.1, -0.4), (2.0, -0.9, 0.35)] {
            for (mesh, reference) in [
                (cuboid(10.0, 10.0, 10.0), &base),
                (cuboid(120.0, 40.0, 8.0), &plate),
            ] {
                let s = compute(&rotated(mesh, angles, [500.0, -20.0, 3.0])).unwrap();
                for k in 0..3 {
                    assert!(
                        close(s.extents[k], reference.extents[k], 0.01),
                        "{angles:?}: {:?} vs {:?}",
                        s.extents,
                        reference.extents
                    );
                }
                assert!(close(s.area, reference.area, 1e-4));
                assert!(close(s.volume, reference.volume, 1e-4));
                let sim = compare(reference, &s, false);
                assert!(sim.shape > 0.97, "{angles:?}: {sim:?}");
                assert!(sim.score > 0.95, "{angles:?}: {sim:?}");
            }
        }
        // A rotated cylinder keeps its extents as well.
        let cyl = compute(&cylinder(20.0, 60.0, 48)).unwrap();
        let turned = compute(&rotated(
            cylinder(20.0, 60.0, 48),
            (0.4, 0.8, 0.2),
            [1.0; 3],
        ))
        .unwrap();
        for k in 0..3 {
            assert!(
                close(cyl.extents[k], turned.extents[k], 0.01),
                "{:?} vs {:?}",
                cyl.extents,
                turned.extents
            );
        }
        assert!(close(cyl.extents[0], 60.0, 1e-3));
        assert!(cyl.closed);
    }

    #[test]
    fn scale_counts_unless_ignored() {
        let cube = compute(&cuboid(10.0, 10.0, 10.0)).unwrap();
        let double = compute(&cuboid(20.0, 20.0, 20.0)).unwrap();
        let near = compute(&cuboid(10.5, 10.0, 9.8)).unwrap();
        // the shape distribution is scale-free
        let scaled = compare(&cube, &double, false);
        assert!(scaled.shape > 0.97, "{scaled:?}");
        assert!(scaled.size < 0.3, "twice the size: {scaled:?}");
        let free = compare(&cube, &double, true);
        assert!(free.score > 0.97, "ignoring scale: {free:?}");
        // a slightly different cube beats the scaled one with scale
        assert!(compare(&cube, &near, false).score > scaled.score);
        let text = explain(&cube, &near, false);
        assert!(
            text.starts_with("extents 10.5 × 10 × 9.8 vs 10 × 10 × 10"),
            "{text}"
        );
        assert!(
            text.contains("% similar") && text.contains("volume"),
            "{text}"
        );
        let text = explain(&cube, &double, true);
        assert!(text.contains("scale ×2"), "{text}");
    }

    #[test]
    fn ranking_puts_the_like_shape_first() {
        let cube = compute(&cuboid(30.0, 30.0, 30.0)).unwrap();
        let candidates = [
            (
                "rotated cube",
                compute(&rotated(
                    cuboid(30.0, 30.0, 30.0),
                    (0.5, 0.5, 0.5),
                    [0.0; 3],
                ))
                .unwrap(),
            ),
            (
                "slightly larger cube",
                compute(&cuboid(31.0, 30.0, 30.0)).unwrap(),
            ),
            (
                "cylinder of the same size",
                compute(&cylinder(15.0, 30.0, 64)).unwrap(),
            ),
            ("plate", compute(&cuboid(30.0, 30.0, 3.0)).unwrap()),
            ("large cube", compute(&cuboid(90.0, 90.0, 90.0)).unwrap()),
        ];
        let mut ranked: Vec<(&str, f64)> = candidates
            .iter()
            .map(|(name, s)| (*name, compare(&cube, s, false).score))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let names: Vec<&str> = ranked.iter().map(|r| r.0).collect();
        assert_eq!(
            &names[..2],
            &["rotated cube", "slightly larger cube"],
            "{ranked:?}"
        );
        assert_eq!(names[2], "cylinder of the same size", "{ranked:?}");
        // the cylinder has the cube's extents but another shape distribution
        let cyl = compare(&cube, &candidates[2].1, false);
        assert!(cyl.size > 0.99 && cyl.shape < 0.97, "{cyl:?}");
    }

    #[test]
    fn hostile_meshes_are_errors_not_panics() {
        let empty = Mesh::default();
        assert!(compute(&empty).is_err());
        // point cloud
        let cloud = Mesh {
            positions: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            ..Mesh::default()
        };
        assert!(compute(&cloud).is_err());
        // all triangles collapsed onto a line
        let line = Mesh {
            positions: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0],
            indices: vec![0, 1, 2, 2, 1, 0],
            ..Mesh::default()
        };
        assert!(compute(&line).is_err());
        // NaN and infinite coordinates, indices out of range
        let mut nan = cuboid(1.0, 1.0, 1.0);
        nan.positions.iter_mut().for_each(|v| *v = f32::NAN);
        assert!(compute(&nan).is_err());
        let mut huge = cuboid(1.0, 1.0, 1.0);
        huge.positions[0] = f32::INFINITY;
        huge.positions[4] = f32::MAX;
        let s = compute(&huge);
        assert!(s.map(|s| s.is_valid()).unwrap_or(true));
        let mut out_of_range = cuboid(1.0, 1.0, 1.0);
        out_of_range.indices.extend([0, 1, 99]);
        let s = compute(&out_of_range).unwrap();
        assert!(!s.closed, "a broken triangle makes the volume unreliable");
        let partial = Mesh {
            positions: vec![0.0, 0.0],
            indices: vec![0],
            ..Mesh::default()
        };
        assert!(compute(&partial).is_err());
        // a single flat triangle: valid, but no volume
        let tri = Mesh {
            positions: vec![0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 3.0, 0.0],
            indices: vec![0, 1, 2],
            ..Mesh::default()
        };
        let s = compute(&tri).unwrap();
        assert!(!s.closed && s.volume == 0.0 && s.extents[2] == 0.0, "{s:?}");
        // comparing with a flat part stays finite
        let cube = compute(&cuboid(4.0, 3.0, 1.0)).unwrap();
        let sim = compare(&cube, &s, false);
        assert!(sim.score.is_finite() && sim.score >= 0.0, "{sim:?}");
        assert!(compare(&s, &s, true).score > 0.99);
        // an open box (one face missing) is not closed
        let mut open = cuboid(2.0, 2.0, 2.0);
        open.indices.truncate(30);
        assert!(!compute(&open).unwrap().closed);
        // inconsistent orientation: a flipped face
        let mut flipped = cuboid(2.0, 2.0, 2.0);
        flipped.indices.swap(0, 1);
        flipped.indices.swap(3, 4);
        assert!(!compute(&flipped).unwrap().closed);
    }

    #[test]
    fn welded_duplicates_still_count_as_closed() {
        // every triangle with its own three vertices, as STL files have them
        let cube = cuboid(5.0, 6.0, 7.0);
        let mut soup = Mesh::default();
        for t in cube.indices.chunks_exact(3) {
            for &i in t {
                let i = i as usize * 3;
                soup.positions.extend_from_slice(&cube.positions[i..i + 3]);
                soup.indices.push(soup.indices.len() as u32);
            }
        }
        let s = compute(&soup).unwrap();
        assert!(s.closed);
        assert!(close(s.volume, 210.0, 1e-9));
        assert_eq!(s, compute(&soup).unwrap());
    }

    /// A finely tessellated cylinder: the work stays bounded (sampled box
    /// search, fixed number of D2 pairs).
    #[test]
    fn large_meshes_are_bounded() {
        let big = cylinder(10.0, 50.0, 40_000);
        assert!(big.triangle_count() >= 160_000);
        let started = std::time::Instant::now();
        let s = compute(&big).unwrap();
        assert!(s.closed);
        assert!(
            close(s.extents[0], 50.0, 1e-3) && close(s.extents[1], 20.0, 1e-3),
            "{:?}",
            s.extents
        );
        // generous for unoptimised test builds on slow CI machines
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(fmt_len(120.0), "120");
        assert_eq!(fmt_len(40.26), "40.3");
        assert_eq!(fmt_len(8.0), "8");
        assert_eq!(fmt_len(0.0125), "0.0125");
        assert_eq!(fmt_len(0.0), "0");
        assert_eq!(fmt_extents([120.0, 40.0, 8.0]), "120 × 40 × 8");
        assert_eq!(fmt_len(f64::NAN), "?");
    }
}
