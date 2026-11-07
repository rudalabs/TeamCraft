//! Refines a coarse tessellation along the true surface: curved
//! point-normal triangles (Vlachos et al., "Curved PN Triangles", 2001).
//!
//! Input are triangles whose corners carry the exact surface normal of the
//! B-rep (JT stores them). Every edge gets one cubic curve that both of its
//! triangles share, so the refined mesh stays watertight:
//!
//! * same normal on both sides at an end (smooth surface): the end tangent
//!   is the chord projected into the tangent plane (PN triangles);
//! * two normals at an end (a crease, e.g. the circular edge where a
//!   cylinder meets a plane): the edge lies on both surfaces, so its tangent
//!   is the cross product of the two normals;
//! * three or more triangles at an edge, or tangents that make no sense:
//!   the edge stays straight.
//!
//! Each triangle is then split into `level²` triangles on the cubic Bézier
//! patch through its edge curves. On the cylinders of an NX part this cuts
//! the deviation from the true surface from tenths of a millimetre to a few
//! micrometres.

use std::collections::HashMap;

type P = [f64; 3];

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn mul(a: P, s: f64) -> P {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: P, b: P) -> P {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: P) -> f64 {
    dot(a, a).sqrt()
}

fn same(a: P, b: P) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= 1e-6)
}

/// Control point next to `pa` on the edge `pa → pb`; `normals` are the
/// normals at `pa` on each side of the edge (empty: straight edge).
fn handle(pa: P, pb: P, normals: &[P]) -> P {
    let chord = sub(pb, pa);
    let straight = add(pa, mul(chord, 1.0 / 3.0));
    let pn = |n: P| sub(straight, mul(n, dot(chord, n) / 3.0));
    match *normals {
        [n] => pn(n),
        [a, b] if same(a, b) => pn(a),
        [a, b] => {
            let t = cross(a, b);
            let lt = length(t);
            if lt < 1e-3 {
                // nearly tangent sides: their mean normal
                let n = add(a, b);
                let ln = length(n);
                return if ln > 0.0 {
                    pn(mul(n, 1.0 / ln))
                } else {
                    straight
                };
            }
            let t = mul(t, 1.0 / lt);
            let d = dot(chord, t);
            if d.abs() < 0.5 * length(chord) {
                straight
            } else {
                add(pa, mul(t, d / 3.0))
            }
        }
        _ => straight,
    }
}

/// The three edges of a triangle, in order.
fn edges(t: &[u32; 3]) -> [(u32, u32); 3] {
    let [a, b, c] = *t;
    [(a, b), (b, c), (c, a)]
}

