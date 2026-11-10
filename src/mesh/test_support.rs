//! Test fixtures shared by the reader tests.

/// An ASCII STL of two unit squares (z = 0 and z = 1), two triangles each.
pub(crate) fn cube_stl_ascii() -> String {
    let mut s = String::from("solid cube\n");
    let quads = [
        [[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0]],
        [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
    ];
    for q in quads {
        for t in [[0, 1, 2], [0, 2, 3]] {
            s.push_str("facet normal 0 0 1\n outer loop\n");
            for k in t {
                let p = q[k];
                s.push_str(&format!("  vertex {} {} {}\n", p[0], p[1], p[2]));
            }
            s.push_str(" endloop\nendfacet\n");
        }
    }
    s.push_str("endsolid cube\n");
    s
}

/// A 3DS chunk: id, length (header included), body.
pub(crate) fn chunk_3ds(id: u16, body: &[u8]) -> Vec<u8> {
    let mut c = id.to_le_bytes().to_vec();
    c.extend_from_slice(&((body.len() + 6) as u32).to_le_bytes());
    c.extend_from_slice(body);
    c
}
