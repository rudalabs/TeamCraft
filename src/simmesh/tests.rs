//! Tests of the simulation result readers with the self-made fixtures in
//! `tests/fixtures/sim/` (see the README there). The binary and encoded
//! fixtures are written by [`generate`]; regenerate them with
//! `TEAMCRAFT_WRITE_SIM_FIXTURES=1 cargo test --lib write_sim_fixtures -- --ignored`.

use super::*;
use crate::mesh::{Field, Mesh, MAX_FIELDS};
use anyhow::Result;
use base64::Engine;
use std::io::Write;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sim")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Text fixtures may have CRLF line ends after a checkout on Windows.
fn lf(data: &[u8]) -> Vec<u8> {
    data.iter().copied().filter(|&b| b != b'\r').collect()
}

/// The fixture model: two unit hexahedra side by side along x.
mod generate {
    use super::*;

    pub fn points() -> Vec<f32> {
        let mut p = Vec::new();
        for k in 0..2 {
            for j in 0..2 {
                for i in 0..3 {
                    p.extend([i as f32, j as f32, k as f32]);
                }
            }
        }
        p
    }

    pub const HEXES: [[u32; 8]; 2] = [[0, 1, 4, 3, 6, 7, 10, 9], [1, 2, 5, 4, 7, 8, 11, 10]];

    fn x(p: usize) -> f32 {
        (p % 3) as f32
    }

    pub fn temperature() -> Vec<f32> {
        (0..12).map(x).collect()
    }

    pub fn displacement() -> Vec<f32> {
        (0..12).flat_map(|p| [0.0, 0.0, -0.1 * x(p)]).collect()
    }

    pub fn stress() -> Vec<f32> {
        (0..12)
            .flat_map(|p| [100.0 * x(p), 0.0, 0.0, 0.0, 0.0, 0.0])
            .collect()
    }

