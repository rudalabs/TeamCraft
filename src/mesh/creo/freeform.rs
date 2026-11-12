//! The freeform surfaces of the Creo B-rep: interpolating bicubic splines
//! (`splsrf`), rolling-ball fillets (`fillet_srf`) and ruled surfaces
//! (`ruled_srf`). All three carry absolute 3D data, so they need no
//! convention and even seed the resolution of the analytic faces.
//!
//! Curves and grids are clamped C² cubic splines through their points with
//! the stored end derivatives (de Boor), evaluated as piecewise cubic
//! Hermite segments.

use super::brep::{cross, dot, lin, norm, scale, sub, Frame, P3};
use super::tagged::Tree;

/// Largest number of knots of one spline direction.
const MAX_KNOTS: usize = 10_000;

/// A clamped C² cubic spline through points at knots.
#[derive(Debug, Clone)]
pub struct Spline {
    pub t: Vec<f64>,
    pub p: Vec<P3>,
    /// First derivatives at the knots (with respect to the real parameter).
    pub d: Vec<P3>,
}

/// Derivatives of a clamped C² cubic spline at its knots (tridiagonal
/// system, Thomas algorithm). `d0`/`d1` are the end derivatives.
fn spline_derivs(t: &[f64], p: &[P3], d0: P3, d1: P3) -> Vec<P3> {
    let n = t.len();
    match n {
        0 => return Vec::new(),
        1 => return vec![d1],
        2 => return vec![d0, d1],
        _ => {}
    }
    let h: Vec<f64> = t.iter().zip(t.iter().skip(1)).map(|(a, b)| b - a).collect();
    let m = n - 2;
    let hk = |i: usize| h.get(i).copied().unwrap_or(1.0);
    let pk = |i: usize| p.get(i).copied().unwrap_or_default();
    // one row per interior knot i = k + 1:
    // lower·d[i−1] + diag·d[i] + upper·d[i+1] = rhs
    let mut rows: Vec<(f64, f64, f64, P3)> = Vec::with_capacity(m);
    for k in 0..m {
        let i = k + 1;
        let (h0, h1) = (hk(i - 1), hk(i));
        let mut rhs = scale(
            3.0,
            lin(
                scale(h1 / h0, sub(pk(i), pk(i - 1))),
                h0 / h1,
                sub(pk(i + 1), pk(i)),
            ),
        );
        // the known end derivatives move to the right-hand side
        if k == 0 {
            rhs = lin(rhs, -h1, d0);
        }
        if k + 1 == m {
            rhs = lin(rhs, -h0, d1);
        }
        let lower = if k > 0 { h1 } else { 0.0 };
        let upper = if k + 1 < m { h0 } else { 0.0 };
        rows.push((lower, 2.0 * (h0 + h1), upper, rhs));
    }
    // Thomas algorithm: forward elimination …
    let mut fwd: Vec<(f64, P3)> = Vec::with_capacity(m);
    let (mut c_prev, mut r_prev) = (0.0, [0.0; 3]);
    for &(lower, diag, upper, rhs) in &rows {
        let mut den = diag - lower * c_prev;
        if den.abs() < 1e-300 {
            den = 1e-300;
        }
        c_prev = upper / den;
        r_prev = scale(1.0 / den, lin(rhs, -lower, r_prev));
        fwd.push((c_prev, r_prev));
    }
    // … and back substitution (the last row has upper = 0)
    let mut x = vec![[0.0; 3]; m];
    let mut next = [0.0; 3];
    for (xk, &(ck, rk)) in x.iter_mut().zip(&fwd).rev() {
        *xk = lin(rk, -ck, next);
        next = *xk;
    }
    let mut d = Vec::with_capacity(n);
    d.push(d0);
    d.extend(x);
    d.push(d1);
    d
}

/// Are the knots strictly increasing?
fn increasing(t: &[f64]) -> bool {
    t.iter().zip(t.iter().skip(1)).all(|(a, b)| b > a)
}

fn hermite(s: f64) -> [f64; 4] {
    let s2 = s * s;
    let s3 = s2 * s;
    [
        2.0 * s3 - 3.0 * s2 + 1.0,
        s3 - 2.0 * s2 + s,
        -2.0 * s3 + 3.0 * s2,
        s3 - s2,
    ]
}

/// Segment index for parameter `x` (clamped to the knot range).
fn segment(t: &[f64], x: f64) -> usize {
    if t.len() < 2 {
        return 0;
    }
    let mut i = t.partition_point(|&k| k <= x);
    i = i.saturating_sub(1);
    i.min(t.len() - 2)
}

