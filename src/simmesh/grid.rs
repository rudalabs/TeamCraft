//! Unstructured points and cells (`Grid`) and the surface the preview shows: outer faces of volume cells, surface cells as they are, triangle thinning.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::mesh::{check_triangles, thin_out, Field, Mesh, MAX_FIELDS};

use super::*;

/// Unstructured points and cells as read from a result file. Cells use VTK
/// cell type ids.
#[derive(Default)]
pub(super) struct Grid {
    /// x, y, z per point.
    pub(super) points: Vec<f32>,
    /// Node indices of all cells, one after another.
    pub(super) conn: Vec<u32>,
    /// End of every cell in `conn`.
    pub(super) ends: Vec<usize>,
    /// VTK cell type of every cell.
    pub(super) types: Vec<u8>,
    pub(super) point_fields: Vec<Field>,
    pub(super) cell_fields: Vec<Field>,
}

// VTK cell types
pub(super) const POLY_VERTEX: u8 = 2;
pub(super) const LINE: u8 = 3;
pub(super) const POLY_LINE: u8 = 4;
pub(super) const TRIANGLE: u8 = 5;
pub(super) const TRIANGLE_STRIP: u8 = 6;
pub(super) const POLYGON: u8 = 7;
pub(super) const PIXEL: u8 = 8;
pub(super) const QUAD: u8 = 9;
pub(super) const TETRA: u8 = 10;
pub(super) const VOXEL: u8 = 11;
pub(super) const HEXAHEDRON: u8 = 12;
pub(super) const WEDGE: u8 = 13;
pub(super) const PYRAMID: u8 = 14;
pub(super) const QUADRATIC_EDGE: u8 = 21;
pub(super) const QUADRATIC_TRIANGLE: u8 = 22;
pub(super) const QUADRATIC_QUAD: u8 = 23;
pub(super) const QUADRATIC_TETRA: u8 = 24;
pub(super) const QUADRATIC_HEXAHEDRON: u8 = 25;
pub(super) const QUADRATIC_WEDGE: u8 = 26;
pub(super) const QUADRATIC_PYRAMID: u8 = 27;
pub(super) const BIQUADRATIC_QUAD: u8 = 28;
pub(super) const TRIQUADRATIC_HEXAHEDRON: u8 = 29;
pub(super) const BIQUADRATIC_QUADRATIC_WEDGE: u8 = 31;
pub(super) const BIQUADRATIC_QUADRATIC_HEXAHEDRON: u8 = 32;
pub(super) const BIQUADRATIC_TRIANGLE: u8 = 34;

pub(super) const TET_FACES: &[&[usize]] = &[&[0, 1, 3], &[1, 2, 3], &[2, 0, 3], &[0, 2, 1]];
pub(super) const HEX_FACES: &[&[usize]] = &[
    &[0, 4, 7, 3],
    &[1, 2, 6, 5],
    &[0, 1, 5, 4],
    &[3, 7, 6, 2],
    &[0, 3, 2, 1],
    &[4, 5, 6, 7],
];
pub(super) const WEDGE_FACES: &[&[usize]] = &[
    &[0, 1, 2],
    &[3, 5, 4],
    &[0, 3, 4, 1],
    &[1, 4, 5, 2],
    &[2, 5, 3, 0],
];
pub(super) const PYRAMID_FACES: &[&[usize]] = &[
    &[0, 3, 2, 1],
    &[0, 1, 4],
    &[1, 2, 4],
    &[2, 3, 4],
    &[3, 0, 4],
];

/// Outer faces of volume cells: a face seen twice is inside and removed.
#[derive(Default)]
pub(super) struct Boundary {
    /// Sorted nodes → (first seen, nodes in cell order, node count).
    pub(super) faces: HashMap<[u32; 4], (u64, [u32; 4], u8)>,
    pub(super) seq: u64,
}

/// Faces that may be open at the same time: every open face becomes at
/// least one triangle unless a later cell closes it, so a file whose open
/// faces exceed this is refused before the map grows to gigabytes.
pub(super) const MAX_OPEN_FACES: usize = 2 * crate::mesh::MAX_TRIANGLES;

