//! Triangulation of a planar polygon with holes by ear clipping.
//!
//! A port of the earcut algorithm (mapbox/earcut, ISC licence,
//! Copyright (c) 2016 Mapbox) without its z-order hash: the outer ring and
//! the holes form one circular doubly linked list (holes are bridged to the
//! outer ring from their leftmost vertex), ears are clipped, and rings that
//! cannot be clipped are first cleaned of collinear and duplicate points,
//! then cured of local self-intersections and finally split in two.
//!
//! The nodes live in one arena and are addressed by index; every access goes
//! through `get`, so a broken invariant ends the triangulation instead of
//! panicking. A step budget bounds the work on degenerate input.

/// One ring vertex in the arena.
#[derive(Clone, Copy, Debug, Default)]
struct Node {
    /// Index of the vertex in the caller's coordinate list.
    i: usize,
    x: f64,
    y: f64,
    prev: usize,
    next: usize,
    /// A single-point hole (kept even when it looks degenerate).
    steiner: bool,
}

struct Arena {
    nodes: Vec<Node>,
    steps: usize,
}

/// Triangulates the polygon `coords` (x, y per vertex). `holes` are the
/// indices of the first vertex of each hole; the outer ring comes first.
/// Returns three vertex indices per triangle (any orientation).
pub(crate) fn earcut(coords: &[[f64; 2]], holes: &[usize]) -> Vec<[usize; 3]> {
    let mut tris = Vec::new();
    if coords.len() < 3 || coords.iter().flatten().any(|c| !c.is_finite()) {
        return tris;
    }
    let mut a = Arena {
        nodes: Vec::with_capacity(coords.len() * 3 / 2 + 8),
        // generous: earcut is quadratic in the worst case
        steps: coords
            .len()
            .saturating_mul(coords.len())
            .saturating_mul(8)
            .saturating_add(10_000),
    };
    let outer_end = holes
        .first()
        .copied()
        .unwrap_or(coords.len())
        .min(coords.len());
    let Some(mut outer) = a.linked_list(coords, 0, outer_end, true) else {
        return tris;
    };
    if a.next(outer) == a.prev(outer) {
        return tris;
    }
    if !holes.is_empty() {
        outer = a.eliminate_holes(coords, holes, outer);
    }
    a.earcut_linked(Some(outer), &mut tris, 0);
    tris
}

fn signed_area(coords: &[[f64; 2]], start: usize, end: usize) -> f64 {
    let ring = coords.get(start..end).unwrap_or_default();
    let mut sum = 0.0;
    let mut j = ring.len().wrapping_sub(1);
    for (i, p) in ring.iter().enumerate() {
        if let Some(q) = ring.get(j) {
            sum += (q[0] - p[0]) * (p[1] + q[1]);
        }
        j = i;
    }
    sum
}

fn point_in_triangle(a: [f64; 2], b: [f64; 2], c: [f64; 2], p: [f64; 2]) -> bool {
    (c[0] - p[0]) * (a[1] - p[1]) >= (a[0] - p[0]) * (c[1] - p[1])
        && (a[0] - p[0]) * (b[1] - p[1]) >= (b[0] - p[0]) * (a[1] - p[1])
        && (b[0] - p[0]) * (c[1] - p[1]) >= (c[0] - p[0]) * (b[1] - p[1])
}

fn sign(v: f64) -> i8 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

impl Arena {
    fn n(&self, k: usize) -> Node {
        self.nodes.get(k).copied().unwrap_or_default()
    }
    fn next(&self, k: usize) -> usize {
        self.n(k).next
    }
    fn prev(&self, k: usize) -> usize {
        self.n(k).prev
    }
    fn xy(&self, k: usize) -> [f64; 2] {
        let n = self.n(k);
        [n.x, n.y]
    }
    fn set_next(&mut self, k: usize, v: usize) {
        if let Some(n) = self.nodes.get_mut(k) {
            n.next = v;
        }
    }
    fn set_prev(&mut self, k: usize, v: usize) {
        if let Some(n) = self.nodes.get_mut(k) {
            n.prev = v;
        }
    }
    /// One unit of work; false once the budget is spent.
    fn step(&mut self) -> bool {
        self.steps = self.steps.saturating_sub(1);
        self.steps > 0
    }

