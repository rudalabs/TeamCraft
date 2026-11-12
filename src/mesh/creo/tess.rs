//! Tessellation of the Creo B-rep: contours → regions → ear clipping in the
//! parameter plane → 3D, with every edge evaluated once on its most exact
//! face so that neighbouring faces share identical vertices, then refined
//! with PN triangles along the analytic normals.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use super::brep::{
    candidates, cross, dot, eval, grow, norm, sub, Brep, Conv, Curve, Kind, Surface, P3,
};
use crate::mesh::{Mesh, MAX_TRIANGLES};

/// Largest number of edges followed around one contour.
const MAX_LOOP: usize = 100_000;
/// Weld distance relative to the model size.
const WELD_REL: f64 = 1e-6;
/// Edges whose two sides differ by more than this (relative to the model
/// size) keep their own vertices per face: the disagreement stays visible.
const SNAP_REL: f64 = 1e-3;
/// Step of the finite differences for the normals (parameter units).
const H: f64 = 1e-4;

fn is_constant(vals: impl Iterator<Item = f64>) -> bool {
    let mut first = None;
    for v in vals {
        match first {
            None => first = Some(v),
            Some(f) if (f - v).abs() > 1e-9 => return false,
            _ => {}
        }
    }
    first.is_some()
}

/// Faces referenced by edges but without a record of their own: the second
/// half of a split periodic face. It shares the primitive of the known face
/// behind a straight seam edge (type 1 or 8, constant u on both sides).
pub fn add_twins(brep: &mut Brep) {
    let ids: HashSet<i64> = brep.surfaces.iter().map(|s| s.id).collect();
    let mut missing: Vec<i64> = brep
        .curves
        .iter()
        .flat_map(|c| c.faces)
        .filter(|g| !ids.contains(g))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    missing.sort_unstable();
    let by_id: HashMap<i64, usize> = brep
        .surfaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect();
    let mut twins = Vec::new();
    for g in missing {
        let mut partner: Option<usize> = None;
        for c in &brep.curves {
            if !c.faces.contains(&g) || !(c.ty == 1 || c.ty == 8) {
                continue;
            }
            let side = usize::from(c.faces[0] != g);
            let other = c.face(1 - side);
            let Some(&oi) = by_id.get(&other) else {
                continue;
            };
            if brep.surfaces.get(oi).is_some_and(|s| {
                matches!(s.kind, Kind::Plane | Kind::Other) || s.kind.is_freeform()
            }) {
                continue;
            }
            if is_constant(c.pts.iter().map(|r| if side == 0 { r[0] } else { r[2] })) {
                partner = Some(oi);
                break;
            }
        }
        let Some(pi) = partner else { continue };
        let Some(p) = brep.surfaces.get(pi) else {
            continue;
        };
        let mut tw = p.clone();
        tw.id = g;
        tw.twin_of = Some(p.id);
        tw.contours = twin_contours(brep, g);
        twins.push(tw);
    }
    brep.surfaces.extend(twins);
}

/// Contours of a twin: chains of the edges that reference it.
fn twin_contours(brep: &Brep, g: i64) -> Vec<(i64, i64)> {
    let on_g: HashMap<i64, (usize, usize)> = brep
        .curves
        .iter()
        .enumerate()
        .filter_map(|(ci, c)| {
            let side = if c.faces[0] == g {
                0
            } else if c.faces[1] == g {
                1
            } else {
                return None;
            };
            Some((c.id, (ci, side)))
        })
        .collect();
    let mut starts = Vec::new();
    let mut seen = HashSet::new();
    let mut ids_sorted: Vec<i64> = on_g.keys().copied().collect();
    ids_sorted.sort_unstable();
    for cid0 in ids_sorted {
        if seen.contains(&cid0) {
            continue;
        }
        let mut cid = cid0;
        for _ in 0..MAX_LOOP {
            let Some(&(ci, side)) = on_g.get(&cid) else {
                break;
            };
            if !seen.insert(cid) {
                break;
            }
            cid = brep.curves.get(ci).map_or(cid, |c| c.next_on(side));
        }
        starts.push(cid0);
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| (s, if k == 0 { 1 } else { -1 }))
        .collect()
}

