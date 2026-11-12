//! The B-rep of a Creo part as the `VisibGeom` section stores it: analytic
//! surfaces with their contours, and edges sampled as point lists in the
//! parameter spaces of both neighbouring faces.
//!
//! ```text
//! Sld_VisGeom / active_geom
//!   srf_array[]  geom_id, geom_type (34 plane, 36 cylinder, 37 cone, 38 torus, 40 spline,
//!                41 fillet, 42 ruled), orient (±1), envlp, outline,
//!                srf_prim_ptr(<kind>) { local_sys[4][3] (rows 0–2: rotation by columns,
//!                row 3: origin), radius | half_angle | radius1, radius2 },
//!                frst_cntr_ptr / next_cntr_ptr { frst_cntr_crv_hdr_ptr, trv (1 outer, -1 hole) },
//!                topol_ref_data { list_id, list_type (98 = solid body) }
//!   crv_array[]  crv_id, type, crv_pnt_dir[2], crv_pnt_arr[n][4] = (u0 v0 u1 v1),
//!                crv_hdr_geom_ptr[0/1] (the two faces), next_crv_hdr_ptr[0/1] (next edge of
//!                the contour on that face)
//! ```
//!
//! The parametrisation of a face (direction of u and v, angle offset, which
//! of the two angles of a torus is u, …) is not written; it is resolved by
//! walking from planes with the identity frame across shared edges and
//! keeping the variant whose points coincide with the neighbour's.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use super::tagged::Tree;

pub type P3 = [f64; 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Plane,
    Cylinder,
    Cone,
    Torus,
    /// Interpolating bicubic spline (`splsrf`).
    Spline,
    /// Rolling-ball blend (`fillet_srf`).
    Fillet,
    /// Ruled surface between two curves (`ruled_srf`).
    Ruled,
    Other,
}

impl Kind {
    /// Freeform faces carry absolute 3D data: no convention to resolve.
    pub fn is_freeform(self) -> bool {
        matches!(self, Kind::Spline | Kind::Fillet | Kind::Ruled)
    }

    /// Exactness rank: analytic faces evaluate exactly, splines approximate
    /// their neighbours (used to pick the face an edge is evaluated on).
    pub fn rank(self) -> u8 {
        match self {
            Kind::Plane => 0,
            Kind::Cylinder => 1,
            Kind::Cone => 2,
            Kind::Torus => 3,
            Kind::Ruled => 4,
            Kind::Fillet => 5,
            Kind::Spline => 6,
            Kind::Other => 9,
        }
    }
}

/// A rigid frame: columns X, Y, Z and the origin.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub x: P3,
    pub y: P3,
    pub z: P3,
    pub o: P3,
}

#[derive(Debug, Clone)]
pub struct Surface {
    pub id: i64,
    pub kind: Kind,
    pub kind_name: String,
    pub orient: i64,
    pub frame: Frame,
    /// radius (cylinder), half angle (cone), (radius1, radius2) (torus).
    pub params: [f64; 2],
    /// Contours: (first edge, trv).
    pub contours: Vec<(i64, i64)>,
    /// `topol_ref_data.list_type`: 98 for faces of a solid body.
    pub list_type: Option<i64>,
    /// The face whose record this twin half shares (see `tess::add_twins`).
    pub twin_of: Option<i64>,
    /// Spline, fillet or ruled surface data.
    pub freeform: Option<super::freeform::Freeform>,
}

#[derive(Debug, Clone)]
pub struct Curve {
    pub id: i64,
    pub ty: i64,
    pub faces: [i64; 2],
    pub next: [i64; 2],
    pub dir: [i64; 2],
    /// (u0, v0, u1, v1) per sample.
    pub pts: Vec<[f64; 4]>,
}

impl Curve {
    /// The face on `side` (0 or 1).
    pub fn face(&self, side: usize) -> i64 {
        if side == 0 {
            self.faces[0]
        } else {
            self.faces[1]
        }
    }