    fn equals(&self, p: usize, q: usize) -> bool {
        self.xy(p) == self.xy(q)
    }

    /// Twice the signed area of the triangle (p, q, r), earcut's sign.
    fn area(&self, p: usize, q: usize, r: usize) -> f64 {
        let (p, q, r) = (self.xy(p), self.xy(q), self.xy(r));
        (q[1] - p[1]) * (r[0] - q[0]) - (q[0] - p[0]) * (r[1] - q[1])
    }

    fn insert(&mut self, i: usize, p: [f64; 2], last: Option<usize>) -> usize {
        let k = self.nodes.len();
        let mut node = Node {
            i,
            x: p[0],
            y: p[1],
            prev: k,
            next: k,
            steiner: false,
        };
        if let Some(last) = last {
            let after = self.next(last);
            node.next = after;
            node.prev = last;
            self.set_prev(after, k);
            self.set_next(last, k);
        }
        self.nodes.push(node);
        k
    }

    fn remove(&mut self, p: usize) {
        let (prev, next) = (self.prev(p), self.next(p));
        self.set_prev(next, prev);
        self.set_next(prev, next);
    }

    fn linked_list(
        &mut self,
        coords: &[[f64; 2]],
        start: usize,
        end: usize,
        clockwise: bool,
    ) -> Option<usize> {
        let mut last = None;
        let range = coords.get(start..end)?;
        if clockwise == (signed_area(coords, start, end) > 0.0) {
            for (k, p) in range.iter().enumerate() {
                last = Some(self.insert(start + k, *p, last));
            }
        } else {
            for (k, p) in range.iter().enumerate().rev() {
                last = Some(self.insert(start + k, *p, last));
            }
        }
        let last = last?;
        if self.equals(last, self.next(last)) {
            let next = self.next(last);
            self.remove(last);
            return Some(next);
        }
        Some(last)
    }

    /// Removes duplicate and collinear points between `start` and `end`.
    fn filter_points(&mut self, start: usize, end: Option<usize>) -> usize {
        let mut end = end.unwrap_or(start);
        let mut p = start;
        loop {
            if !self.step() {
                return end;
            }
            let mut again = false;
            let (pn, pp) = (self.next(p), self.prev(p));
            if !self.n(p).steiner && (self.equals(p, pn) || self.area(pp, p, pn) == 0.0) {
                self.remove(p);
                p = pp;
                end = pp;
                if p == self.next(p) {
                    break;
                }
                again = true;
            } else {
                p = pn;
            }
            if !again && p == end {
                break;
            }
        }
        end
    }

    fn earcut_linked(&mut self, ear: Option<usize>, tris: &mut Vec<[usize; 3]>, pass: u8) {
        let Some(mut ear) = ear else { return };
        let mut stop = ear;
        while self.prev(ear) != self.next(ear) {
            if !self.step() {
                return;
            }
            let (prev, next) = (self.prev(ear), self.next(ear));
            if self.is_ear(ear) {
                tris.push([self.n(prev).i, self.n(ear).i, self.n(next).i]);
                self.remove(ear);
                ear = self.next(next);
                stop = ear;
                continue;
            }
            ear = next;
            if ear == stop {
                match pass {
                    0 => {
                        let e = self.filter_points(ear, None);
                        self.earcut_linked(Some(e), tris, 1);
                    }
                    1 => {
                        let e = self.filter_points(ear, None);
                        let e = self.cure_local_intersections(e, tris);
                        self.earcut_linked(Some(e), tris, 2);
                    }
                    _ => self.split_earcut(ear, tris),
                }
                return;
            }
        }
    }