/// Twins whose convention stayed unresolved got the wrong partner: the
/// curved record whose best candidate fits the 3D points known from the
/// resolved neighbours becomes the donor. Returns how many were changed.
pub fn repair_twins(brep: &mut Brep, fixed: &HashMap<i64, Conv>) -> usize {
    let donors: Vec<usize> = brep
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.twin_of.is_none() && matches!(s.kind, Kind::Cylinder | Kind::Cone | Kind::Torus)
        })
        .map(|(i, _)| i)
        .collect();
    let by_id: HashMap<i64, usize> = brep
        .surfaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect();
    let mut changes: Vec<(usize, usize)> = Vec::new();
    for (ti, tw) in brep.surfaces.iter().enumerate() {
        if tw.twin_of.is_none() || fixed.contains_key(&tw.id) {
            continue;
        }
        let g = tw.id;
        let mut samples: Vec<([f64; 2], P3)> = Vec::new();
        for c in &brep.curves {
            for sd in 0..2 {
                if c.face(sd) != g {
                    continue;
                }
                let other = c.face(1 - sd);
                let (Some(oconv), Some(&oi)) = (fixed.get(&other), by_id.get(&other)) else {
                    continue;
                };
                let Some(os) = brep.surfaces.get(oi) else {
                    continue;
                };
                for r in &c.pts {
                    if r.iter().all(|x| x.is_finite()) {
                        let (um, vm, uo, vo) = if sd == 0 {
                            (r[0], r[1], r[2], r[3])
                        } else {
                            (r[2], r[3], r[0], r[1])
                        };
                        let p = eval(os, oconv, uo, vo);
                        if p.iter().all(|x| x.is_finite()) {
                            samples.push(([um, vm], p));
                        }
                    }
                }
            }
        }
        if samples.is_empty() {
            continue;
        }
        let mut best: Option<(f64, usize)> = None;
        for &di in &donors {
            let Some(d) = brep.surfaces.get(di) else {
                continue;
            };
            for cand in candidates(d.kind) {
                let mut errs: Vec<f64> = samples
                    .iter()
                    .map(|(uv, p)| norm(sub(eval(d, &cand, uv[0], uv[1]), *p)))
                    .filter(|e| e.is_finite())
                    .collect();
                if errs.is_empty() {
                    continue;
                }
                errs.sort_by(|a, b| a.total_cmp(b));
                let med = errs.get(errs.len() / 2).copied().unwrap_or(f64::INFINITY);
                if best.is_none_or(|(e, _)| med < e) {
                    best = Some((med, di));
                }
            }
        }
        if let Some((_, di)) = best {
            if brep.surfaces.get(di).map(|d| d.id) != tw.twin_of {
                changes.push((ti, di));
            }
        }
    }
    for &(ti, di) in &changes {
        let donor = brep.surfaces.get(di).cloned();
        if let (Some(d), Some(tw)) = (donor, brep.surfaces.get_mut(ti)) {
            tw.kind = d.kind;
            tw.kind_name = d.kind_name.clone();
            tw.orient = d.orient;
            tw.frame = d.frame;
            tw.params = d.params;
            tw.freeform = d.freeform.clone();
            tw.twin_of = Some(d.id);
        }
    }
    changes.len()
}

/// A loop point: parameters on the face and the shared 3D point (NaN when
/// the edge is not snapped).
#[derive(Clone, Copy)]
struct LoopPt {
    uv: [f64; 2],
    xyz: P3,
}