impl Boundary {
    pub(super) fn toggle(&mut self, nodes: &[u32]) {
        // Collapsed cells (a hexahedron written as a wedge, …) repeat nodes:
        // the face is matched by its distinct corners, so a quad with one
        // repeated node pairs with the triangle of the neighbour.
        let mut face = [u32::MAX; 4];
        let mut n = 0usize;
        for &p in nodes.iter().take(4) {
            let last = n.checked_sub(1).and_then(|i| face.get(i)).copied();
            if last != Some(p) {
                if let Some(slot) = face.get_mut(n) {
                    *slot = p;
                    n += 1;
                }
            }
        }
        if n > 1 && face.get(n - 1) == face.first() {
            n -= 1;
            if let Some(slot) = face.get_mut(n) {
                *slot = u32::MAX;
            }
        }
        if n < 3 {
            return; // a collapsed face has no area
        }
        let mut key = face;
        if let Some(corners) = key.get_mut(..n) {
            corners.sort_unstable();
        }
        if self.faces.remove(&key).is_none() {
            self.faces.insert(key, (self.seq, face, n as u8));
            self.seq += 1;
        }
    }

    pub(super) fn cell(&mut self, nodes: &[u32], faces: &[&[usize]], order: Option<&[usize; 8]>) {
        'faces: for f in faces {
            let mut face = [0u32; 4];
            for (slot, &i) in face.iter_mut().zip(f.iter()) {
                let i = order.and_then(|o| o.get(i).copied()).unwrap_or(i);
                // the callers check the node count of the cell type
                let Some(&p) = nodes.get(i) else {
                    continue 'faces;
                };
                *slot = p;
            }
            self.toggle(face.get(..f.len()).unwrap_or(&face));
        }
    }
}

/// Keeps every k-th triangle so that at most `max` remain.
pub(super) fn thin_triangles(tris: &mut Vec<u32>, max: usize) {
    let triangles = tris.len() / 3;
    if triangles > max && max > 0 {
        let stride = triangles.div_ceil(max);
        *tris = tris
            .chunks_exact(3)
            .step_by(stride)
            .flatten()
            .copied()
            .collect();
    }
}

/// Polygon → triangle fan.
pub(super) fn fan(out: &mut Vec<u32>, polygon: &[u32]) {
    let Some((&first, rest)) = polygon.split_first() else {
        return;
    };
    for w in rest.windows(2) {
        if let [b, c] = *w {
            out.extend([first, b, c]);
        }
    }
}

impl Grid {
    pub(super) fn point_count(&self) -> usize {
        self.points.len() / 3
    }

    pub(super) fn cell_count(&self) -> usize {
        self.ends.len()
    }

    pub(super) fn check_cells(&self, more: usize, more_conn: usize) -> Result<()> {
        if self.cell_count().saturating_add(more) > MAX_CELLS {
            bail!("The result has more than {MAX_CELLS} cells – too many for the preview.");
        }
        if self.conn.len().saturating_add(more_conn) > MAX_CONNECTIVITY {
            bail!("The result has too many cell nodes for the preview.");
        }
        Ok(())
    }

    /// The nodes of cell `c` (the readers check that the ends rise and stay
    /// inside the connectivity; anything else is an empty cell here).
    pub(super) fn cell(&self, c: usize) -> &[u32] {
        let start = c
            .checked_sub(1)
            .and_then(|p| self.ends.get(p))
            .copied()
            .unwrap_or(0);
        let end = self.ends.get(c).copied().unwrap_or(start);
        self.conn.get(start..end).unwrap_or_default()
    }

    /// Adds a field unless the field budget is used up.
    pub(super) fn add_field(fields: &mut Vec<Field>, field: Field) {
        if fields.len() < MAX_FIELDS && !fields.iter().any(|f| f.name == field.name) {
            fields.push(field);
        }
    }