fn key(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Splits every triangle into `level²` triangles on its PN patch. Points
/// and normals are indexed by `triangles` / `normal_triangles` (checked by
/// the caller); the original points keep their indices.
pub(crate) fn refine(
    points: &[P],
    triangles: &[[u32; 3]],
    normals: &[P],
    normal_triangles: &[[u32; 3]],
    level: usize,
) -> (Vec<P>, Vec<[u32; 3]>) {
    let point = |i: u32| points.get(i as usize).copied().unwrap_or_default();
    let normal = |i: u32| normals.get(i as usize).copied().unwrap_or_default();
    if level < 2 {
        return (points.to_vec(), triangles.to_vec());
    }
    // normals at both ends of every edge, one pair per incident triangle
    // (stored low index first)
    let mut sides: HashMap<(u32, u32), Vec<(P, P)>> = HashMap::new();
    for (t, n) in triangles.iter().zip(normal_triangles) {
        for ((a, b), (na, nb)) in edges(t).into_iter().zip(edges(n)) {
            let (na, nb) = (normal(na), normal(nb));
            let pair = if a < b { (na, nb) } else { (nb, na) };
            sides.entry(key(a, b)).or_default().push(pair);
        }
    }
    // canonical control points of every edge, low index first
    let mut controls: HashMap<(u32, u32), (P, P)> = HashMap::with_capacity(sides.len());
    for (&(a, b), pairs) in &sides {
        let (at_a, at_b): (Vec<P>, Vec<P>) = if pairs.len() > 2 {
            (Vec::new(), Vec::new())
        } else {
            pairs.iter().copied().unzip()
        };
        let (pa, pb) = (point(a), point(b));
        controls.insert((a, b), (handle(pa, pb, &at_a), handle(pb, pa, &at_b)));
    }
    let directed = |a: u32, b: u32| -> (P, P) {
        let (pa, pb) = (point(a), point(b));
        let fallback = (
            add(pa, mul(sub(pb, pa), 1.0 / 3.0)),
            add(pb, mul(sub(pa, pb), 1.0 / 3.0)),
        );
        match controls.get(&key(a, b)) {
            Some(&(c1, c2)) if a < b => (c1, c2),
            Some(&(c1, c2)) => (c2, c1),
            None => fallback,
        }
    };

    let n = level;
    let nf = n as f64;
    let mut out_points = points.to_vec();
    let mut out_triangles = Vec::with_capacity(triangles.len() * n * n);
    let mut edge_points: HashMap<(u32, u32, usize), u32> = HashMap::new();
    let mut edge_point = |out: &mut Vec<P>, a: u32, b: u32, i: usize| -> u32 {
        let (a, b, i) = if a < b { (a, b, i) } else { (b, a, n - i) };
        *edge_points.entry((a, b, i)).or_insert_with(|| {
            let (pa, pb) = (point(a), point(b));
            let (c1, c2) = directed(a, b);
            let t = i as f64 / nf;
            let s = 1.0 - t;
            let p = add(
                add(mul(pa, s * s * s), mul(c1, 3.0 * s * s * t)),
                add(mul(c2, 3.0 * s * t * t), mul(pb, t * t * t)),
            );
            out.push(p);
            (out.len() - 1) as u32
        })
    };

    let mut grid = vec![0u32; (n + 1) * (n + 1)];
    for t in triangles {
        let [i0, i1, i2] = *t;
        let p = [point(i0), point(i1), point(i2)];
        let (b01, b10) = directed(i0, i1);
        let (b12, b21) = directed(i1, i2);
        let (b20, b02) = directed(i2, i0);
        let e = mul(
            add(add(add(b01, b10), add(b12, b21)), add(b20, b02)),
            1.0 / 6.0,
        );
        let v = mul(add(add(p[0], p[1]), p[2]), 1.0 / 3.0);
        let b111 = add(e, mul(sub(e, v), 0.5));
        // grid (i, j): barycentric u = i/n towards corner 1, v = j/n towards corner 2
        for i in 0..=n {
            for j in 0..=(n - i) {
                let k = n - i - j;
                let index = if i == 0 && j == 0 {
                    i0
                } else if i == n {
                    i1
                } else if j == n {
                    i2
                } else if j == 0 {
                    edge_point(&mut out_points, i0, i1, i)
                } else if i == 0 {
                    edge_point(&mut out_points, i0, i2, j)
                } else if k == 0 {
                    edge_point(&mut out_points, i1, i2, j)
                } else {
                    let (u, w, x) = (i as f64 / nf, j as f64 / nf, k as f64 / nf);
                    let terms = [
                        (p[0], x * x * x),
                        (p[1], u * u * u),
                        (p[2], w * w * w),
                        (b01, 3.0 * x * x * u),
                        (b10, 3.0 * x * u * u),
                        (b12, 3.0 * u * u * w),
                        (b21, 3.0 * u * w * w),
                        (b20, 3.0 * w * w * x),
                        (b02, 3.0 * w * x * x),
                        (b111, 6.0 * u * w * x),
                    ];
                    let q = terms
                        .iter()
                        .fold([0.0; 3], |acc, &(c, f)| add(acc, mul(c, f)));
                    out_points.push(q);
                    (out_points.len() - 1) as u32
                };
                if let Some(cell) = grid.get_mut(i * (n + 1) + j) {
                    *cell = index;
                }
            }
        }
        let at = |i: usize, j: usize| grid.get(i * (n + 1) + j).copied().unwrap_or(i0);
        for i in 0..n {
            for j in 0..(n - i) {
                out_triangles.push([at(i, j), at(i + 1, j), at(i, j + 1)]);
                if i + j + 1 < n {
                    out_triangles.push([at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
        }
    }
    (out_points, out_triangles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// A closed cylinder of radius 1 along z with `segments` facets around
    /// and flat caps (separate cap normals, so the rims are creases).
    /// Points, triangles, normals, normal triangles.
    type Shape = (Vec<P>, Vec<[u32; 3]>, Vec<P>, Vec<[u32; 3]>);

    fn cylinder(segments: u32) -> Shape {
        let mut points = Vec::new();
        let mut normals = Vec::new();
        for z in [0.0, 1.0] {
            for s in 0..segments {
                let a = 2.0 * PI * f64::from(s) / f64::from(segments);
                points.push([a.cos(), a.sin(), z]);
                normals.push([a.cos(), a.sin(), 0.0]);
            }
        }
        let (bottom, top) = (points.len() as u32, points.len() as u32 + 1);
        points.push([0.0, 0.0, 0.0]);
        points.push([0.0, 0.0, 1.0]);
        let (down, up) = (normals.len() as u32, normals.len() as u32 + 1);
        normals.push([0.0, 0.0, -1.0]);
        normals.push([0.0, 0.0, 1.0]);
        let (mut tris, mut ntris) = (Vec::new(), Vec::new());
        for s in 0..segments {
            let s1 = (s + 1) % segments;
            let (a, b, c, d) = (s, s1, s + segments, s1 + segments);
            tris.push([a, b, d]);
            ntris.push([a, b, d]);
            tris.push([a, d, c]);
            ntris.push([a, d, c]);
            tris.push([bottom, b, a]);
            ntris.push([down, down, down]);
            tris.push([top, c, d]);
            ntris.push([up, up, up]);
        }
        (points, tris, normals, ntris)
    }

    fn worst_radius_error(points: &[P]) -> f64 {
        points
            .iter()
            // the wall and the rims (the cap interiors lie inside the circle)
            .filter(|p| p[0].hypot(p[1]) > 0.9)
            .map(|p| (p[0].hypot(p[1]) - 1.0).abs())
            .fold(0.0, f64::max)
    }

    fn closed(triangles: &[[u32; 3]]) -> bool {
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for t in triangles {
            for e in super::edges(t) {
                *edges.entry(e).or_default() += 1;
            }
        }
        edges
            .iter()
            .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1))
    }

    #[test]
    fn refinement_follows_the_cylinder_and_stays_closed() {
        let (points, tris, normals, ntris) = cylinder(16);
        assert!(closed(&tris));
        let (flat, flat_tris) = refine(&points, &tris, &[], &tris, 3);
        let (curved, curved_tris) = refine(&points, &tris, &normals, &ntris, 3);
        assert_eq!(curved_tris.len(), tris.len() * 9);
        assert!(closed(&curved_tris), "edge points are shared");
        assert!(closed(&flat_tris));
        let chord = worst_radius_error(&flat);
        let pn = worst_radius_error(&curved);
        // 16 segments: the chord misses the circle by 1 - cos(π/16) ≈ 0.019
        assert!(chord > 0.015, "{chord}");
        assert!(pn < chord / 20.0, "PN {pn} vs chord {chord}");
        // the caps stay flat
        assert!(curved
            .iter()
            .filter(|p| p[0].hypot(p[1]) < 0.95)
            .all(|p| p[2].abs() < 1e-9 || (p[2] - 1.0).abs() < 1e-9));
    }

    #[test]
    fn level_one_and_missing_normals_change_nothing() {
        let (points, tris, normals, ntris) = cylinder(8);
        assert_eq!(
            refine(&points, &tris, &normals, &ntris, 1),
            (points.clone(), tris.clone())
        );
        let (p, t) = refine(&points, &tris, &[], &tris, 2);
        assert_eq!(t.len(), tris.len() * 4);
        assert!(p.iter().all(|q| q.iter().all(|c| c.is_finite())));
    }
}