impl Spline {
    pub fn new(t: Vec<f64>, p: Vec<P3>, d0: P3, d1: P3) -> Option<Spline> {
        if t.len() != p.len() || t.is_empty() || t.len() > MAX_KNOTS {
            return None;
        }
        if t.iter().chain(p.iter().flatten()).any(|x| !x.is_finite()) {
            return None;
        }
        if !increasing(&t) {
            return None;
        }
        let d = spline_derivs(&t, &p, d0, d1);
        Some(Spline { t, p, d })
    }

    pub fn at(&self, x: f64) -> P3 {
        let i = segment(&self.t, x);
        let (t0, t1) = (
            self.t.get(i).copied().unwrap_or(0.0),
            self.t.get(i + 1).copied().unwrap_or(1.0),
        );
        let h = t1 - t0;
        if h.abs() < 1e-300 || self.t.len() < 2 {
            return self.p.first().copied().unwrap_or_default();
        }
        let s = (x - t0) / h;
        let b = hermite(s);
        let (p0, p1) = (
            self.p.get(i).copied().unwrap_or_default(),
            self.p.get(i + 1).copied().unwrap_or_default(),
        );
        let (d0, d1) = (
            self.d.get(i).copied().unwrap_or_default(),
            self.d.get(i + 1).copied().unwrap_or_default(),
        );
        let out = lin(scale(b[0], p0), b[1] * h, d0);
        lin(lin(out, b[2], p1), b[3] * h, d1)
    }

    pub fn first_knot(&self) -> f64 {
        self.t.first().copied().unwrap_or(0.0)
    }

    pub fn last_knot(&self) -> f64 {
        self.t.last().copied().unwrap_or(1.0)
    }
}

/// Bicubic C² interpolation through a `nu × nv` grid with end tangents and
/// corner twists (`splsrf`).
#[derive(Debug, Clone)]
pub struct Grid {
    tu: Vec<f64>,
    tv: Vec<f64>,
    nv: usize,
    g: Vec<P3>,
    pu: Vec<P3>,
    pv: Vec<P3>,
    puv: Vec<P3>,
}

impl Grid {
    fn idx(&self, i: usize, j: usize) -> usize {
        i * self.nv + j
    }

    pub fn eval(&self, u: f64, v: f64) -> P3 {
        let i = segment(&self.tu, u);
        let j = segment(&self.tv, v);
        let (u0, u1) = (
            self.tu.get(i).copied().unwrap_or(0.0),
            self.tu.get(i + 1).copied().unwrap_or(1.0),
        );
        let (v0, v1) = (
            self.tv.get(j).copied().unwrap_or(0.0),
            self.tv.get(j + 1).copied().unwrap_or(1.0),
        );
        let (hu, hv) = (u1 - u0, v1 - v0);
        if hu.abs() < 1e-300 || hv.abs() < 1e-300 {
            return self.g.get(self.idx(i, j)).copied().unwrap_or_default();
        }
        let bu = hermite((u - u0) / hu);
        let bv = hermite((v - v0) / hv);
        let mut out = [0.0; 3];
        // Hermite basis order: value at 0, derivative at 0, value at 1, derivative at 1
        let corners = [(0, 0), (0, 1), (1, 0), (1, 1)];
        for (wa, (di, du)) in bu.into_iter().zip(corners) {
            for (wb, (dj, dv)) in bv.into_iter().zip(corners) {
                let k = self.idx(i + di, j + dj);
                let q = match (du, dv) {
                    (0, 0) => self.g.get(k).copied().unwrap_or_default(),
                    (1, 0) => self.pu.get(k).copied().unwrap_or_default().map(|x| x * hu),
                    (0, 1) => self.pv.get(k).copied().unwrap_or_default().map(|x| x * hv),
                    _ => self
                        .puv
                        .get(k)
                        .copied()
                        .unwrap_or_default()
                        .map(|x| x * hu * hv),
                };
                out = lin(out, wa * wb, q);
            }
        }
        out
    }
}

/// One freeform surface.
#[derive(Debug, Clone)]
pub enum Freeform {
    Spline(Grid),
    /// Rolling-ball blend: `s = v0 + v (v1 − v0)`, centre `ctr(s)`, radius
    /// vector `pnt(s) − ctr(s)`, circle axis `tan(s)`; `u` is the angle.
    Fillet {
        pnt: Spline,
        ctr: Spline,
        tan: Spline,
        v0: f64,
        v1: f64,
    },
    /// `C1(u) + v (C2(u) − C1(u))`, `u` normalised along both curves.
    Ruled {
        c1: Curve,
        c2: Curve,
    },
}