    /// The next edge of the contour on `side`.
    pub fn next_on(&self, side: usize) -> i64 {
        if side == 0 {
            self.next[0]
        } else {
            self.next[1]
        }
    }

    /// The direction of the samples on `side` (−1 = reversed).
    pub fn dir_on(&self, side: usize) -> i64 {
        if side == 0 {
            self.dir[0]
        } else {
            self.dir[1]
        }
    }
}

#[derive(Debug, Default)]
pub struct Brep {
    pub surfaces: Vec<Surface>,
    pub curves: Vec<Curve>,
}

fn p3(v: &[f64]) -> Option<P3> {
    Some([*v.first()?, *v.get(1)?, *v.get(2)?])
}

// Fixed-size arrays: i, j, k, a and b are all below 3.
#[allow(clippy::indexing_slicing)]
pub(crate) fn frame_of(rows: &[Vec<f64>]) -> Option<Frame> {
    let r0 = p3(rows.first()?)?;
    let r1 = p3(rows.get(1)?)?;
    let r2 = p3(rows.get(2)?)?;
    let o = p3(rows.get(3)?)?;
    // rows 0..2 hold the rotation by columns
    let mut m = [r0, r1, r2];
    // a single unknown entry (NaN) is repaired from orthonormality
    for _ in 0..2 {
        for i in 0..3 {
            for j in 0..3 {
                if m[i][j].is_nan() {
                    let others: Vec<f64> = (0..3).filter(|&k| k != j).map(|k| m[i][k]).collect();
                    if others.iter().all(|x| x.is_finite()) {
                        let a = (j + 1) % 3;
                        let b = (j + 2) % 3;
                        let ca = [m[0][a], m[1][a], m[2][a]];
                        let cb = [m[0][b], m[1][b], m[2][b]];
                        if ca.iter().chain(cb.iter()).all(|x| x.is_finite()) {
                            m[i][j] = cross(ca, cb)[i];
                        } else {
                            m[i][j] = (1.0 - others.iter().map(|x| x * x).sum::<f64>())
                                .max(0.0)
                                .sqrt();
                        }
                    }
                }
            }
        }
    }
    let x = [m[0][0], m[1][0], m[2][0]];
    let y = [m[0][1], m[1][1], m[2][1]];
    let z = [m[0][2], m[1][2], m[2][2]];
    if x.iter()
        .chain(y.iter())
        .chain(z.iter())
        .chain(o.iter())
        .any(|v| !v.is_finite())
    {
        return None;
    }
    Some(Frame { x, y, z, o })
}

pub fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn norm(a: P3) -> f64 {
    dot(a, a).sqrt()
}

/// Widens the box `lo`–`hi` to contain `p`.
pub fn grow(lo: &mut P3, hi: &mut P3, p: P3) {
    for ((l, h), x) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
        *l = l.min(x);
        *h = h.max(x);
    }
}

/// `a + k·b`.
pub fn lin(a: P3, k: f64, b: P3) -> P3 {
    [a[0] + k * b[0], a[1] + k * b[1], a[2] + k * b[2]]
}

/// `k·a`.
pub fn scale(k: f64, a: P3) -> P3 {
    [k * a[0], k * a[1], k * a[2]]
}

fn axpy(out: &mut P3, s: f64, v: P3) {
    out[0] += s * v[0];
    out[1] += s * v[1];
    out[2] += s * v[2];
}

/// Reads the faces and edges of a `VisibGeom` tree.
pub fn extract(tree: &Tree) -> Brep {
    let mut brep = Brep::default();
    let root = tree.root();
    let mut nodes = vec![root];
    // surfaces and curves may sit anywhere below the root (older parsers
    // nested them); collect every struct that looks like one
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        nodes.push(n);
        match tree.node(n) {
            super::tagged::Node::Struct(kids) => stack.extend(kids.iter().map(|(_, c)| *c)),
            super::tagged::Node::List(items) => stack.extend(items.iter().copied()),
            _ => {}
        }
    }
    for &n in &nodes {
        if let Some(s) = surface(tree, n) {
            brep.surfaces.push(s);
        } else if let Some(c) = curve(tree, n) {
            brep.curves.push(c);
        }
    }
    brep
}