/// One 3D polyline per edge, evaluated on its most exact face (when both
/// sides agree to `agree`), and the B-rep vertices unified across the
/// contours (union-find over the loop successors).
fn edge_points(
    brep: &Brep,
    fixed: &HashMap<i64, Conv>,
    by_id: &HashMap<i64, usize>,
    agree: f64,
) -> HashMap<i64, Vec<P3>> {
    let mut out: HashMap<i64, Vec<P3>> = HashMap::new();
    let mut rank: HashMap<i64, u8> = HashMap::new();
    for c in &brep.curves {
        let sides: Vec<(usize, &Surface)> = (0..2)
            .filter_map(|sd| {
                by_id
                    .get(&c.face(sd))
                    .and_then(|&i| brep.surfaces.get(i))
                    .map(|s| (sd, s))
            })
            .collect();
        let Some(&(sd, s)) = sides.iter().min_by_key(|(sd, s)| (s.kind.rank(), *sd)) else {
            continue;
        };
        let Some(conv) = fixed.get(&s.id) else {
            continue;
        };
        let pts: Vec<P3> = c
            .pts
            .iter()
            .map(|r| {
                if sd == 0 {
                    eval(s, conv, r[0], r[1])
                } else {
                    eval(s, conv, r[2], r[3])
                }
            })
            .collect();
        if sides.len() == 2 {
            let Some(&(osd, os)) = sides.get(1 - sd) else {
                continue;
            };
            if let Some(oconv) = fixed.get(&os.id) {
                let mut disagree = false;
                for (r, p) in c.pts.iter().zip(&pts) {
                    let q = if osd == 0 {
                        eval(os, oconv, r[0], r[1])
                    } else {
                        eval(os, oconv, r[2], r[3])
                    };
                    let d = norm(sub(*p, q));
                    if d.is_nan() || d > agree {
                        disagree = true;
                        break;
                    }
                }
                if disagree {
                    continue;
                }
            }
        }
        rank.insert(c.id, s.kind.rank());
        out.insert(c.id, pts);
    }
    // union-find over edge ends that meet in a contour
    let cmap: HashMap<i64, &Curve> = brep.curves.iter().map(|c| (c.id, c)).collect();
    let mut parent: HashMap<(i64, bool), (i64, bool)> = HashMap::new();
    fn find(parent: &mut HashMap<(i64, bool), (i64, bool)>, mut x: (i64, bool)) -> (i64, bool) {
        let mut path = Vec::new();
        while let Some(&p) = parent.get(&x) {
            if p == x {
                break;
            }
            path.push(x);
            x = p;
        }
        for q in path {
            parent.insert(q, x);
        }
        x
    }
    for s in &brep.surfaces {
        for &(start, _) in &s.contours {
            let mut cid = start;
            let mut prev: Option<(i64, bool)> = None;
            let mut first: Option<(i64, bool)> = None;
            for _ in 0..MAX_LOOP {
                let Some(c) = cmap.get(&cid) else { break };
                let side = if c.faces[0] == s.id {
                    0
                } else if c.faces[1] == s.id {
                    1
                } else {
                    break;
                };
                if !out.contains_key(&cid) {
                    break;
                }
                // (edge, end): false = first sample, true = last sample
                let (head, tail) = if c.dir_on(side) == -1 {
                    ((cid, true), (cid, false))
                } else {
                    ((cid, false), (cid, true))
                };
                parent.entry(head).or_insert(head);
                parent.entry(tail).or_insert(tail);
                if let Some(p) = prev {
                    let (ra, rb) = (find(&mut parent, p), find(&mut parent, head));
                    parent.insert(ra, rb);
                }
                first.get_or_insert(head);
                prev = Some(tail);
                cid = c.next_on(side);
                if cid == start {
                    if let (Some(p), Some(f)) = (prev, first) {
                        let (ra, rb) = (find(&mut parent, p), find(&mut parent, f));
                        parent.insert(ra, rb);
                    }
                    break;
                }
            }
        }
    }
    let keys: Vec<(i64, bool)> = parent.keys().copied().collect();
    let mut groups: HashMap<(i64, bool), Vec<(i64, bool)>> = HashMap::new();
    for k in keys {
        let r = find(&mut parent, k);
        groups.entry(r).or_default().push(k);
    }
    for members in groups.values() {
        let best = members
            .iter()
            .filter_map(|&(cid, end)| {
                let pts = out.get(&cid)?;
                let p = if end { *pts.last()? } else { *pts.first()? };
                p.iter().all(|x| x.is_finite()).then_some((
                    rank.get(&cid).copied().unwrap_or(9),
                    cid,
                    p,
                ))
            })
            .min_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let Some((_, _, p)) = best else { continue };
        for &(cid, end) in members {
            if let Some(pts) = out.get_mut(&cid) {
                if end {
                    if let Some(l) = pts.last_mut() {
                        *l = p;
                    }
                } else if let Some(f) = pts.first_mut() {
                    *f = p;
                }
            }
        }
    }
    out
}