/// A boundary curve of a ruled surface.
#[derive(Debug, Clone)]
pub enum Curve {
    /// Ellipse `O + c1 cos t X + c2 sin t Y`, `t = t0 + u (t1 − t0)`.
    Conic {
        frame: Frame,
        c1: f64,
        c2: f64,
        t0: f64,
        t1: f64,
    },
    Spline(Spline),
}

impl Curve {
    fn at(&self, u: f64) -> P3 {
        match self {
            Curve::Conic {
                frame,
                c1,
                c2,
                t0,
                t1,
            } => {
                let t = t0 + u * (t1 - t0);
                lin(lin(frame.o, c1 * t.cos(), frame.x), c2 * t.sin(), frame.y)
            }
            Curve::Spline(s) => s.at(s.first_knot() + u * (s.last_knot() - s.first_knot())),
        }
    }
}

impl Freeform {
    pub fn eval(&self, u: f64, v: f64) -> P3 {
        match self {
            Freeform::Spline(g) => g.eval(u, v),
            Freeform::Fillet {
                pnt,
                ctr,
                tan,
                v0,
                v1,
            } => {
                let s = v0 + v * (v1 - v0);
                let c = ctr.at(s);
                let a = sub(pnt.at(s), c);
                let r2 = dot(a, a);
                let mut t = tan.at(s);
                if r2 > 1e-300 {
                    t = lin(t, -dot(t, a) / r2, a);
                }
                let lt = norm(t);
                if lt > 1e-300 {
                    t = t.map(|x| x / lt);
                }
                let b = cross(t, a);
                let (cu, su) = (u.cos(), u.sin());
                lin(lin(c, cu, a), su, b)
            }
            Freeform::Ruled { c1, c2 } => {
                let a = c1.at(u);
                let b = c2.at(u);
                lin(a, v, sub(b, a))
            }
        }
    }
}

fn p3s(tree: &Tree, node: usize) -> Vec<P3> {
    tree.rows(node)
        .into_iter()
        .filter_map(|r| <P3>::try_from(r.as_slice()).ok())
        .collect()
}

fn spline_of(tree: &Tree, node: usize) -> Option<Spline> {
    let t = tree.dbls(tree.child(node, "params")?);
    let p = p3s(tree, tree.child(node, "i_pnts")?);
    let e = p3s(tree, tree.child(node, "end_tangts")?);
    Spline::new(
        t,
        p,
        e.first().copied().unwrap_or_default(),
        e.get(1).copied().unwrap_or_default(),
    )
}