    pub(super) fn into_mesh(mut self) -> Result<Mesh> {
        let n = self.point_count();
        if n == 0 {
            bail!("The result file contains no points.");
        }
        if self.types.len() != self.ends.len() {
            bail!("The cell types do not match the cells.");
        }
        if self.conn.iter().any(|&i| i as usize >= n) {
            bail!("A cell refers to a missing point.");
        }
        let cells = self.cell_count();

        // fields: point data, then cell data averaged to the points
        let mut fields: Vec<Field> = std::mem::take(&mut self.point_fields);
        fields.retain(|f| f.values.len() == n);
        for f in std::mem::take(&mut self.cell_fields) {
            if f.values.len() != cells || fields.len() >= MAX_FIELDS {
                continue;
            }
            let mut sum = vec![0f64; n];
            let mut hits = vec![0u32; n];
            for (c, &v) in f.values.iter().enumerate() {
                if v.is_finite() {
                    for &p in self.cell(c) {
                        // every node is below `n` (checked above)
                        if let (Some(s), Some(h)) =
                            (sum.get_mut(p as usize), hits.get_mut(p as usize))
                        {
                            *s += v as f64;
                            *h += 1;
                        }
                    }
                }
            }
            let values = sum
                .iter()
                .zip(&hits)
                .map(|(s, &h)| {
                    if h > 0 {
                        (s / h as f64) as f32
                    } else {
                        f32::NAN
                    }
                })
                .collect();
            Grid::add_field(
                &mut fields,
                Field {
                    name: format!("{} (cells)", f.name),
                    unit: f.unit,
                    values,
                },
            );
        }
        let keep = (FIELD_BUDGET / n).min(MAX_FIELDS);
        fields.truncate(keep);

        // surface triangles
        let mut tris: Vec<u32> = Vec::new();
        let mut boundary = Boundary::default();
        const VOXEL_ORDER: [usize; 8] = [0, 1, 3, 2, 4, 5, 7, 6];
        for (c, &ty) in self.types.iter().enumerate() {
            let nodes = self.cell(c);
            let len = nodes.len();
            match (ty, nodes) {
                (TRIANGLE | QUADRATIC_TRIANGLE | BIQUADRATIC_TRIANGLE, &[a, b, c, ..]) => {
                    tris.extend([a, b, c])
                }
                (TRIANGLE_STRIP, _) => {
                    for (k, w) in nodes.windows(3).enumerate() {
                        if let [a, b, c] = *w {
                            // every second triangle turns the other way
                            tris.extend(if k % 2 == 0 { [a, b, c] } else { [b, a, c] });
                        }
                    }
                }
                (POLYGON, _) => fan(&mut tris, nodes),
                (PIXEL, &[a, b, c, d, ..]) => fan(&mut tris, &[a, b, d, c]),
                (QUAD | QUADRATIC_QUAD | BIQUADRATIC_QUAD, &[a, b, c, d, ..]) => {
                    fan(&mut tris, &[a, b, c, d])
                }
                (TETRA | QUADRATIC_TETRA, _) if len >= 4 => boundary.cell(nodes, TET_FACES, None),
                (VOXEL, _) if len >= 8 => boundary.cell(nodes, HEX_FACES, Some(&VOXEL_ORDER)),
                (
                    HEXAHEDRON
                    | QUADRATIC_HEXAHEDRON
                    | TRIQUADRATIC_HEXAHEDRON
                    | BIQUADRATIC_QUADRATIC_HEXAHEDRON,
                    _,
                ) if len >= 8 => boundary.cell(nodes, HEX_FACES, None),
                (WEDGE | QUADRATIC_WEDGE | BIQUADRATIC_QUADRATIC_WEDGE, _) if len >= 6 => {
                    boundary.cell(nodes, WEDGE_FACES, None)
                }
                (PYRAMID | QUADRATIC_PYRAMID, _) if len >= 5 => {
                    boundary.cell(nodes, PYRAMID_FACES, None)
                }
                // vertices, lines, beams, polyhedra and unknown cells are not drawn
                _ => {}
            }
            check_triangles(tris.len() / 3)?;
            if boundary.faces.len() > MAX_OPEN_FACES {
                bail!(
                    "The result has too many outer faces for the preview \
                     (more than {MAX_OPEN_FACES})."
                );
            }
        }
        let mut outer: Vec<(u64, [u32; 4], u8)> = boundary.faces.into_values().collect();
        outer.sort_unstable_by_key(|f| f.0);
        for (_, face, k) in outer {
            fan(&mut tris, face.get(..k as usize).unwrap_or(&face));
            check_triangles(tris.len() / 3)?;
        }
        // drop degenerate triangles (collapsed hexahedra, repeated nodes)
        let mut kept = Vec::with_capacity(tris.len());
        for t in tris.chunks_exact(3) {
            if let [a, b, c] = *t {
                if a != b && b != c && a != c {
                    kept.extend([a, b, c]);
                }
            }
        }
        let mut tris = kept;

        if tris.is_empty() {
            // no surface (only points, vertex or line cells): a point cloud
            let mut mesh = Mesh {
                positions: self.points,
                parts: 1,
                fields,
                ..Mesh::default()
            };
            thin_out(&mut mesh);
            mesh.bake_colors();
            return Ok(mesh);
        }

        thin_triangles(&mut tris, MAX_RESULT_TRIANGLES);

        // only the points of the surface
        let mut remap = vec![u32::MAX; n];
        let mut positions = Vec::new();
        let mut used: Vec<u32> = Vec::new();
        for i in tris.iter_mut() {
            let p = *i as usize;
            // every node is below `n` (checked above)
            let (Some(slot), Some(xyz)) = (remap.get_mut(p), self.points.get(p * 3..p * 3 + 3))
            else {
                bail!("A cell refers to a missing point.");
            };
            if *slot == u32::MAX {
                *slot = used.len() as u32;
                used.push(*i);
                positions.extend_from_slice(xyz);
            }
            *i = *slot;
        }
        for f in &mut fields {
            let values = &f.values;
            f.values = used
                .iter()
                .map(|&p| values.get(p as usize).copied().unwrap_or(f32::NAN))
                .collect();
        }
        let mut mesh = Mesh {
            positions,
            indices: tris,
            parts: 1,
            fields,
            ..Mesh::default()
        };
        mesh.bake_colors();
        Ok(mesh)
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// A grid of `points` and `(cell type, nodes)` cells.
    fn grid(points: &[[f32; 3]], cells: &[(u8, &[u32])]) -> Grid {
        let mut g = Grid {
            points: points.iter().flatten().copied().collect(),
            ..Grid::default()
        };
        for (ty, nodes) in cells {
            g.conn.extend_from_slice(nodes);
            g.ends.push(g.conn.len());
            g.types.push(*ty);
        }
        g
    }

    const TET_POINTS: [[f32; 3]; 5] = [
        [0., 0., 0.],
        [1., 0., 0.],
        [0., 1., 0.],
        [0., 0., 1.],
        [1., 1., 1.],
    ];

    /// The corners of an `nx × 1 × 1` row of unit cubes.
    fn cube_row(nx: u32) -> (Vec<[f32; 3]>, Vec<[u32; 8]>) {
        let mut points = Vec::new();
        for x in 0..=nx {
            for (y, z) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
                points.push([x as f32, y as f32, z as f32]);
            }
        }
        // corner k of plane x: x * 4 + k
        let hexes = (0..nx)
            .map(|x| {
                let a = x * 4;
                let b = a + 4;
                [a, b, b + 1, a + 1, a + 3, b + 3, b + 2, a + 2]
            })
            .collect();
        (points, hexes)
    }