    fn is_ear(&mut self, ear: usize) -> bool {
        let (a, b, c) = (self.prev(ear), ear, self.next(ear));
        if self.area(a, b, c) >= 0.0 {
            return false; // reflex
        }
        let (pa, pb, pc) = (self.xy(a), self.xy(b), self.xy(c));
        let (x0, x1) = (pa[0].min(pb[0]).min(pc[0]), pa[0].max(pb[0]).max(pc[0]));
        let (y0, y1) = (pa[1].min(pb[1]).min(pc[1]), pa[1].max(pb[1]).max(pc[1]));
        let mut p = self.next(c);
        while p != a {
            if !self.step() {
                return false;
            }
            let pp = self.xy(p);
            if pp[0] >= x0
                && pp[0] <= x1
                && pp[1] >= y0
                && pp[1] <= y1
                && pp != pa
                && point_in_triangle(pa, pb, pc, pp)
                && self.area(self.prev(p), p, self.next(p)) >= 0.0
            {
                return false;
            }
            p = self.next(p);
        }
        true
    }

    fn cure_local_intersections(&mut self, start: usize, tris: &mut Vec<[usize; 3]>) -> usize {
        let mut start = start;
        let mut p = start;
        loop {
            if !self.step() {
                break;
            }
            let a = self.prev(p);
            let b = self.next(self.next(p));
            if !self.equals(a, b)
                && self.intersects(a, p, self.next(p), b)
                && self.locally_inside(a, b)
                && self.locally_inside(b, a)
            {
                tris.push([self.n(a).i, self.n(p).i, self.n(b).i]);
                let pn = self.next(p);
                self.remove(p);
                self.remove(pn);
                p = b;
                start = b;
            }
            p = self.next(p);
            if p == start {
                break;
            }
        }
        self.filter_points(p, None)
    }

    fn split_earcut(&mut self, start: usize, tris: &mut Vec<[usize; 3]>) {
        let mut a = start;
        loop {
            let mut b = self.next(self.next(a));
            while b != self.prev(a) {
                if !self.step() {
                    return;
                }
                if self.n(a).i != self.n(b).i && self.is_valid_diagonal(a, b) {
                    let c = self.split_polygon(a, b);
                    let a2 = self.filter_points(a, Some(self.next(a)));
                    let c2 = self.filter_points(c, Some(self.next(c)));
                    self.earcut_linked(Some(a2), tris, 0);
                    self.earcut_linked(Some(c2), tris, 0);
                    return;
                }
                b = self.next(b);
            }
            a = self.next(a);
            if a == start {
                return;
            }
        }
    }