/// Reads a freeform surface from its `srf_prim_ptr(<kind>)` node.
pub fn read(tree: &Tree, kind: &str, prim: usize) -> Option<Freeform> {
    match kind {
        "splsrf" => {
            let tu = tree.dbls(tree.child(prim, "u_params")?);
            let tv = tree.dbls(tree.child(prim, "v_params")?);
            let (nu, nv) = (tu.len(), tv.len());
            if nu < 2
                || nv < 2
                || nu > MAX_KNOTS
                || nv > MAX_KNOTS
                || nu.checked_mul(nv)? > 4_000_000
            {
                return None;
            }
            let g = p3s(tree, tree.child(prim, "i_points")?);
            let eu = p3s(tree, tree.child(prim, "end_u_tangts")?);
            let ev = p3s(tree, tree.child(prim, "end_v_tangts")?);
            let tw = p3s(tree, tree.child(prim, "end_uv_deriv")?);
            if g.len() != nu * nv || eu.len() != 2 * nv || ev.len() != 2 * nu || tw.len() != 4 {
                return None;
            }
            if !increasing(&tu) || !increasing(&tv) {
                return None;
            }
            if g.iter()
                .chain(eu.iter())
                .chain(ev.iter())
                .chain(tw.iter())
                .flatten()
                .any(|x| !x.is_finite())
            {
                return None;
            }
            // grid value at (i, j); the sizes were checked above
            let at = |v: &[P3], i: usize, j: usize| v.get(i * nv + j).copied().unwrap_or_default();
            let get = |v: &[P3], k: usize| v.get(k).copied().unwrap_or_default();
            let put = |v: &mut Vec<P3>, i: usize, j: usize, x: P3| {
                if let Some(slot) = v.get_mut(i * nv + j) {
                    *slot = x;
                }
            };
            let mut pu = vec![[0.0; 3]; nu * nv];
            let mut pv = vec![[0.0; 3]; nu * nv];
            let mut puv = vec![[0.0; 3]; nu * nv];
            for j in 0..nv {
                let col: Vec<P3> = (0..nu).map(|i| at(&g, i, j)).collect();
                let d = spline_derivs(&tu, &col, get(&eu, j), get(&eu, nv + j));
                for (i, x) in d.into_iter().enumerate() {
                    put(&mut pu, i, j, x);
                }
            }
            for i in 0..nu {
                let row: Vec<P3> = (0..nv).map(|j| at(&g, i, j)).collect();
                let d = spline_derivs(&tv, &row, get(&ev, i), get(&ev, nu + i));
                for (j, x) in d.into_iter().enumerate() {
                    put(&mut pv, i, j, x);
                }
            }
            // twists along the first and last u rows, then across u
            for (e, i) in [(0usize, 0usize), (1, nu - 1)] {
                let row: Vec<P3> = (0..nv).map(|j| at(&pu, i, j)).collect();
                let d = spline_derivs(&tv, &row, get(&tw, e * 2), get(&tw, e * 2 + 1));
                for (j, x) in d.into_iter().enumerate() {
                    put(&mut puv, i, j, x);
                }
            }
            for j in 0..nv {
                let col: Vec<P3> = (0..nu).map(|i| at(&pv, i, j)).collect();
                let d = spline_derivs(&tu, &col, at(&puv, 0, j), at(&puv, nu - 1, j));
                for (i, x) in d.into_iter().enumerate() {
                    put(&mut puv, i, j, x);
                }
            }
            Some(Freeform::Spline(Grid {
                tu,
                tv,
                nv,
                g,
                pu,
                pv,
                puv,
            }))
        }
        "fillet_srf" => {
            let pnt = spline_of(tree, tree.child(prim, "pnt_spline")?)?;
            let ctr = spline_of(tree, tree.child(prim, "ctr_spline")?)?;
            let tan = spline_of(tree, tree.child(prim, "tan_spline")?)?;
            let v0 = tree.dbl_at(prim, "par_v_0")?;
            let v1 = tree.dbl_at(prim, "par_v_1")?;
            if !(v0.is_finite() && v1.is_finite()) {
                return None;
            }
            Some(Freeform::Fillet {
                pnt,
                ctr,
                tan,
                v0,
                v1,
            })
        }
        "ruled_srf" => {
            let c1 = ruled_curve(tree, prim, "curve_1")?;
            let c2 = ruled_curve(tree, prim, "curve_2")?;
            Some(Freeform::Ruled { c1, c2 })
        }
        _ => None,
    }
}

fn ruled_curve(tree: &Tree, prim: usize, key: &str) -> Option<Curve> {
    let (name, node) = tree
        .children_with_prefix(prim, &format!("{key}("))
        .into_iter()
        .next()?;
    let ty = name
        .trim_start_matches(key)
        .trim_start_matches('(')
        .trim_end_matches(')');
    match ty {
        "conic" => {
            let rows = tree.rows(tree.child(node, "local_sys")?);
            let frame = super::brep::frame_of(&rows)?;
            let c1 = tree.dbl_at(node, "c1")?;
            let c2 = tree.dbl_at(node, "c2")?;
            let t0 = tree.dbl_at(node, "t0")?;
            let t1 = tree.dbl_at(node, "t1")?;
            if ![c1, c2, t0, t1].iter().all(|x| x.is_finite()) {
                return None;
            }
            Some(Curve::Conic {
                frame,
                c1,
                c2,
                t0,
                t1,
            })
        }
        "spline" => Some(Curve::Spline(spline_of(tree, node)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clamped_spline_interpolates_its_points_and_end_tangents() {
        let t = vec![0.0, 1.0, 2.0, 3.0];
        let p: Vec<P3> = t.iter().map(|&x| [x, x * x, 0.0]).collect();
        let s = Spline::new(t.clone(), p.clone(), [1.0, 0.0, 0.0], [1.0, 6.0, 0.0]).unwrap();
        for (x, q) in t.iter().zip(&p) {
            let r = s.at(*x);
            assert!(norm(sub(r, *q)) < 1e-12);
        }
        // a parabola with exact end derivatives is reproduced exactly
        let r = s.at(1.5);
        assert!((r[1] - 2.25).abs() < 1e-9, "{r:?}");
        assert_eq!(segment(&t, -1.0), 0);
        assert_eq!(segment(&t, 9.0), 2);
    }
}