/// The contours of a face as closed point lists in its parameter plane.
fn loops(
    s: &Surface,
    curves: &HashMap<i64, &Curve>,
    e3: &HashMap<i64, Vec<P3>>,
) -> Vec<(Vec<LoopPt>, i64)> {
    let mut out = Vec::new();
    for &(start, trv) in &s.contours {
        let mut pts: Vec<LoopPt> = Vec::new();
        let mut cid = start;
        let mut ok = true;
        for _ in 0..MAX_LOOP {
            let Some(c) = curves.get(&cid) else {
                ok = false;
                break;
            };
            let side = if c.faces[0] == s.id {
                0
            } else if c.faces[1] == s.id {
                1
            } else {
                ok = false;
                break;
            };
            let snapped = e3.get(&cid);
            let mut seg: Vec<LoopPt> = c
                .pts
                .iter()
                .enumerate()
                .map(|(k, r)| LoopPt {
                    uv: if side == 0 {
                        [r[0], r[1]]
                    } else {
                        [r[2], r[3]]
                    },
                    xyz: snapped
                        .and_then(|v| v.get(k))
                        .copied()
                        .unwrap_or([f64::NAN; 3]),
                })
                .filter(|p| p.uv[0].is_finite() && p.uv[1].is_finite())
                .collect();
            if c.dir_on(side) == -1 {
                seg.reverse();
            }
            pts.extend(seg);
            cid = c.next_on(side);
            if cid == start {
                break;
            }
        }
        if !ok || pts.len() < 3 {
            continue;
        }
        let mut dedup: Vec<LoopPt> = Vec::with_capacity(pts.len());
        for p in pts {
            if dedup
                .last()
                .is_none_or(|q| (q.uv[0] - p.uv[0]).hypot(q.uv[1] - p.uv[1]) > 1e-9)
            {
                dedup.push(p);
            }
        }
        if dedup.len() > 1 {
            let closed = match (dedup.first(), dedup.last()) {
                (Some(f), Some(l)) => (f.uv[0] - l.uv[0]).hypot(f.uv[1] - l.uv[1]) < 1e-9,
                _ => false,
            };
            if closed {
                dedup.pop();
            }
        }
        if dedup.len() >= 3 {
            out.push((dedup, trv));
        }
    }
    out
}

fn area2(poly: &[[f64; 2]], scale: [f64; 2]) -> f64 {
    let n = poly.len();
    let mut s = 0.0;
    for i in 0..n {
        let a = poly.get(i).copied().unwrap_or_default();
        let b = poly.get((i + 1) % n).copied().unwrap_or_default();
        s += a[0] * scale[0] * b[1] * scale[1] - b[0] * scale[0] * a[1] * scale[1];
    }
    0.5 * s
}

fn point_in_poly(p: [f64; 2], poly: &[[f64; 2]]) -> bool {
    let n = poly.len();
    let mut inside = false;
    for i in 0..n {
        let a = poly.get(i).copied().unwrap_or_default();
        let b = poly.get((i + 1) % n).copied().unwrap_or_default();
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
            if x > p[0] {
                inside = !inside;
            }
        }
    }
    inside
}

/// The coarse mesh: per-face vertices (not yet welded) with their analytic
/// normals, and triangles oriented outwards.
pub struct Coarse {
    pub points: Vec<P3>,
    pub normals: Vec<P3>,
    pub triangles: Vec<[u32; 3]>,
}