    fn be_f32(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_be_bytes()).collect()
    }

    fn be_i32(v: &[i32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_be_bytes()).collect()
    }

    /// Legacy VTK, BINARY (big endian), classic CELLS layout.
    pub fn legacy_binary() -> Vec<u8> {
        let mut out = b"# vtk DataFile Version 3.0\nTeamCraft test fixture: two hexahedra, binary (self-made)\nBINARY\nDATASET UNSTRUCTURED_GRID\nPOINTS 12 float\n".to_vec();
        out.extend(be_f32(&points()));
        out.extend(b"\nCELLS 2 18\n");
        let mut cells = Vec::new();
        for h in HEXES {
            cells.push(8);
            cells.extend(h.iter().map(|&i| i as i32));
        }
        out.extend(be_i32(&cells));
        out.extend(b"\nCELL_TYPES 2\n");
        out.extend(be_i32(&[12, 12]));
        out.extend(b"\nPOINT_DATA 12\nSCALARS temperature float 1\nLOOKUP_TABLE default\n");
        out.extend(be_f32(&temperature()));
        out.extend(b"\nVECTORS displacement float\n");
        out.extend(be_f32(&displacement()));
        out.extend(b"\nCELL_DATA 2\nSCALARS material int 1\nLOOKUP_TABLE default\n");
        out.extend(be_i32(&[1, 2]));
        out.extend(b"\n");
        out
    }

    #[derive(Clone, Copy, PartialEq)]
    pub enum Mode {
        /// inline base64, UInt32 header
        Base64,
        /// raw appended data, UInt64 header
        AppendedRaw,
        /// inline base64 with zlib blocks, UInt64 header (header and data
        /// encoded separately as VTK does)
        Zlib,
    }

    struct Array {
        section: &'static str,
        name: &'static str,
        ty: &'static str,
        components: usize,
        bytes: Vec<u8>,
    }

    fn le<T: Copy>(v: &[T], f: impl Fn(T) -> Vec<u8>) -> Vec<u8> {
        v.iter().flat_map(|&x| f(x)).collect()
    }

    pub fn vtu(mode: Mode) -> Vec<u8> {
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let mut conn = Vec::new();
        for h in HEXES {
            conn.extend(h.iter().map(|&i| i as i64));
        }
        let arrays = [
            Array {
                section: "PointData",
                name: "temperature",
                ty: "Float32",
                components: 1,
                bytes: le(&temperature(), |v| v.to_le_bytes().to_vec()),
            },
            Array {
                section: "PointData",
                name: "displacement",
                ty: "Float64",
                components: 3,
                bytes: le(&displacement(), |v| (v as f64).to_le_bytes().to_vec()),
            },
            Array {
                section: "PointData",
                name: "stress",
                ty: "Float32",
                components: 6,
                bytes: le(&stress(), |v| v.to_le_bytes().to_vec()),
            },
            Array {
                section: "CellData",
                name: "material",
                ty: "Int32",
                components: 1,
                bytes: le(&[1i32, 2], |v| v.to_le_bytes().to_vec()),
            },
            Array {
                section: "Points",
                name: "Points",
                ty: "Float32",
                components: 3,
                bytes: le(&points(), |v| v.to_le_bytes().to_vec()),
            },
            Array {
                section: "Cells",
                name: "connectivity",
                ty: "Int64",
                components: 1,
                bytes: le(&conn, |v| v.to_le_bytes().to_vec()),
            },
            Array {
                section: "Cells",
                name: "offsets",
                ty: "Int64",
                components: 1,
                bytes: le(&[8i64, 16], |v| v.to_le_bytes().to_vec()),
            },
            Array {
                section: "Cells",
                name: "types",
                ty: "UInt8",
                components: 1,
                bytes: vec![12, 12],
            },
        ];
        let header64 = mode != Mode::Base64;
        let head = |n: usize| -> Vec<u8> {
            if header64 {
                (n as u64).to_le_bytes().to_vec()
            } else {
                (n as u32).to_le_bytes().to_vec()
            }
        };
        let mut xml = format!(
            "<?xml version=\"1.0\"?>\n<!-- TeamCraft test fixture: two hexahedra (self-made) -->\n<VTKFile type=\"UnstructuredGrid\" version=\"1.0\" byte_order=\"LittleEndian\" header_type=\"{}\"{}>\n  <UnstructuredGrid>\n    <Piece NumberOfPoints=\"12\" NumberOfCells=\"2\">\n",
            if header64 { "UInt64" } else { "UInt32" },
            if mode == Mode::Zlib { " compressor=\"vtkZLibDataCompressor\"" } else { "" }
        );
        let mut appended: Vec<u8> = Vec::new();
        let mut open: Option<&str> = None;
        for a in &arrays {
            if open != Some(a.section) {
                if let Some(s) = open {
                    xml.push_str(&format!("      </{s}>\n"));
                }
                let attr = if a.section == "PointData" {
                    " Scalars=\"temperature\""
                } else {
                    ""
                };
                xml.push_str(&format!("      <{}{attr}>\n", a.section));
                open = Some(a.section);
            }
            let start = format!(
                "        <DataArray type=\"{}\" Name=\"{}\" NumberOfComponents=\"{}\"",
                a.ty, a.name, a.components
            );
            match mode {
                Mode::Base64 => {
                    let mut block = head(a.bytes.len());
                    block.extend(&a.bytes);
                    xml.push_str(&format!(
                        "{start} format=\"binary\">\n          {}\n        </DataArray>\n",
                        b64(&block)
                    ));
                }
                Mode::Zlib => {
                    let mut z =
                        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                    z.write_all(&a.bytes).unwrap();
                    let compressed = z.finish().unwrap();
                    let mut h = head(1);
                    h.extend(head(a.bytes.len()));
                    h.extend(head(a.bytes.len()));
                    h.extend(head(compressed.len()));
                    xml.push_str(&format!(
                        "{start} format=\"binary\">\n          {}{}\n        </DataArray>\n",
                        b64(&h),
                        b64(&compressed)
                    ));
                }
                Mode::AppendedRaw => {
                    xml.push_str(&format!(
                        "{start} format=\"appended\" offset=\"{}\"/>\n",
                        appended.len()
                    ));
                    appended.extend(head(a.bytes.len()));
                    appended.extend(&a.bytes);
                }
            }
        }
        if let Some(s) = open {
            xml.push_str(&format!("      </{s}>\n"));
        }
        xml.push_str("    </Piece>\n  </UnstructuredGrid>\n");
        let mut out = xml.into_bytes();
        if mode == Mode::AppendedRaw {
            out.extend(b"  <AppendedData encoding=\"raw\">\n   _");
            out.extend(&appended);
            out.extend(b"\n  </AppendedData>\n");
        }
        out.extend(b"</VTKFile>\n");
        out
    }

    /// Fortran E12.5, as CalculiX writes it.
    fn e12(v: f32) -> String {
        let v = v as f64;
        let s = if v == 0.0 {
            "0.00000E+00".to_string()
        } else {
            let e = v.abs().log10().floor() as i32;
            format!(
                "{:.5}E{}{:02}",
                v / 10f64.powi(e),
                if e < 0 { '-' } else { '+' },
                e.abs()
            )
        };
        format!("{s:>12}")
    }

    /// CalculiX .frd, long format, node numbers 10, 20, …; DISP twice (the
    /// second increment counts) and STRESS.
    pub fn frd() -> String {
        let mut s = String::from(
            "    1C                                                                   \n",
        );
        s.push_str("    1UUSER\n    1UDATE              5.november.2026\n");
        s.push_str("    2C                            12                                     1\n");
        let p = points();
        for i in 0..12 {
            s.push_str(&format!(
                " -1{:>10}{}{}{}\n",
                (i + 1) * 10,
                e12(p[i * 3]),
                e12(p[i * 3 + 1]),
                e12(p[i * 3 + 2])
            ));
        }
        s.push_str(" -3\n");
        s.push_str("    3C                             2                                     1\n");
        for (k, h) in HEXES.iter().enumerate() {
            s.push_str(&format!(" -1{:>10}{:>5}{:>5}{:>5}\n -2", k + 1, 1, 0, 1));
            for n in h {
                s.push_str(&format!("{:>10}", (n + 1) * 10));
            }
            s.push('\n');
        }
        s.push_str(" -3\n");
        let disp = displacement();
        for (step, scale) in [(1, 0.5f32), (2, 1.0)] {
            s.push_str(&format!("    1PSTEP{:>26}{:>12}{:>12}\n", step, 1, step));
            s.push_str(
                "  100CL  101 1.000000000           12                     0    1           1\n",
            );
            s.push_str(" -4  DISP        4    1\n");
            for (c, n) in ["D1", "D2", "D3"].iter().enumerate() {
                s.push_str(&format!(" -5  {n:<8}    1    2{:>5}    0\n", c + 1));
            }
            s.push_str(" -5  ALL         1    2    0    0    1ALL\n");
            for i in 0..12 {
                s.push_str(&format!(" -1{:>10}", (i + 1) * 10));
                for c in 0..3 {
                    s.push_str(&e12(disp[i * 3 + c] * scale));
                }
                s.push('\n');
            }
            s.push_str(" -3\n");
        }
        s.push_str(" -4  STRESS      6    1\n");
        for (n, a, b) in [
            ("SXX", 1, 1),
            ("SYY", 2, 2),
            ("SZZ", 3, 3),
            ("SXY", 1, 2),
            ("SYZ", 2, 3),
            ("SZX", 3, 1),
        ] {
            s.push_str(&format!(" -5  {n:<8}    1    4{a:>5}{b:>5}\n"));
        }
        let st = stress();
        for i in 0..12 {
            s.push_str(&format!(" -1{:>10}", (i + 1) * 10));
            for c in 0..6 {
                s.push_str(&e12(st[i * 6 + c]));
            }
            s.push('\n');
        }
        s.push_str(" -3\n9999\n");
        s
    }
}