fn surface(tree: &Tree, n: usize) -> Option<Surface> {
    let id = tree.int_at(n, "geom_id")?;
    let geom_type = tree.int_at(n, "geom_type")?;
    tree.child(n, "envlp")?;
    let (kind_name, prim) = tree
        .children_with_prefix(n, "srf_prim_ptr(")
        .into_iter()
        .next()
        .map(|(name, c)| {
            (
                name.trim_start_matches("srf_prim_ptr(")
                    .trim_end_matches(')')
                    .to_string(),
                c,
            )
        })?;
    let freeform = super::freeform::read(tree, &kind_name, prim);
    let kind = match (geom_type, kind_name.as_str()) {
        (34, _) => Kind::Plane,
        (36, _) => Kind::Cylinder,
        (37, _) => Kind::Cone,
        (38, _) => Kind::Torus,
        (_, "splsrf") if freeform.is_some() => Kind::Spline,
        (_, "fillet_srf") if freeform.is_some() => Kind::Fillet,
        (_, "ruled_srf") if freeform.is_some() => Kind::Ruled,
        _ => Kind::Other,
    };
    let frame = tree
        .child(prim, "local_sys")
        .and_then(|ls| frame_of(&tree.rows(ls)))
        .unwrap_or(Frame {
            x: [1.0, 0.0, 0.0],
            y: [0.0, 1.0, 0.0],
            z: [0.0, 0.0, 1.0],
            o: [0.0; 3],
        });
    let params = match kind {
        Kind::Cylinder => [tree.dbl_at(prim, "radius").unwrap_or(f64::NAN), 0.0],
        Kind::Cone => [tree.dbl_at(prim, "half_angle").unwrap_or(f64::NAN), 0.0],
        Kind::Torus => [
            tree.dbl_at(prim, "radius1").unwrap_or(f64::NAN),
            tree.dbl_at(prim, "radius2").unwrap_or(f64::NAN),
        ],
        _ => [0.0, 0.0],
    };
    let mut contours = Vec::new();
    let mut conts: Vec<usize> = tree.children(n, "frst_cntr_ptr");
    for c in tree.children(n, "next_cntr_ptr") {
        match tree.node(c) {
            super::tagged::Node::List(items) => conts.extend(items.iter().copied()),
            _ => conts.push(c),
        }
    }
    for c in conts {
        if let Some(start) = tree.int_at(c, "frst_cntr_crv_hdr_ptr") {
            contours.push((start, tree.int_at(c, "trv").unwrap_or(0)));
        }
    }
    let list_type = tree
        .child(n, "topol_ref_data")
        .and_then(|t| tree.int_at(t, "list_type"));
    let orient = tree.int_at(n, "orient").unwrap_or(1);
    Some(Surface {
        id,
        kind,
        kind_name,
        orient,
        frame,
        params,
        contours,
        list_type,
        twin_of: None,
        freeform,
    })
}

fn curve(tree: &Tree, n: usize) -> Option<Curve> {
    let id = tree.int_at(n, "crv_id")?;
    let arr = tree.child(n, "crv_pnt_arr")?;
    let faces = [
        tree.int_at(n, "crv_hdr_geom_ptr[0]")?,
        tree.int_at(n, "crv_hdr_geom_ptr[1]")?,
    ];
    let next = [
        tree.int_at(n, "next_crv_hdr_ptr[0]")?,
        tree.int_at(n, "next_crv_hdr_ptr[1]")?,
    ];
    let dir_list = tree
        .child(n, "crv_pnt_dir")
        .map(|d| tree.dbls(d))
        .unwrap_or_default();
    let dir = [
        dir_list.first().copied().unwrap_or(1.0) as i64,
        dir_list.get(1).copied().unwrap_or(1.0) as i64,
    ];
    let pts = tree
        .rows(arr)
        .into_iter()
        .filter_map(|r| <[f64; 4]>::try_from(r.as_slice()).ok())
        .collect();
    Some(Curve {
        id,
        ty: tree.int_at(n, "type").unwrap_or(0),
        faces,
        next,
        dir,
        pts,
    })
}