/// Tessellates every face of a solid body.
pub fn tessellate(brep: &Brep, fixed: &HashMap<i64, Conv>) -> Result<Coarse> {
    let curves: HashMap<i64, &Curve> = brep.curves.iter().map(|c| (c.id, c)).collect();
    let by_id: HashMap<i64, usize> = brep
        .surfaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect();
    // model size from a first pass over the edges, then the shared polylines
    let rough = edge_points(brep, fixed, &by_id, f64::INFINITY);
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in rough.values().flatten() {
        if p.iter().all(|x| x.is_finite()) {
            grow(&mut lo, &mut hi, *p);
        }
    }
    let size = if lo[0].is_finite() {
        norm(sub(hi, lo)).max(1e-9)
    } else {
        1.0
    };
    let e3 = edge_points(brep, fixed, &by_id, SNAP_REL * size);
    let mut out = Coarse {
        points: Vec::new(),
        normals: Vec::new(),
        triangles: Vec::new(),
    };
    for s in &brep.surfaces {
        if s.kind == Kind::Other {
            continue;
        }
        let conv = fixed
            .get(&s.id)
            .copied()
            .or_else(|| candidates(s.kind).first().copied())
            .unwrap_or(Conv::IDENTITY);
        let polys = loops(s, &curves, &e3);
        if polys.is_empty() {
            continue;
        }
        // metric parameter plane: angles scaled by their radius
        let scale = match s.kind {
            Kind::Cylinder => [s.params[0].abs().max(1e-9), 1.0],
            Kind::Torus => [
                (s.params[0].abs() + s.params[1].abs()).max(1e-9),
                s.params[1].abs().max(1e-9),
            ],
            Kind::Cone => [(s.params[0].tan().abs() * 0.5).max(1e-9), 1.0],
            _ => [1.0, 1.0],
        };
        let uv_of = |p: &Vec<LoopPt>| -> Vec<[f64; 2]> { p.iter().map(|q| q.uv).collect() };
        let mut outers: Vec<&Vec<LoopPt>> = polys
            .iter()
            .filter(|(_, t)| *t == 1)
            .map(|(p, _)| p)
            .collect();
        let mut holes: Vec<&Vec<LoopPt>> = polys
            .iter()
            .filter(|(_, t)| *t != 1)
            .map(|(p, _)| p)
            .collect();
        if outers.is_empty() {
            let Some((k, _)) = polys.iter().enumerate().max_by(|a, b| {
                area2(&uv_of(&a.1 .0), scale)
                    .abs()
                    .total_cmp(&area2(&uv_of(&b.1 .0), scale).abs())
            }) else {
                continue;
            };
            outers = polys.get(k).map(|(p, _)| vec![p]).unwrap_or_default();
            holes = polys
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != k)
                .map(|(_, (p, _))| p)
                .collect();
        }
        let flip = s.orient == -1;
        for outer in outers {
            let mut ring: Vec<LoopPt> = outer.clone();
            if area2(&uv_of(&ring), scale) < 0.0 {
                ring.reverse();
            }
            let ring_uv = uv_of(&ring);
            let mut coords: Vec<f64> = Vec::new();
            let mut pts: Vec<LoopPt> = Vec::new();
            let mut hole_starts: Vec<usize> = Vec::new();
            for p in &ring {
                coords.extend([p.uv[0] * scale[0], p.uv[1] * scale[1]]);
                pts.push(*p);
            }
            for h in &holes {
                let Some(first) = h.first() else { continue };
                if !point_in_poly(first.uv, &ring_uv) {
                    continue;
                }
                let mut hr: Vec<LoopPt> = (*h).clone();
                if area2(&uv_of(&hr), scale) > 0.0 {
                    hr.reverse();
                }
                hole_starts.push(pts.len());
                for p in &hr {
                    coords.extend([p.uv[0] * scale[0], p.uv[1] * scale[1]]);
                    pts.push(*p);
                }
            }
            let pairs: Vec<[f64; 2]> = coords
                .chunks_exact(2)
                .filter_map(|c| <[f64; 2]>::try_from(c).ok())
                .collect();
            let tris: Vec<usize> = crate::mesh::earcut::earcut(&pairs, &hole_starts)
                .into_iter()
                .flatten()
                .collect();
            if tris.len() < 3 {
                continue;
            }
            let tris = delaunay_flips(tris, &coords);
            let signed: f64 = tris
                .chunks(3)
                .map(|t| {
                    let g = |k: usize| {
                        pts.get(t.get(k).copied().unwrap_or(usize::MAX))
                            .map(|p| p.uv)
                            .unwrap_or_default()
                    };
                    area2(&[g(0), g(1), g(2)], scale)
                })
                .sum();
            let ccw = signed >= 0.0;
            let base = u32::try_from(out.points.len()).map_err(|_| anyhow!("too many vertices"))?;
            for p in &pts {
                let [u, v] = p.uv;
                let xyz = if p.xyz.iter().all(|x| x.is_finite()) {
                    p.xyz
                } else {
                    eval(s, &conv, u, v)
                };
                let du = sub(eval(s, &conv, u + H, v), eval(s, &conv, u - H, v));
                let dv = sub(eval(s, &conv, u, v + H), eval(s, &conv, u, v - H));
                let mut n = cross(du, dv);
                let l = norm(n);
                n = if l > 1e-18 && n.iter().all(|x| x.is_finite()) {
                    n.map(|x| x / l)
                } else {
                    [0.0; 3]
                };
                if flip {
                    n = n.map(|x| -x);
                }
                if !xyz.iter().all(|x| x.is_finite()) {
                    bail!("face {} evaluates to NaN", s.id);
                }
                out.points.push(xyz);
                out.normals.push(n);
            }
            for t in tris.chunks(3) {
                let (Some(&a), Some(&b), Some(&c)) = (t.first(), t.get(1), t.get(2)) else {
                    continue;
                };
                let (a, b, c) = (u32::try_from(a)?, u32::try_from(b)?, u32::try_from(c)?);
                let tri = if ccw != flip {
                    [base + a, base + b, base + c]
                } else {
                    [base + a, base + c, base + b]
                };
                out.triangles.push(tri);
                if out.triangles.len() > MAX_TRIANGLES {
                    bail!("The model has more than {MAX_TRIANGLES} triangles – more than the preview shows.");
                }
            }
        }
    }
    if out.triangles.is_empty() {
        bail!("The Creo part contains no faces that could be tessellated.");
    }
    Ok(out)
}