#[test]
#[ignore = "writes tests/fixtures/sim; run with TEAMCRAFT_WRITE_SIM_FIXTURES=1"]
fn write_sim_fixtures() {
    if std::env::var("TEAMCRAFT_WRITE_SIM_FIXTURES").as_deref() != Ok("1") {
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sim");
    std::fs::write(dir.join("beam-binary.vtk"), generate::legacy_binary()).unwrap();
    std::fs::write(
        dir.join("beam-base64.vtu"),
        generate::vtu(generate::Mode::Base64),
    )
    .unwrap();
    std::fs::write(
        dir.join("beam-appended-raw.vtu"),
        generate::vtu(generate::Mode::AppendedRaw),
    )
    .unwrap();
    std::fs::write(
        dir.join("beam-zlib.vtu"),
        generate::vtu(generate::Mode::Zlib),
    )
    .unwrap();
    std::fs::write(dir.join("beam.frd"), generate::frd()).unwrap();
}

fn field<'a>(mesh: &'a Mesh, name: &str) -> &'a Field {
    mesh.fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no field {name} in {:?}",
                mesh.fields.iter().map(|f| &f.name).collect::<Vec<_>>()
            )
        })
}

fn near(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
}

/// The surface of the two hexahedra: 10 outer quads, the shared face is gone.
fn assert_beam(mesh: &Mesh) {
    mesh.check().unwrap();
    assert_eq!(mesh.triangle_count(), 20, "10 outer quads");
    assert_eq!(mesh.vertex_count(), 12);
    assert_eq!(mesh.bounds(), [0., 0., 0., 2., 1., 1.]);
    assert_eq!(mesh.colors.len(), 36, "first field baked as colours");
    // the shared face x = 1 is not part of the surface
    for t in mesh.indices.chunks_exact(3) {
        let xs: Vec<f32> = t.iter().map(|&i| mesh.positions[i as usize * 3]).collect();
        assert!(xs.iter().any(|&x| x != 1.0), "interior face {t:?}");
    }
    // values follow their vertices through the compaction
    let t = field(mesh, "temperature");
    for (v, &value) in t.values.iter().enumerate() {
        assert_eq!(value, mesh.positions[v * 3]);
    }
    assert!(near(
        field(mesh, "displacement (magnitude)").range(),
        (0.0, 0.2)
    ));
    let material = field(mesh, "material (cells)");
    assert!(near(material.range(), (1.0, 2.0)));
    for (v, &value) in material.values.iter().enumerate() {
        let x = mesh.positions[v * 3];
        assert_eq!(
            value,
            [1.0, 1.5, 2.0][x as usize],
            "cell data averaged at x = {x}"
        );
    }
}

#[test]
fn legacy_ascii_and_binary() {
    let ascii = vtk_legacy(&fixture("beam-ascii.vtk")).unwrap();
    assert_beam(&ascii);
    assert_eq!(ascii.fields[0].name, "temperature");
    let binary = vtk_legacy(&fixture("beam-binary.vtk")).unwrap();
    assert_beam(&binary);
    assert_eq!(binary, ascii, "binary and ASCII give the same mesh");
    assert_eq!(
        fixture("beam-binary.vtk"),
        generate::legacy_binary(),
        "fixture is current"
    );
}