    fn no_degenerate_triangles(mesh: &Mesh) -> bool {
        mesh.indices
            .chunks_exact(3)
            .all(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
    }

    #[test]
    fn volume_cells_show_their_outer_faces() {
        let one = grid(&TET_POINTS[..4], &[(TETRA, &[0, 1, 2, 3])]);
        let mesh = one.into_mesh().unwrap();
        assert_eq!((mesh.triangle_count(), mesh.vertex_count()), (4, 4));

        // two tetrahedra sharing the face 1-2-3: that face is inside
        let two = grid(
            &TET_POINTS,
            &[(TETRA, &[0, 1, 2, 3]), (TETRA, &[4, 2, 1, 3])],
        );
        let mesh = two.into_mesh().unwrap();
        assert_eq!((mesh.triangle_count(), mesh.vertex_count()), (6, 5));

        let (points, hexes) = cube_row(1);
        let mesh = grid(&points, &[(HEXAHEDRON, &hexes[0])])
            .into_mesh()
            .unwrap();
        assert_eq!(mesh.triangle_count(), 12, "6 quads");
        // two hexahedra side by side: 10 outer quads
        let (points, hexes) = cube_row(2);
        let mesh = grid(&points, &[(HEXAHEDRON, &hexes[0]), (HEXAHEDRON, &hexes[1])])
            .into_mesh()
            .unwrap();
        assert_eq!((mesh.triangle_count(), mesh.vertex_count()), (20, 12));
        assert!(no_degenerate_triangles(&mesh));
    }

    #[test]
    fn collapsed_cells_leave_no_degenerate_triangles() {
        // a wedge written as a hexahedron: the top edge repeats its nodes
        let points = [
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 1., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [1., 0., 1.],
        ];
        let mesh = grid(&points, &[(HEXAHEDRON, &[0, 1, 2, 3, 4, 5, 5, 4])])
            .into_mesh()
            .unwrap();
        assert!(no_degenerate_triangles(&mesh));
        // 2 triangles + 3 quads of the wedge
        assert_eq!(mesh.triangle_count(), 8);
    }

    #[test]
    fn surface_cells_are_drawn_as_they_are() {
        let square = [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]];
        for (ty, nodes, tris) in [
            (TRIANGLE, &[0u32, 1, 2][..], 1),
            (QUAD, &[0, 1, 2, 3][..], 2),
            (PIXEL, &[0, 1, 3, 2][..], 2),
            (POLYGON, &[0, 1, 2, 3][..], 2),
            (TRIANGLE_STRIP, &[0, 1, 3, 2][..], 2),
        ] {
            let mesh = grid(&square, &[(ty, nodes)]).into_mesh().unwrap();
            assert_eq!(mesh.triangle_count(), tris, "cell type {ty}");
        }
        // every second triangle of a strip turns the other way
        let mesh = grid(&square, &[(TRIANGLE_STRIP, &[0, 1, 3, 2])])
            .into_mesh()
            .unwrap();
        let p = |i: usize| mesh.positions.get(i * 3..i * 3 + 3).unwrap().to_vec();
        let normal = |t: &[u32]| {
            let (a, b, c) = (p(t[0] as usize), p(t[1] as usize), p(t[2] as usize));
            (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
        };
        let tris: Vec<&[u32]> = mesh.indices.chunks_exact(3).collect();
        assert_eq!(normal(tris[0]).signum(), normal(tris[1]).signum());
    }

    #[test]
    fn only_surface_points_and_their_values_are_kept() {
        let mut g = grid(&TET_POINTS, &[(TETRA, &[0, 1, 2, 3])]);
        g.point_fields.push(Field {
            name: "T".into(),
            unit: "K".into(),
            values: vec![10.0, 11.0, 12.0, 13.0, 99.0],
        });
        let mesh = g.into_mesh().unwrap();
        assert_eq!(mesh.vertex_count(), 4, "point 4 is in no cell");
        let mut values = mesh.fields[0].values.clone();
        values.sort_by(f32::total_cmp);
        assert_eq!(values, [10.0, 11.0, 12.0, 13.0]);
        assert!(mesh.check().is_ok());
    }

    #[test]
    fn cell_values_are_averaged_to_the_points() {
        let mut g = grid(
            &TET_POINTS,
            &[(TETRA, &[0, 1, 2, 3]), (TETRA, &[4, 2, 1, 3])],
        );
        g.cell_fields.push(Field {
            name: "Damage".into(),
            unit: String::new(),
            values: vec![2.0, 4.0],
        });
        let mesh = g.into_mesh().unwrap();
        assert_eq!(mesh.fields[0].name, "Damage (cells)");
        // positions → value: point 0 only in cell 0, point 4 only in cell 1,
        // the shared points 1, 2, 3 in both
        for (k, v) in mesh.fields[0].values.iter().enumerate() {
            let pos = mesh.positions.get(k * 3..k * 3 + 3).unwrap();
            let expected = match pos {
                [0., 0., 0.] => 2.0,
                [1., 1., 1.] => 4.0,
                _ => 3.0,
            };
            assert_eq!(*v, expected, "{pos:?}");
        }
        // a cell field of the wrong length is dropped
        let mut g = grid(&TET_POINTS[..4], &[(TETRA, &[0, 1, 2, 3])]);
        g.cell_fields.push(Field {
            name: "X".into(),
            unit: String::new(),
            values: vec![1.0, 2.0],
        });
        assert!(g.into_mesh().unwrap().fields.is_empty());
    }

    #[test]
    fn points_without_a_surface_are_a_point_cloud() {
        let g = grid(&TET_POINTS, &[(POLY_VERTEX, &[0, 1, 2]), (LINE, &[3, 4])]);
        let mesh = g.into_mesh().unwrap();
        assert!(mesh.is_point_cloud());
        assert_eq!(mesh.vertex_count(), 5);
    }

    #[test]
    fn broken_grids_are_errors() {
        let msg = |g: Grid| g.into_mesh().unwrap_err().to_string();
        assert!(msg(Grid::default()).contains("no points"));
        let mut g = grid(&TET_POINTS[..4], &[(TETRA, &[0, 1, 2, 3])]);
        g.types.push(TETRA);
        assert!(msg(g).contains("cell types do not match"));
        assert!(msg(grid(&TET_POINTS[..4], &[(TETRA, &[0, 1, 2, 9])])).contains("missing point"));
        // a volume cell with too few nodes is not drawn – no panic
        let short = grid(&TET_POINTS[..4], &[(HEXAHEDRON, &[0, 1, 2, 3])]);
        assert!(short.into_mesh().unwrap().is_point_cloud());
    }

    #[test]
    fn cells_and_limits() {
        let g = grid(&TET_POINTS, &[(TETRA, &[0, 1, 2, 3]), (LINE, &[3, 4])]);
        assert_eq!((g.point_count(), g.cell_count()), (5, 2));
        assert_eq!(g.cell(1), &[3, 4]);
        assert!(g.cell(5).is_empty(), "past the last cell");
        assert!(g.check_cells(1, 1).is_ok());
        assert!(g.check_cells(MAX_CELLS, 0).is_err());
        assert!(g.check_cells(0, MAX_CONNECTIVITY).is_err());

        let mut fields = Vec::new();
        let field = |name: &str| Field {
            name: name.into(),
            unit: String::new(),
            values: Vec::new(),
        };
        Grid::add_field(&mut fields, field("A"));
        Grid::add_field(&mut fields, field("A"));
        assert_eq!(fields.len(), 1, "no duplicate names");
        for i in 0..2 * MAX_FIELDS {
            Grid::add_field(&mut fields, field(&format!("F{i}")));
        }
        assert_eq!(fields.len(), MAX_FIELDS);
    }

    #[test]
    fn thinning_and_fans() {
        let mut tris: Vec<u32> = (0..30).collect(); // 10 triangles
        thin_triangles(&mut tris, 4);
        assert_eq!(
            tris,
            [0, 1, 2, 9, 10, 11, 18, 19, 20, 27, 28, 29],
            "every third"
        );
        let mut few: Vec<u32> = (0..9).collect();
        thin_triangles(&mut few, 3);
        assert_eq!(few.len(), 9, "at the limit");
        thin_triangles(&mut few, 0);
        assert_eq!(few.len(), 9, "0 means no limit");

        let mut out = Vec::new();
        fan(&mut out, &[5, 6, 7, 8]);
        assert_eq!(out, [5, 6, 7, 5, 7, 8]);

        // a face that collapses to two corners has no area
        let mut b = Boundary::default();
        b.toggle(&[1, 2, 2, 1]);
        assert!(b.faces.is_empty());
        // the same face twice (any order) cancels out
        b.toggle(&[1, 2, 3]);
        b.toggle(&[3, 1, 2]);
        assert!(b.faces.is_empty());
    }
}