/// Lawson flips towards a Delaunay triangulation in the metric parameter
/// plane: ear clipping leaves long thin triangles that, mapped to a curved
/// face, cut through the material. Boundary edges are never flipped.
fn delaunay_flips(tris: Vec<usize>, coords: &[f64]) -> Vec<usize> {
    let n = tris.len() / 3;
    let mut t: Vec<[usize; 3]> = tris
        .chunks_exact(3)
        .filter_map(|c| <[usize; 3]>::try_from(c).ok())
        .collect();
    let pt = |i: usize| -> [f64; 2] {
        [
            coords.get(2 * i).copied().unwrap_or(0.0),
            coords.get(2 * i + 1).copied().unwrap_or(0.0),
        ]
    };
    let in_circle = |a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]| -> bool {
        let (ax, ay) = (a[0] - d[0], a[1] - d[1]);
        let (bx, by) = (b[0] - d[0], b[1] - d[1]);
        let (cx, cy) = (c[0] - d[0], c[1] - d[1]);
        let det = (ax * ax + ay * ay) * (bx * cy - cx * by)
            - (bx * bx + by * by) * (ax * cy - cx * ay)
            + (cx * cx + cy * cy) * (ax * by - bx * ay);
        let orient = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if orient > 0.0 {
            det > 1e-12
        } else {
            det < -1e-12
        }
    };
    // Lawson passes; each pass flips many edges, so a few dozen suffice.
    // A BTreeMap keeps the result the same from run to run.
    for _ in 0..n.clamp(1, 64) {
        let mut edges: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (ti, &[x, y, z]) in t.iter().enumerate() {
            for (a, b) in [(x, y), (y, z), (z, x)] {
                edges.entry((a.min(b), a.max(b))).or_default().push(ti);
            }
        }
        let mut flipped = false;
        let mut done: HashSet<usize> = HashSet::new();
        for (&(a, b), owners) in &edges {
            let [ti, tj] = owners.as_slice() else {
                continue;
            };
            if done.contains(ti) || done.contains(tj) {
                continue;
            }
            let (Some(&x), Some(&y)) = (t.get(*ti), t.get(*tj)) else {
                continue;
            };
            let Some(&c) = x.iter().find(|&&v| v != a && v != b) else {
                continue;
            };
            let Some(&d) = y.iter().find(|&&v| v != a && v != b) else {
                continue;
            };
            if !in_circle(pt(a), pt(b), pt(c), pt(d)) {
                continue;
            }
            // the quad must be convex: both new triangles keep the orientation
            let orient = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| {
                (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
            };
            let o_old = orient(pt(x[0]), pt(x[1]), pt(x[2]));
            let new1 = [c, d, b];
            let new2 = [d, c, a];
            let (n1, n2) = (orient(pt(c), pt(d), pt(b)), orient(pt(d), pt(c), pt(a)));
            if n1 * o_old <= 0.0 || n2 * o_old <= 0.0 {
                continue;
            }
            if let Some(v) = t.get_mut(*ti) {
                *v = new1;
            }
            if let Some(v) = t.get_mut(*tj) {
                *v = new2;
            }
            done.insert(*ti);
            done.insert(*tj);
            flipped = true;
        }
        if !flipped {
            break;
        }
    }
    t.into_iter().flatten().collect()
}