#[test]
fn legacy_version5_offsets_and_polydata() {
    let v5 = b"# vtk DataFile Version 5.1\nv5\nASCII\nDATASET UNSTRUCTURED_GRID\n\
POINTS 4 double\n0 0 0 1 0 0 0 1 0 0 0 1\nMETADATA\nINFORMATION 0\n\n\
CELLS 2 4\nOFFSETS vtktypeint64\n0 4\nCONNECTIVITY vtktypeint64\n0 1 2 3\n\
CELL_TYPES 1\n10\nPOINT_DATA 4\nTENSORS6 s float\n1 0 0 0 0 0 2 0 0 0 0 0 3 0 0 0 0 0 4 0 0 0 0 0\n";
    let mesh = vtk_legacy(v5).unwrap();
    assert_eq!(mesh.triangle_count(), 4, "tetrahedron");
    assert_eq!(mesh.fields[0].name, "s (von Mises)");
    assert!(near(mesh.fields[0].range(), (1.0, 4.0)));

    let poly = b"# vtk DataFile Version 3.0\npoly\nASCII\nDATASET POLYDATA\n\
POINTS 6 float\n0 0 0 1 0 0 1 1 0 0 1 0 2 0 0 2 1 0\n\
POLYGONS 1 5\n4 0 1 2 3\nTRIANGLE_STRIPS 1 5\n4 1 4 2 5\nLINES 1 3\n2 0 4\n\
FIELD FieldData 1\nTIME 1 1 double\n0.5\n\
POINT_DATA 6\nFIELD attributes 1\np 1 6 float\n1 2 3 4 5 6\n";
    let mesh = vtk_legacy(poly).unwrap();
    assert_eq!(mesh.triangle_count(), 4, "quad + strip of two");
    assert_eq!(mesh.fields[0].name, "p");

    let cloud = b"# vtk DataFile Version 3.0\npts\nASCII\nDATASET POLYDATA\nPOINTS 3 float\n0 0 0 1 0 0 0 1 0\nVERTICES 1 4\n3 0 1 2\n";
    let mesh = vtk_legacy(cloud).unwrap();
    assert!(mesh.is_point_cloud());
    assert_eq!(mesh.vertex_count(), 3);
}