    fn eliminate_holes(&mut self, coords: &[[f64; 2]], holes: &[usize], outer: usize) -> usize {
        let mut queue = Vec::new();
        for (k, &start) in holes.iter().enumerate() {
            let end = holes
                .get(k + 1)
                .copied()
                .unwrap_or(coords.len())
                .min(coords.len());
            if start >= end {
                continue;
            }
            let Some(list) = self.linked_list(coords, start, end, false) else {
                continue;
            };
            if list == self.next(list) {
                if let Some(n) = self.nodes.get_mut(list) {
                    n.steiner = true;
                }
            }
            queue.push(self.leftmost(list));
        }
        queue.sort_by(|&p, &q| {
            let (a, b) = (self.n(p), self.n(q));
            a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y))
        });
        let mut outer = outer;
        for hole in queue {
            outer = self.eliminate_hole(hole, outer);
        }
        outer
    }

    fn eliminate_hole(&mut self, hole: usize, outer: usize) -> usize {
        let Some(bridge) = self.find_hole_bridge(hole, outer) else {
            return outer;
        };
        let reverse = self.split_polygon(bridge, hole);
        self.filter_points(reverse, Some(self.next(reverse)));
        self.filter_points(bridge, Some(self.next(bridge)))
    }

    /// David Eberly's algorithm: the outer vertex a hole is bridged to.
    fn find_hole_bridge(&mut self, hole: usize, outer: usize) -> Option<usize> {
        let [hx, hy] = self.xy(hole);
        let mut qx = f64::NEG_INFINITY;
        let mut m = None;
        let mut p = outer;
        if self.equals(hole, p) {
            return Some(p);
        }
        loop {
            if !self.step() {
                return None;
            }
            let pn = self.next(p);
            if self.equals(hole, pn) {
                return Some(pn);
            }
            let ([px, py], [nx, ny]) = (self.xy(p), self.xy(pn));
            if hy <= py && hy >= ny && ny != py {
                let x = px + (hy - py) * (nx - px) / (ny - py);
                if x <= hx && x > qx {
                    qx = x;
                    m = Some(if px < nx { p } else { pn });
                    if x == hx {
                        return m;
                    }
                }
            }
            p = pn;
            if p == outer {
                break;
            }
        }
        let mut m = m?;
        let stop = m;
        let [mx, my] = self.xy(m);
        let mut tan_min = f64::INFINITY;
        p = m;
        loop {
            if !self.step() {
                return Some(m);
            }
            let [px, py] = self.xy(p);
            let (ax, cx) = if hy < my { (hx, qx) } else { (qx, hx) };
            if hx >= px
                && px >= mx
                && hx != px
                && point_in_triangle([ax, hy], [mx, my], [cx, hy], [px, py])
            {
                let tan = (hy - py).abs() / (hx - px);
                if self.locally_inside(p, hole)
                    && (tan < tan_min
                        || (tan == tan_min
                            && (px > self.n(m).x
                                || (px == self.n(m).x && self.sector_contains_sector(m, p)))))
                {
                    m = p;
                    tan_min = tan;
                }
            }
            p = self.next(p);
            if p == stop {
                break;
            }
        }
        Some(m)
    }

    fn sector_contains_sector(&self, m: usize, p: usize) -> bool {
        self.area(self.prev(m), m, self.prev(p)) < 0.0
            && self.area(self.next(p), m, self.next(m)) < 0.0
    }

    fn leftmost(&mut self, start: usize) -> usize {
        let mut p = start;
        let mut best = start;
        loop {
            if !self.step() {
                return best;
            }
            let (a, b) = (self.n(p), self.n(best));
            if a.x < b.x || (a.x == b.x && a.y < b.y) {
                best = p;
            }
            p = self.next(p);
            if p == start {
                return best;
            }
        }
    }

    fn is_valid_diagonal(&mut self, a: usize, b: usize) -> bool {
        let (an, ap) = (self.next(a), self.prev(a));
        let (bn, bp) = (self.next(b), self.prev(b));
        self.n(an).i != self.n(b).i
            && self.n(ap).i != self.n(b).i
            && !self.intersects_polygon(a, b)
            && ((self.locally_inside(a, b)
                && self.locally_inside(b, a)
                && self.middle_inside(a, b)
                && (self.area(ap, a, bp) != 0.0 || self.area(a, bp, b) != 0.0))
                || (self.equals(a, b) && self.area(ap, a, an) > 0.0 && self.area(bp, b, bn) > 0.0))
    }

    fn on_segment(&self, p: usize, q: usize, r: usize) -> bool {
        let (p, q, r) = (self.xy(p), self.xy(q), self.xy(r));
        q[0] <= p[0].max(r[0])
            && q[0] >= p[0].min(r[0])
            && q[1] <= p[1].max(r[1])
            && q[1] >= p[1].min(r[1])
    }

    fn intersects(&self, p1: usize, q1: usize, p2: usize, q2: usize) -> bool {
        let o1 = sign(self.area(p1, q1, p2));
        let o2 = sign(self.area(p1, q1, q2));
        let o3 = sign(self.area(p2, q2, p1));
        let o4 = sign(self.area(p2, q2, q1));
        (o1 != o2 && o3 != o4)
            || (o1 == 0 && self.on_segment(p1, p2, q1))
            || (o2 == 0 && self.on_segment(p1, q2, q1))
            || (o3 == 0 && self.on_segment(p2, p1, q2))
            || (o4 == 0 && self.on_segment(p2, q1, q2))
    }

    fn intersects_polygon(&mut self, a: usize, b: usize) -> bool {
        let (ai, bi) = (self.n(a).i, self.n(b).i);
        let mut p = a;
        loop {
            if !self.step() {
                return true;
            }
            let pn = self.next(p);
            let (pi, pni) = (self.n(p).i, self.n(pn).i);
            if pi != ai && pni != ai && pi != bi && pni != bi && self.intersects(p, pn, a, b) {
                return true;
            }
            p = pn;
            if p == a {
                return false;
            }
        }
    }

    fn locally_inside(&self, a: usize, b: usize) -> bool {
        let (ap, an) = (self.prev(a), self.next(a));
        if self.area(ap, a, an) < 0.0 {
            self.area(a, b, an) >= 0.0 && self.area(a, ap, b) >= 0.0
        } else {
            self.area(a, b, ap) < 0.0 || self.area(a, an, b) < 0.0
        }
    }

    fn middle_inside(&mut self, a: usize, b: usize) -> bool {
        let (pa, pb) = (self.xy(a), self.xy(b));
        let (px, py) = ((pa[0] + pb[0]) / 2.0, (pa[1] + pb[1]) / 2.0);
        let mut inside = false;
        let mut p = a;
        loop {
            if !self.step() {
                return false;
            }
            let pn = self.next(p);
            let (q, r) = (self.xy(p), self.xy(pn));
            if (q[1] > py) != (r[1] > py)
                && r[1] != q[1]
                && px < (r[0] - q[0]) * (py - q[1]) / (r[1] - q[1]) + q[0]
            {
                inside = !inside;
            }
            p = pn;
            if p == a {
                return inside;
            }
        }
    }

    /// Links `a` to `b` with a diagonal; returns the copy of `b` that
    /// starts the second ring.
    fn split_polygon(&mut self, a: usize, b: usize) -> usize {
        let (na, nb) = (self.n(a), self.n(b));
        let a2 = self.nodes.len();
        let b2 = a2 + 1;
        let (an, bp) = (na.next, nb.prev);
        self.nodes.push(Node {
            i: na.i,
            x: na.x,
            y: na.y,
            prev: b2,
            next: an,
            steiner: false,
        });
        self.nodes.push(Node {
            i: nb.i,
            x: nb.x,
            y: nb.y,
            prev: bp,
            next: a2,
            steiner: false,
        });
        self.set_next(a, b);
        self.set_prev(b, a);
        self.set_prev(an, a2);
        self.set_next(bp, b2);
        b2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(coords: &[[f64; 2]], tris: &[[usize; 3]]) -> f64 {
        tris.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| coords.get(i).copied().unwrap_or_default());
                ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0
            })
            .sum()
    }

    #[test]
    fn a_square_becomes_two_triangles() {
        let sq = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let t = earcut(&sq, &[]);
        assert_eq!(t.len(), 2);
        assert!((area(&sq, &t) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_concave_polygon_is_covered_exactly() {
        // a dart: the 0-2 diagonal runs outside
        let dart = [[0.0, 0.0], [2.0, 1.0], [4.0, 0.0], [2.0, 4.0]];
        let t = earcut(&dart, &[]);
        assert_eq!(t.len(), 2);
        assert!((area(&dart, &t) - 6.0).abs() < 1e-12);
    }

    #[test]
    fn holes_are_left_open() {
        let mut c = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        // two holes, either orientation
        c.extend([[2.0, 2.0], [4.0, 2.0], [4.0, 4.0], [2.0, 4.0]]);
        c.extend([[6.0, 6.0], [6.0, 8.0], [8.0, 8.0], [8.0, 6.0]]);
        let t = earcut(&c, &[4, 8]);
        assert!((area(&c, &t) - 92.0).abs() < 1e-9, "{}", area(&c, &t));
    }

    #[test]
    fn a_loop_touching_itself_is_covered() {
        // a figure of two squares sharing the corner (1, 1)
        let c = [
            [0.0, 0.0],
            [1.0, 0.0],
            [1.0, 1.0],
            [2.0, 1.0],
            [2.0, 2.0],
            [1.0, 2.0],
            [1.0, 1.0],
            [0.0, 1.0],
        ];
        let t = earcut(&c, &[]);
        assert!((area(&c, &t) - 2.0).abs() < 1e-9, "{}", area(&c, &t));
    }

    #[test]
    fn degenerate_input_gives_no_triangles_and_no_panic() {
        assert!(earcut(&[], &[]).is_empty());
        assert!(earcut(&[[0.0, 0.0], [1.0, 1.0]], &[]).is_empty());
        assert!(earcut(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]], &[]).is_empty());
        assert!(earcut(&[[f64::NAN, 0.0], [1.0, 0.0], [0.0, 1.0]], &[]).is_empty());
        // hole indices out of range or unordered
        let sq = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let _ = earcut(&sq, &[9, 2, 2]);
        let _ = earcut(&sq, &[0]);
    }
}