/// Welds the per-face vertices, refines along the normals and builds the
/// preview mesh (model units are kept).
pub fn build(coarse: &Coarse) -> Result<Mesh> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &coarse.points {
        grow(&mut lo, &mut hi, *p);
    }
    let size = norm(sub(hi, lo)).max(1e-9);
    let weld = WELD_REL * size;
    let mut points: Vec<P3> = Vec::new();
    let mut welded: HashMap<[i64; 3], u32> = HashMap::new();
    let mut index: Vec<u32> = Vec::with_capacity(coarse.points.len());
    for p in &coarse.points {
        let grid = p.map(|c| (c / weld).round() as i64);
        let next = u32::try_from(points.len()).map_err(|_| anyhow!("too many vertices"))?;
        index.push(*welded.entry(grid).or_insert_with(|| {
            points.push(*p);
            next
        }));
    }
    let mut triangles: Vec<[u32; 3]> = Vec::with_capacity(coarse.triangles.len());
    let mut normal_triangles: Vec<[u32; 3]> = Vec::with_capacity(coarse.triangles.len());
    for t in &coarse.triangles {
        let w = t.map(|k| index.get(k as usize).copied().unwrap_or(u32::MAX));
        if w[0] == w[1] || w[1] == w[2] || w[0] == w[2] || w.contains(&u32::MAX) {
            continue;
        }
        triangles.push(w);
        normal_triangles.push(*t);
    }
    let volume: f64 = triangles
        .iter()
        .map(|t| {
            let g = |k: u32| points.get(k as usize).copied().unwrap_or_default();
            dot(g(t[0]), cross(g(t[1]), g(t[2])))
        })
        .sum::<f64>()
        / 6.0;
    let mut normals = coarse.normals.clone();
    if volume < 0.0 {
        for t in &mut triangles {
            t.swap(1, 2);
        }
        for t in &mut normal_triangles {
            t.swap(1, 2);
        }
        for n in &mut normals {
            *n = n.map(|x| -x);
        }
    }
    let level = crate::mesh::pn::refine_level(triangles.len());
    let (points, triangles) = if level > 1 {
        crate::mesh::pn::refine(&points, &triangles, &normals, &normal_triangles, level)
    } else {
        (points, triangles)
    };
    if triangles.len() > MAX_TRIANGLES {
        bail!("The model has more than {MAX_TRIANGLES} triangles – more than the preview shows.");
    }
    let mut mesh = Mesh {
        parts: 1,
        ..Mesh::default()
    };
    for p in &points {
        mesh.vertex(*p);
    }
    mesh.indices.extend(triangles.iter().flatten());
    Ok(mesh)
}