/// One way to read the parameters of a face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Conv {
    /// Sign of u (and swap flag for the torus in `swap`).
    pub su: f64,
    /// Offset added to the first angle.
    pub off_u: f64,
    pub sv: f64,
    /// Offset added to the second angle (torus).
    pub off_v: f64,
    /// Torus: u and v exchanged.
    pub swap: bool,
    /// Torus: sign of the tube radius; cone: 1 = slant length instead of height.
    pub extra: f64,
}

impl Conv {
    pub const IDENTITY: Conv = Conv {
        su: 1.0,
        off_u: 0.0,
        sv: 1.0,
        off_v: 0.0,
        swap: false,
        extra: 1.0,
    };

    /// Handedness of the parametrisation relative to the canonical one.
    pub fn handed(&self) -> f64 {
        self.su * self.sv * if self.swap { -1.0 } else { 1.0 }
    }
}

/// The candidate conventions of a surface kind.
pub fn candidates(kind: Kind) -> Vec<Conv> {
    let mut out = Vec::new();
    match kind {
        Kind::Plane => {
            for su in [1.0, -1.0] {
                for sv in [1.0, -1.0] {
                    out.push(Conv {
                        su,
                        sv,
                        ..Conv::IDENTITY
                    });
                }
            }
        }
        Kind::Cylinder => {
            for su in [1.0, -1.0] {
                for off_u in [0.0, PI] {
                    for sv in [1.0, -1.0] {
                        out.push(Conv {
                            su,
                            off_u,
                            sv,
                            ..Conv::IDENTITY
                        });
                    }
                }
            }
        }
        Kind::Cone => {
            for su in [1.0, -1.0] {
                for off_u in [0.0, PI] {
                    for sv in [1.0, -1.0] {
                        for extra in [0.0, 1.0] {
                            out.push(Conv {
                                su,
                                off_u,
                                sv,
                                extra,
                                ..Conv::IDENTITY
                            });
                        }
                    }
                }
            }
        }
        Kind::Torus => {
            for swap in [false, true] {
                for su in [1.0, -1.0] {
                    for off_u in [0.0, PI] {
                        for sv in [1.0, -1.0] {
                            for off_v in [0.0, 0.5 * PI, PI, 1.5 * PI] {
                                for extra in [1.0, -1.0] {
                                    out.push(Conv {
                                        su,
                                        off_u,
                                        sv,
                                        off_v,
                                        swap,
                                        extra,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        Kind::Spline | Kind::Fillet | Kind::Ruled => out.push(Conv::IDENTITY),
        Kind::Other => {}
    }
    out
}

/// The 3D point of parameters (u, v) on `s` read with `c`.
pub fn eval(s: &Surface, c: &Conv, u: f64, v: f64) -> P3 {
    let f = &s.frame;
    let mut p = f.o;
    match s.kind {
        Kind::Plane => {
            axpy(&mut p, c.su * u, f.x);
            axpy(&mut p, c.sv * v, f.y);
        }
        Kind::Cylinder => {
            let r = s.params[0].abs();
            let a = c.su * u + c.off_u;
            axpy(&mut p, r * a.cos(), f.x);
            axpy(&mut p, r * a.sin(), f.y);
            axpy(&mut p, c.sv * v, f.z);
        }
        Kind::Cone => {
            let ha = s.params[0];
            let a = c.su * u + c.off_u;
            let (rr, h) = if c.extra > 0.5 {
                (v * ha.sin(), v * ha.cos())
            } else {
                (v * ha.tan(), v)
            };
            axpy(&mut p, rr * a.cos(), f.x);
            axpy(&mut p, rr * a.sin(), f.y);
            axpy(&mut p, c.sv * h, f.z);
        }
        Kind::Torus => {
            let (r1, r2) = (s.params[0], s.params[1].abs() * c.extra);
            let (a, b) = if c.swap { (v, u) } else { (u, v) };
            let th = c.su * a + c.off_u;
            let ph = c.sv * b + c.off_v;
            let ring = r1 + r2 * ph.cos();
            axpy(&mut p, ring * th.cos(), f.x);
            axpy(&mut p, ring * th.sin(), f.y);
            axpy(&mut p, r2 * ph.sin(), f.z);
        }
        Kind::Spline | Kind::Fillet | Kind::Ruled => {
            return s
                .freeform
                .as_ref()
                .map_or([f64::NAN; 3], |ff| ff.eval(u, v));
        }
        Kind::Other => return [f64::NAN; 3],
    }
    p
}

/// Resolves the convention of every face by walking from planes with the
/// identity frame across shared edges. Faces that stay unresolved get the
/// first candidate.
pub fn resolve(brep: &Brep) -> HashMap<i64, Conv> {
    let by_id: HashMap<i64, &Surface> = brep.surfaces.iter().map(|s| (s.id, s)).collect();
    let mut adj: HashMap<i64, Vec<(usize, usize)>> = HashMap::new();
    for (ci, c) in brep.curves.iter().enumerate() {
        if by_id.contains_key(&c.faces[0]) && by_id.contains_key(&c.faces[1]) {
            adj.entry(c.faces[0]).or_default().push((ci, 0));
            adj.entry(c.faces[1]).or_default().push((ci, 1));
        }
    }
    let mut fixed: HashMap<i64, Conv> = HashMap::new();
    for s in &brep.surfaces {
        if s.kind.is_freeform() {
            fixed.insert(s.id, Conv::IDENTITY);
        }
        if s.kind == Kind::Plane {
            let f = &s.frame;
            let ident = |a: P3, b: P3| sub(a, b).iter().all(|d| d.abs() < 1e-9);
            if ident(f.x, [1.0, 0.0, 0.0])
                && ident(f.y, [0.0, 1.0, 0.0])
                && ident(f.z, [0.0, 0.0, 1.0])
            {
                fixed.insert(s.id, Conv::IDENTITY);
            }
        }
    }
    // model size from the fixed planes' edge points
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (&id, conv) in &fixed {
        let Some(s) = by_id.get(&id) else { continue };
        for &(ci, side) in adj
            .get(&id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
            .iter()
            .take(50)
        {
            let Some(c) = brep.curves.get(ci) else {
                continue;
            };
            for r in &c.pts {
                let (u, v) = if side == 0 {
                    (r[0], r[1])
                } else {
                    (r[2], r[3])
                };
                if !(u.is_finite() && v.is_finite()) {
                    continue;
                }
                grow(&mut lo, &mut hi, eval(s, conv, u, v));
            }
        }
    }
    let size = if lo[0].is_finite() {
        norm(sub(hi, lo)).max(1e-3)
    } else {
        1.0
    };
    let tol = (1e-6 * size).max(1e-6) * 10.0;
    for _round in 0..60 {
        let mut changed = false;
        for s in &brep.surfaces {
            if fixed.contains_key(&s.id) {
                continue;
            }
            let cands = candidates(s.kind);
            if cands.is_empty() {
                continue;
            }
            let mut score = vec![0usize; cands.len()];
            let mut rows = 0usize;
            for &(ci, side) in adj.get(&s.id).map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some(c) = brep.curves.get(ci) else {
                    continue;
                };
                let other = c.face(1 - side);
                let (Some(oconv), Some(os)) = (fixed.get(&other), by_id.get(&other)) else {
                    continue;
                };
                for r in &c.pts {
                    let (um, vm, uo, vo) = if side == 0 {
                        (r[0], r[1], r[2], r[3])
                    } else {
                        (r[2], r[3], r[0], r[1])
                    };
                    if ![um, vm, uo, vo].iter().all(|x| x.is_finite()) {
                        continue;
                    }
                    let p = eval(os, oconv, uo, vo);
                    if !p.iter().all(|x| x.is_finite()) {
                        continue;
                    }
                    rows += 1;
                    for (k, cand) in cands.iter().enumerate() {
                        let q = eval(s, cand, um, vm);
                        if norm(sub(q, p)) < tol {
                            if let Some(sc) = score.get_mut(k) {
                                *sc += 1;
                            }
                        }
                    }
                }
            }
            if rows == 0 {
                continue;
            }
            // on a tie the first candidate wins (symmetric edges, e.g. all
            // samples at u = 0, cannot tell the signs apart)
            let Some((best, &best_score)) = score.iter().enumerate().fold(
                None,
                |acc: Option<(usize, &usize)>, (k, v)| match acc {
                    Some((_, b)) if b >= v => acc,
                    _ => Some((k, v)),
                },
            ) else {
                continue;
            };
            if best_score > 0 && best_score * 10 >= rows * 6 {
                if let Some(c) = cands.get(best) {
                    fixed.insert(s.id, *c);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for s in &brep.surfaces {
        if let Some(c) = candidates(s.kind).first() {
            fixed.entry(s.id).or_insert(*c);
        }
    }
    fixed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brep_of_ctc01() -> Brep {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cad/nist/ctc-01/creo-10/nist_ctc_01_asme1_rd_cr10030.prt.5"
        ))
        .unwrap();
        let toc = super::super::container::toc(&data).unwrap();
        let sec = super::super::container::section(&data, &toc, "VisibGeom")
            .unwrap()
            .unwrap();
        extract(&super::super::tagged::parse(&sec).unwrap())
    }

    #[test]
    fn faces_edges_and_frames_of_ctc_01() {
        let b = brep_of_ctc01();
        assert_eq!(b.surfaces.len(), 141);
        assert_eq!(b.curves.len(), 422);
        let kinds = |k: Kind| b.surfaces.iter().filter(|s| s.kind == k).count();
        assert_eq!(
            (kinds(Kind::Plane), kinds(Kind::Cylinder), kinds(Kind::Cone)),
            (78, 59, 4)
        );
        // every rotation is right-handed and orthonormal
        for s in &b.surfaces {
            let f = &s.frame;
            assert!((dot(cross(f.x, f.y), f.z) - 1.0).abs() < 1e-9, "{}", s.id);
        }
        let hole = b.surfaces.iter().find(|s| s.id == 210).unwrap();
        assert_eq!(hole.kind, Kind::Cylinder);
        assert!((hole.params[0].abs() - 17.5).abs() < 1e-9);
        assert_eq!(hole.frame.o, [-325.0, -175.0, 0.0]);
        // 137 body faces, four loose surfaces
        assert_eq!(
            b.surfaces
                .iter()
                .filter(|s| s.list_type == Some(98))
                .count(),
            137
        );
    }

    #[test]
    fn conventions_make_both_sides_of_every_edge_coincide() {
        let b = brep_of_ctc01();
        let fixed = resolve(&b);
        let by_id: HashMap<i64, &Surface> = b.surfaces.iter().map(|s| (s.id, s)).collect();
        let (mut good, mut bad) = (0, 0);
        for c in &b.curves {
            let (Some(a), Some(bb)) = (by_id.get(&c.faces[0]), by_id.get(&c.faces[1])) else {
                continue;
            };
            for r in &c.pts {
                let p = eval(a, &fixed[&a.id], r[0], r[1]);
                let q = eval(bb, &fixed[&bb.id], r[2], r[3]);
                if norm(sub(p, q)) < 1e-3 {
                    good += 1;
                } else {
                    bad += 1;
                }
            }
        }
        // the prototype gets every one of the 1437 samples
        assert_eq!((good, bad), (1437, 0));
    }
}