/// One cell of each kind; the outer surface has the expected triangles.
#[test]
fn volume_cells_give_their_outer_faces() {
    let grid = |points: &[f32], cells: &[(u8, &[u32])]| {
        let mut g = Grid {
            points: points.to_vec(),
            ..Grid::default()
        };
        for (ty, nodes) in cells {
            g.conn.extend_from_slice(nodes);
            g.ends.push(g.conn.len());
            g.types.push(*ty);
        }
        g.into_mesh().unwrap()
    };
    let cube: Vec<f32> = vec![
        0., 0., 0., 1., 0., 0., 1., 1., 0., 0., 1., 0., 0., 0., 1., 1., 0., 1., 1., 1., 1., 0., 1.,
        1.,
    ];
    let quadratic: Vec<f32> = cube
        .iter()
        .chain(&[0.5f32, 0.5, 0.5].repeat(12))
        .copied()
        .collect();
    type Case<'a> = (&'a str, &'a [f32], &'a [(u8, &'a [u32])], usize);
    let cases: &[Case] = &[
        ("tetra", &cube, &[(TETRA, &[0, 1, 3, 4])], 4),
        (
            "two tetras sharing a face",
            &cube,
            &[(TETRA, &[0, 1, 3, 4]), (TETRA, &[1, 2, 3, 4])],
            6,
        ),
        (
            "hexahedron",
            &cube,
            &[(HEXAHEDRON, &[0, 1, 2, 3, 4, 5, 6, 7])],
            12,
        ),
        ("voxel", &cube, &[(VOXEL, &[0, 1, 3, 2, 4, 5, 7, 6])], 12),
        ("wedge", &cube, &[(WEDGE, &[0, 1, 3, 4, 5, 7])], 8),
        ("pyramid", &cube, &[(PYRAMID, &[0, 1, 2, 3, 6])], 6),
        (
            "quadratic tetra",
            &quadratic,
            &[(QUADRATIC_TETRA, &[0, 1, 3, 4, 8, 9, 10, 11, 12, 13])],
            4,
        ),
        (
            "quadratic hexahedron",
            &quadratic,
            &[(
                QUADRATIC_HEXAHEDRON,
                &[
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
                ],
            )],
            12,
        ),
        (
            "pixel and quad",
            &cube,
            &[(PIXEL, &[0, 1, 3, 2]), (QUAD, &[4, 5, 6, 7])],
            4,
        ),
        (
            "collapsed hexahedron",
            &cube,
            &[(HEXAHEDRON, &[0, 1, 2, 2, 4, 5, 6, 6])],
            8,
        ),
        ("lines only", &cube, &[(LINE, &[0, 1])], 0),
    ];
    for (name, points, cells, triangles) in cases {
        let mesh = grid(points, cells);
        mesh.check().unwrap();
        assert_eq!(mesh.triangle_count(), *triangles, "{name}");
        if *triangles > 0 {
            assert!(
                mesh.vertex_count() <= 8,
                "{name}: midside nodes are dropped"
            );
        }
    }
}

#[test]
fn vtu_ascii_base64_appended_and_zlib() {
    for name in [
        "beam-ascii.vtu",
        "beam-base64.vtu",
        "beam-appended-raw.vtu",
        "beam-zlib.vtu",
    ] {
        let mesh = vtk_xml(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_beam(&mesh);
        assert_eq!(
            mesh.fields[0].name, "temperature",
            "{name}: active scalars first"
        );
        assert!(
            near(field(&mesh, "stress (von Mises)").range(), (0.0, 200.0)),
            "{name}"
        );
    }
    for (name, mode) in [
        ("beam-base64.vtu", generate::Mode::Base64),
        ("beam-appended-raw.vtu", generate::Mode::AppendedRaw),
    ] {
        assert_eq!(
            lf(&fixture(name)),
            lf(&generate::vtu(mode)),
            "{name} is current"
        );
    }
    // the generator's zlib output may differ between zlib versions: decode it too
    assert_beam(&vtk_xml(&generate::vtu(generate::Mode::Zlib)).unwrap());
}

#[test]
fn vtp_polydata_and_pieces() {
    let vtp = br#"<VTKFile type="PolyData" version="0.1">
<PolyData>
<Piece NumberOfPoints="4" NumberOfPolys="1">
<PointData><DataArray type="Float64" Name="p" format="ascii">1 2 3 nan</DataArray></PointData>
<Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0 1 0 0 1 1 0 0 1 0</DataArray></Points>
<Polys>
<DataArray type="Int32" Name="connectivity" format="ascii">0 1 2 3</DataArray>
<DataArray type="Int32" Name="offsets" format="ascii">4</DataArray>
</Polys>
</Piece>
<Piece NumberOfPoints="3" NumberOfStrips="1">
<Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 1 1 0 1 0 1 1</DataArray></Points>
<Strips>
<DataArray type="Int32" Name="connectivity" format="ascii">0 1 2</DataArray>
<DataArray type="Int32" Name="offsets" format="ascii">3</DataArray>
</Strips>
</Piece>
</PolyData>
</VTKFile>"#;
    let mesh = vtk_xml(vtp).unwrap();
    mesh.check().unwrap();
    assert_eq!(mesh.triangle_count(), 3);
    assert_eq!(mesh.vertex_count(), 7);
    let p = field(&mesh, "p");
    assert_eq!(p.range(), (1.0, 3.0), "NaN and missing values are ignored");
    assert_eq!(p.values.iter().filter(|v| v.is_nan()).count(), 4);
}

#[test]
fn frd_nodes_elements_and_results() {
    let data = fixture("beam.frd");
    assert_eq!(
        String::from_utf8_lossy(&lf(&data)),
        generate::frd(),
        "fixture is current"
    );
    let mesh = frd(&data).unwrap();
    mesh.check().unwrap();
    assert_eq!(mesh.triangle_count(), 20);
    assert_eq!(mesh.vertex_count(), 12);
    assert_eq!(mesh.fields[0].name, "STRESS (von Mises)", "stress first");
    assert!(near(mesh.fields[0].range(), (0.0, 200.0)));
    assert_eq!(mesh.fields[1].name, "DISP (magnitude)");
    assert!(
        near(mesh.fields[1].range(), (0.0, 0.2)),
        "the last increment counts"
    );
    assert_eq!(mesh.fields.len(), 2);

    // short format (I5 node numbers), whitespace-separated fallback
    let short = "    2C                             4                                     0\n\
 -1    1 0.00000E+00 0.00000E+00 0.00000E+00\n -1    2 1.00000E+00 0.00000E+00 0.00000E+00\n\
 -1    3 0.00000E+00 1.00000E+00 0.00000E+00\n -1    4 0.00000E+00 0.00000E+00 1.00000E+00\n -3\n\
    3C                             1                                     0\n -1    1    3    0    1\n -2    1    2    3    4\n -3\n\
 -4  NDTEMP      1    1\n -5  T           1    1    0    0\n -1    1 2.00000E+01\n -1    4 3.00000E+01\n -3\n9999\n";
    let mesh = frd(short.as_bytes()).unwrap();
    assert_eq!(mesh.triangle_count(), 4);
    assert_eq!(mesh.fields[0].name, "NDTEMP");
    assert_eq!(mesh.fields[0].range(), (20.0, 30.0));
    assert!(frd(b"    2C 1 2\n").is_err(), "binary FRD");
    assert!(frd(b"hello").is_err());
}

#[test]
fn crlf_text_files_are_read() {
    let crlf = |data: Vec<u8>| -> Vec<u8> {
        lf(&data)
            .iter()
            .flat_map(|&b| {
                if b == b'\n' {
                    vec![b'\r', b'\n']
                } else {
                    vec![b]
                }
            })
            .collect()
    };
    assert_beam(&vtk_legacy(&crlf(fixture("beam-ascii.vtk"))).unwrap());
    assert_beam(&vtk_xml(&crlf(fixture("beam-ascii.vtu"))).unwrap());
    assert_beam(&vtk_xml(&crlf(fixture("beam-base64.vtu"))).unwrap());
    assert_eq!(
        frd(&crlf(fixture("beam.frd"))).unwrap().triangle_count(),
        20
    );
}

#[test]
fn many_triangles_are_thinned_evenly() {
    let mut tris: Vec<u32> = (0..30).collect();
    thin_triangles(&mut tris, 4);
    assert_eq!(tris, vec![0, 1, 2, 9, 10, 11, 18, 19, 20, 27, 28, 29]);
    let mut few: Vec<u32> = (0..6).collect();
    thin_triangles(&mut few, 4);
    assert_eq!(few.len(), 6);
}

#[test]
fn encoded_fields_round_trip_and_old_meshes_decode() {
    let mesh = vtk_xml(&fixture("beam-ascii.vtu")).unwrap();
    let bytes = mesh.encode();
    assert_eq!(bytes.len() % 4, 0);
    assert_eq!(
        u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
        3,
        "colours + fields"
    );
    let back = Mesh::decode(&bytes).unwrap();
    assert_eq!(back, mesh);
    // An old reader stops after the colours: the prefix is a complete mesh.
    let n = mesh.vertex_count();
    let old_len = 48 + n * 12 + mesh.indices.len() * 4 + n * 3;
    let mut old = bytes[..old_len.div_ceil(4) * 4].to_vec();
    old[20] = 1;
    let old_mesh = Mesh::decode(&old).unwrap();
    assert!(old_mesh.fields.is_empty());
    assert_eq!(old_mesh.colors, mesh.colors);
    // damaged field sections are errors, never panics
    for cut in (old_len..bytes.len()).step_by(3) {
        assert!(Mesh::decode(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    let mut bad = bytes.clone();
    let at = old_len.div_ceil(4) * 4;
    bad[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Mesh::decode(&bad).is_err());
    let mut bad = bytes;
    bad[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Mesh::decode(&bad).is_err());
}

#[test]
fn hostile_headers_are_errors() {
    let cases: &[(&str, &[u8])] = &[
        ("huge point count", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 4000000000 float\n0 0 0\n"),
        ("huge binary points", b"# vtk DataFile Version 3.0\nx\nBINARY\nDATASET UNSTRUCTURED_GRID\nPOINTS 1000000 float\n\0\0\0\0"),
        ("huge cells", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 1 float\n0 0 0\nCELLS 18446744073709551615 18446744073709551615\n1 0\n"),
        ("cell count overflows", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 1 float\n0 0 0\nCELLS 1 2\n4000000000 0\n"),
        ("missing point", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 3 float\n0 0 0 1 0 0 0 1 0\nCELLS 1 4\n3 0 1 7\nCELL_TYPES 1\n5\n"),
        ("negative index", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 3 float\n0 0 0 1 0 0 0 1 0\nCELLS 1 4\n3 0 1 -1\nCELL_TYPES 1\n5\n"),
        ("NaN coordinate", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 3 float\nnan 0 0 1 0 0 0 1 0\nCELLS 1 4\n3 0 1 2\nCELL_TYPES 1\n5\n"),
        ("huge components", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 1 float\n0 0 0\nPOINT_DATA 1\nSCALARS s float 99999999999\n"),
        ("structured", b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET STRUCTURED_POINTS\n"),
        ("no header", b"POINTS 1 float\n0 0 0\n"),
        ("binary garbage header", b"# vtk DataFile Version 3.0\nx\nBINARY\nDATASET POLYDATA\n\xff\xfe\n"),
    ];
    for (name, data) in cases {
        assert!(vtk_legacy(data).and_then(|m| m.check()).is_err(), "{name}");
    }
    let xml_cases: &[(&str, &[u8])] = &[
        ("offset outside", br#"<VTKFile type="UnstructuredGrid"><UnstructuredGrid><Piece NumberOfPoints="1" NumberOfCells="0"><Points><DataArray type="Float32" NumberOfComponents="3" format="appended" offset="999999"/></Points></Piece></UnstructuredGrid><AppendedData encoding="raw">_abcd</AppendedData></VTKFile>"#),
        ("raw header too large", b"<VTKFile type=\"UnstructuredGrid\"><UnstructuredGrid><Piece NumberOfPoints=\"1\" NumberOfCells=\"0\"><Points><DataArray type=\"Float32\" NumberOfComponents=\"3\" format=\"appended\" offset=\"0\"/></Points></Piece></UnstructuredGrid><AppendedData encoding=\"raw\">_\xff\xff\xff\x7f\0\0</AppendedData></VTKFile>"),
        ("zlib block bomb", br#"<VTKFile type="UnstructuredGrid" compressor="vtkZLibDataCompressor"><UnstructuredGrid><Piece NumberOfPoints="1" NumberOfCells="0"><Points><DataArray type="Float32" NumberOfComponents="3" format="binary">/////wAAAAAAAAAA</DataArray></Points></Piece></UnstructuredGrid></VTKFile>"#),
        ("lz4", br#"<VTKFile type="UnstructuredGrid" compressor="vtkLZ4DataCompressor"></VTKFile>"#),
        ("image data", br#"<VTKFile type="ImageData"><ImageData/></VTKFile>"#),
        ("huge piece", br#"<VTKFile type="UnstructuredGrid"><UnstructuredGrid><Piece NumberOfPoints="999999999999" NumberOfCells="0"/></UnstructuredGrid></VTKFile>"#),
        ("ascii too short", br#"<VTKFile type="UnstructuredGrid"><UnstructuredGrid><Piece NumberOfPoints="5" NumberOfCells="0"><Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0</DataArray></Points></Piece></UnstructuredGrid></VTKFile>"#),
        ("bad base64", br#"<VTKFile type="UnstructuredGrid"><UnstructuredGrid><Piece NumberOfPoints="1" NumberOfCells="0"><Points><DataArray type="Float32" NumberOfComponents="3" format="binary">!!!!</DataArray></Points></Piece></UnstructuredGrid></VTKFile>"#),
        ("offsets beyond connectivity", br#"<VTKFile type="UnstructuredGrid"><UnstructuredGrid><Piece NumberOfPoints="3" NumberOfCells="1"><Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0 1 0 0 0 1 0</DataArray></Points><Cells><DataArray type="Int32" Name="connectivity" format="ascii">0 1 2</DataArray><DataArray type="Int32" Name="offsets" format="ascii">4000000000</DataArray><DataArray type="UInt8" Name="types" format="ascii">5</DataArray></Cells></Piece></UnstructuredGrid></VTKFile>"#),
        ("not xml", b"\x00\x01\x02"),
    ];
    for (name, data) in xml_cases {
        assert!(vtk_xml(data).and_then(|m| m.check()).is_err(), "{name}");
    }
    let frd_cases: &[(&str, &[u8])] = &[
        ("element with a missing node", b"    2C 1 1\n -1         1 0.00000E+00 0.00000E+00 0.00000E+00\n -3\n    3C 1 1\n -1         1    3    0    1\n -2         1         2         3         9\n -3\n"),
        ("too many components", b"    2C 1 1\n -1         1 0.00000E+00 0.00000E+00 0.00000E+00\n -3\n -4  X        9999    1\n"),
        ("NaN node", b"    2C 1 1\n -1         1         NaN 0.00000E+00 0.00000E+00\n -3\n"),
        ("only nodes, wrong lines", b"    2C 1 1\nrubbish\n"),
    ];
    for (name, data) in frd_cases {
        assert!(frd(data).and_then(|m| m.check()).is_err(), "{name}");
    }
}

/// Every prefix and many corrupted copies of every fixture: errors are
/// fine, panics, hangs and huge allocations are not.
#[test]
fn damaged_fixtures_never_panic() {
    type Reader = fn(&[u8]) -> Result<Mesh>;
    let files: &[(&str, Reader)] = &[
        ("beam-ascii.vtk", vtk_legacy),
        ("beam-binary.vtk", vtk_legacy),
        ("beam-ascii.vtu", vtk_xml),
        ("beam-base64.vtu", vtk_xml),
        ("beam-appended-raw.vtu", vtk_xml),
        ("beam-zlib.vtu", vtk_xml),
        ("beam.frd", frd),
    ];
    let mut seed = 0x2545_f491_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for (name, read) in files {
        let data = fixture(name);
        for cut in (0..data.len()).step_by(5) {
            if let Ok(mesh) = read(&data[..cut]) {
                let _ = mesh.check();
            }
        }
        for _ in 0..300 {
            let mut copy = data.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() as usize) % copy.len();
                copy[at] = match next() % 4 {
                    0 => b'9',
                    1 => 0xff,
                    2 => b' ',
                    _ => next() as u8,
                };
            }
            if let Ok(mesh) = read(&copy) {
                let _ = mesh.check();
                let _ = Mesh::decode(&mesh.encode());
            }
        }
    }
}

/// Timing of a large volume mesh (100³ hexahedra, 1 M cells):
/// `cargo test --release --lib large_hex_grid -- --ignored --nocapture`.
#[test]
#[ignore = "timing, run in release mode"]
fn large_hex_grid() {
    let n = 100u32;
    let p = n + 1;
    let id = |i: u32, j: u32, k: u32| i + p * (j + p * k);
    let mut g = Grid::default();
    for k in 0..p {
        for j in 0..p {
            for i in 0..p {
                g.points.extend([i as f32, j as f32, k as f32]);
            }
        }
    }
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                g.conn.extend([
                    id(i, j, k),
                    id(i + 1, j, k),
                    id(i + 1, j + 1, k),
                    id(i, j + 1, k),
                    id(i, j, k + 1),
                    id(i + 1, j, k + 1),
                    id(i + 1, j + 1, k + 1),
                    id(i, j + 1, k + 1),
                ]);
                g.ends.push(g.conn.len());
                g.types.push(HEXAHEDRON);
            }
        }
    }
    g.point_fields.push(Field {
        name: "x".into(),
        unit: String::new(),
        values: (0..p * p * p).map(|v| (v % p) as f32).collect(),
    });
    let started = std::time::Instant::now();
    let mesh = g.into_mesh().unwrap();
    println!(
        "{} triangles, {} vertices in {:?}",
        mesh.triangle_count(),
        mesh.vertex_count(),
        started.elapsed()
    );
    assert_eq!(mesh.triangle_count(), 6 * 2 * (n * n) as usize);
}

// ---------------------------------------------------------------------------
// review findings
// ---------------------------------------------------------------------------

/// The last value of an ASCII file may end the file (no line end).
#[test]
fn legacy_ascii_without_final_line_end() {
    let data = b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 3 float\n0 0 0 1 0 0 0 1 0\nCELLS 1 4\n3 0 1 2\nCELL_TYPES 1\n5";
    assert_eq!(vtk_legacy(data).unwrap().triangle_count(), 1);
    let data = b"# vtk DataFile Version 3.0\nx\nASCII\nDATASET POLYDATA\nPOINTS 3 float\n0 0 0 1 0 0 0 1 0\nPOLYGONS 1 4\n3 0 1 2\nPOINT_DATA 3\nSCALARS t float 1\nLOOKUP_TABLE default\n1 2 3";
    let mesh = vtk_legacy(data).unwrap();
    assert_eq!(field(&mesh, "t").range(), (1.0, 3.0));
}

/// A hexahedron collapsed to a wedge (nodes 2 = 3, 6 = 7) next to a tetra:
/// the shared triangle is inside and must not be drawn.
#[test]
fn collapsed_cells_pair_with_their_neighbours() {
    let mut g = Grid {
        points: vec![
            0., 0., 0., 1., 0., 0., 0.5, 1., 0., // 0 1 2
            0., 0., 1., 1., 0., 1., 0.5, 1., 1., // 3 4 5 (top)
            0.5, 0.4, -1., // 6: apex below
        ],
        ..Grid::default()
    };
    for (ty, nodes) in [
        (HEXAHEDRON, &[0u32, 1, 2, 2, 3, 4, 5, 5][..]),
        (TETRA, &[0, 1, 2, 6][..]),
    ] {
        g.conn.extend_from_slice(nodes);
        g.ends.push(g.conn.len());
        g.types.push(ty);
    }
    let mesh = g.into_mesh().unwrap();
    // wedge: 3 quads + top triangle = 7, tetra: 3 outer faces
    assert_eq!(mesh.triangle_count(), 10);
}

/// An empty first piece (parallel writers emit those) keeps the fields of
/// the pieces after it.
#[test]
fn empty_first_piece_keeps_the_fields() {
    let vtu = br#"<VTKFile type="UnstructuredGrid">
<UnstructuredGrid>
<Piece NumberOfPoints="0" NumberOfCells="0"></Piece>
<Piece NumberOfPoints="3" NumberOfCells="1">
<PointData><DataArray type="Float32" Name="t" format="ascii">1 2 3</DataArray></PointData>
<CellData><DataArray type="Float32" Name="m" format="ascii">7</DataArray></CellData>
<Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0 1 0 0 0 1 0</DataArray></Points>
<Cells>
<DataArray type="Int32" Name="connectivity" format="ascii">0 1 2</DataArray>
<DataArray type="Int32" Name="offsets" format="ascii">3</DataArray>
<DataArray type="UInt8" Name="types" format="ascii">5</DataArray>
</Cells>
</Piece>
</UnstructuredGrid>
</VTKFile>"#;
    let mesh = vtk_xml(vtu).unwrap();
    assert_eq!(field(&mesh, "t").range(), (1.0, 3.0));
    assert_eq!(field(&mesh, "m (cells)").range(), (7.0, 7.0));
}

/// Thousands of differently named FRD result blocks keep at most
/// MAX_FIELDS value arrays, and STRESS still wins.
#[test]
fn frd_many_result_blocks_are_bounded() {
    let mut f = String::new();
    f.push_str("    2C                             3                                     1\n");
    f.push_str(" -1         1 0.00000E+00 0.00000E+00 0.00000E+00\n");
    f.push_str(" -1         2 1.00000E+00 0.00000E+00 0.00000E+00\n");
    f.push_str(" -1         3 0.00000E+00 1.00000E+00 0.00000E+00\n");
    f.push_str(" -3\n");
    f.push_str("    3C                             1                                     1\n");
    f.push_str(" -1         1    7    0    1\n");
    f.push_str(" -2         1         2         3\n");
    f.push_str(" -3\n");
    let block = |name: &str| {
        format!(" -4  {name:<8}    1    1\n -5  V           1    1    0    0\n -1         1 1.00000E+00\n -3\n")
    };
    for i in 0..2000 {
        f.push_str(&block(&format!("R{i}")));
    }
    f.push_str(&block("STRESS"));
    let mesh = frd(f.as_bytes()).unwrap();
    assert_eq!(mesh.triangle_count(), 1);
    assert!(mesh.fields.len() <= MAX_FIELDS);
    assert_eq!(mesh.fields[0].name, "STRESS");
    assert_eq!(mesh.fields[1].name, "R0", "the first blocks stay");
}

/// Many appended arrays: the end of each one is found by binary search.
#[test]
fn many_appended_arrays_are_fast() {
    let pieces = 20_000;
    let mut xml = String::from(r#"<VTKFile type="PolyData"><PolyData>"#);
    let mut raw = Vec::new();
    for _ in 0..pieces {
        xml.push_str(&format!(
            r#"<Piece NumberOfPoints="1"><Points><DataArray type="Float32" NumberOfComponents="3" format="appended" offset="{}"/></Points></Piece>"#,
            raw.len()
        ));
        raw.extend_from_slice(&12u32.to_le_bytes());
        raw.extend_from_slice(&[0u8; 12]);
    }
    xml.push_str(r#"</PolyData><AppendedData encoding="raw">_"#);
    let mut data = xml.into_bytes();
    data.extend_from_slice(&raw);
    data.extend_from_slice(b"</AppendedData></VTKFile>");
    let started = std::time::Instant::now();
    let mesh = vtk_xml(&data).unwrap();
    assert_eq!(mesh.vertex_count(), pieces);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

/// A zlib header that promises a huge array (1.2 GB of connectivity for a
/// tiny file) is an error without reserving that memory first.
#[test]
fn lying_zlib_header_is_an_error() {
    // UInt32 header: 1 block of 1.2 GB, compressed size 8, then garbage
    let mut bin = Vec::new();
    for v in [1u32, 1_200_000_000, 0, 8] {
        bin.extend_from_slice(&v.to_le_bytes());
    }
    bin.extend_from_slice(&[0x78, 0x9c, 0, 0, 0, 0, 0, 0]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bin);
    let vtu = format!(
        r#"<VTKFile type="UnstructuredGrid" compressor="vtkZLibDataCompressor"><UnstructuredGrid><Piece NumberOfPoints="1" NumberOfCells="1"><Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0</DataArray></Points><Cells><DataArray type="Int64" Name="offsets" format="ascii">150000000</DataArray><DataArray type="Int64" Name="connectivity" format="binary">{b64}</DataArray><DataArray type="UInt8" Name="types" format="ascii">7</DataArray></Cells></Piece></UnstructuredGrid></VTKFile>"#
    );
    assert!(vtk_xml(vtu.as_bytes()).is_err());
}
