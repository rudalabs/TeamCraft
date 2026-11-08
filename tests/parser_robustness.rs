//! Hostile and damaged input for every reader of untrusted files. Small
//! hand-made samples are cut at every length and corrupted byte by byte
//! (a deterministic generator, no fuzzing engine): the readers must return
//! an error or a valid result, never panic, hang or allocate what the file
//! only claims. No database needed.
//!
//! The samples also pin what the readers *do* accept, so that a mutation
//! test cannot pass by rejecting everything.

use std::io::Cursor;

use teamcraft::{bomtable, cadformat, cadrefs, htmltext, mesh, nxpart, stepbom, swpart, textindex};

/// xorshift64: the same corruptions on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Every prefix of `sample`, then `flips` copies with a few random bytes
/// replaced (interesting values first), each handed to `f`.
fn damage(sample: &[u8], flips: usize, mut f: impl FnMut(&[u8])) {
    for n in 0..=sample.len() {
        f(&sample[..n]);
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ sample.len() as u64);
    const SPECIAL: [u8; 6] = [0x00, 0xFF, 0x7F, 0x80, b'-', b'9'];
    for _ in 0..flips {
        let mut copy = sample.to_vec();
        for _ in 0..1 + rng.next() % 4 {
            if copy.is_empty() {
                break;
            }
            let at = (rng.next() % copy.len() as u64) as usize;
            copy[at] = if rng.next() % 2 == 0 {
                SPECIAL[(rng.next() % 6) as usize]
            } else {
                rng.next() as u8
            };
        }
        f(&copy);
    }
}

fn stl_binary(triangles: u32, body: usize) -> Vec<u8> {
    let mut v = vec![0u8; 80];
    v.extend(triangles.to_le_bytes());
    v.extend(std::iter::repeat_n(0u8, body));
    v
}

const STL_ASCII: &str = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\n\
    vertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
const OBJ: &str =
    "# c\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nvn 0 0 1\nf 1 2 3\nf 1//1 3//1 4//1\nf -4 -3 -2 -1\n";
const PLY: &str = "ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\n\
    property float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n\
    0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n";
const OFF: &str = "OFF\n3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n";
const AMF: &str = "<?xml version=\"1.0\"?><amf><object id=\"0\"><mesh><vertices>\
    <vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex>\
    <vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex>\
    <vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex></vertices>\
    <volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object></amf>";
const GLTF: &str = "{\"asset\":{\"version\":\"2.0\"},\"buffers\":[{\"byteLength\":36,\"uri\":\
    \"data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA\"}],\
    \"bufferViews\":[{\"buffer\":0,\"byteLength\":36}],\"accessors\":[{\"bufferView\":0,\
    \"componentType\":5126,\"count\":3,\"type\":\"VEC3\",\"min\":[0,0,0],\"max\":[1,1,0]}],\"meshes\":[{\"primitives\":\
    [{\"attributes\":{\"POSITION\":0}}]}],\"nodes\":[{\"mesh\":0}],\"scenes\":[{\"nodes\":[0]}]}";
const PTS: &str = "3\n0 0 0\n1 0 0\n0 1 0\n";
const VTK: &str = "# vtk DataFile Version 3.0\nt\nASCII\nDATASET UNSTRUCTURED_GRID\n\
    POINTS 4 float\n0 0 0 1 0 0 0 1 0 0 0 1\nCELLS 1 5\n4 0 1 2 3\nCELL_TYPES 1\n10\n";
const VTU: &str = "<VTKFile type=\"UnstructuredGrid\" version=\"0.1\"><UnstructuredGrid>\
    <Piece NumberOfPoints=\"4\" NumberOfCells=\"1\"><Points><DataArray type=\"Float32\" \
    NumberOfComponents=\"3\" format=\"ascii\">0 0 0 1 0 0 0 1 0 0 0 1</DataArray></Points>\
    <Cells><DataArray type=\"Int32\" Name=\"connectivity\" format=\"ascii\">0 1 2 3</DataArray>\
    <DataArray type=\"Int32\" Name=\"offsets\" format=\"ascii\">4</DataArray>\
    <DataArray type=\"UInt8\" Name=\"types\" format=\"ascii\">10</DataArray></Cells>\
    </Piece></UnstructuredGrid></VTKFile>";

/// Samples whose validity is not asserted (only that damage never panics).
fn lenient_samples() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("stl", STL_ASCII.as_bytes().to_vec()),
        ("stl", stl_binary(1, 50)),
        ("obj", OBJ.as_bytes().to_vec()),
        ("ply", PLY.as_bytes().to_vec()),
        ("off", OFF.as_bytes().to_vec()),
        ("amf", AMF.as_bytes().to_vec()),
        ("gltf", GLTF.as_bytes().to_vec()),
        ("pts", PTS.as_bytes().to_vec()),
        ("xyz", b"0 0 0\n1 0 0\n0 1 0 255 0 0\n".to_vec()),
        ("vtk-legacy", VTK.as_bytes().to_vec()),
        ("vtk-unstructured", VTU.as_bytes().to_vec()),
    ]
}

/// The samples of the simple formats are valid meshes (so the damage tests
/// start from something real) and unknown formats are refused.
#[test]
fn the_samples_are_valid_meshes() {
    for (format, data) in lenient_samples() {
        match mesh::read(format, &data) {
            Ok(m) => {
                assert!(m.vertex_count() >= 1, "{format}");
                m.check().unwrap();
            }
            Err(e) => panic!("{format} sample refused: {e}"),
        }
    }
    assert!(mesh::read("dwg", b"AC1015").is_err());
}

#[test]
fn damaged_mesh_files_never_panic() {
    for (format, data) in lenient_samples() {
        damage(&data, 400, |d| {
            if let Ok(m) = mesh::read(format, d) {
                // whatever is accepted must be a consistent mesh
                m.check().unwrap_or_else(|e| panic!("{format}: {e}"));
                let n = m.vertex_count() as u32;
                assert!(m.indices.iter().all(|&i| i < n), "{format}: index range");
                assert!(m.indices.len() % 3 == 0, "{format}");
                // and survives the cache format
                let round = mesh::Mesh::decode(&m.encode()).unwrap();
                assert_eq!(round.positions, m.positions, "{format}");
                assert_eq!(round.indices, m.indices, "{format}");
            }
        });
    }
}

/// Random bytes behind the magic numbers of the binary formats.
#[test]
fn binary_mesh_formats_survive_random_bodies() {
    let mut rng = Rng(42);
    let heads: [(&str, &[u8]); 4] = [
        ("glb", b"glTF\x02\x00\x00\x00"),
        ("3ds", b"\x4D\x4D\x20\x00\x00\x00"),
        ("las", b"LASF"),
        ("3mf", b"PK\x03\x04"),
    ];
    for (format, head) in heads {
        for _ in 0..200 {
            let mut d = head.to_vec();
            let len = (rng.next() % 300) as usize;
            d.extend((0..len).map(|_| rng.next() as u8));
            let _ = mesh::read(format, &d);
        }
        let _ = mesh::read(format, head);
        let _ = mesh::read(format, b"");
    }
}

/// A header that claims far more than the file holds is refused before
/// anything is allocated for it.
#[test]
fn claimed_sizes_beyond_the_file_are_refused() {
    // binary STL: 4 billion triangles, 50 bytes of data
    assert!(mesh::read("stl", &stl_binary(u32::MAX, 50)).is_err());
    assert!(mesh::read("stl", &stl_binary(mesh::MAX_TRIANGLES as u32 + 1, 0)).is_err());
    // PLY: a billion vertices and no data
    let ply = "ply\nformat ascii 1.0\nelement vertex 1000000000\nproperty float x\n\
        property float y\nproperty float z\nend_header\n0 0 0\n";
    assert!(mesh::read("ply", ply.as_bytes()).is_err());
    // OFF: counts that do not match the body
    assert!(mesh::read("off", b"OFF\n4000000000 1 0\n0 0 0\n").is_err());
    // point cloud: the point count line lies
    let _ = mesh::read("pts", b"2000000000\n0 0 0\n");
}

#[test]
fn faces_pointing_outside_the_vertices_never_yield_a_bad_mesh() {
    let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 99\n";
    if let Ok(m) = mesh::read("obj", obj.as_bytes()) {
        let n = m.vertex_count() as u32;
        assert!(m.indices.iter().all(|&i| i < n));
    }
    let off = "OFF\n3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 7\n";
    if let Ok(m) = mesh::read("off", off.as_bytes()) {
        let n = m.vertex_count() as u32;
        assert!(m.indices.iter().all(|&i| i < n));
    }
}

#[test]
fn non_finite_coordinates_never_reach_the_mesh() {
    for bad in ["NaN", "inf", "-inf", "1e999"] {
        let obj = format!("v 0 0 0\nv 1 0 0\nv 0 {bad} 0\nf 1 2 3\n");
        if let Ok(m) = mesh::read("obj", obj.as_bytes()) {
            assert!(m.positions.iter().all(|p| p.is_finite()), "{bad}");
        }
    }
}

// ---------------------------------------------------------------------------
// BOM tables and STEP structure
// ---------------------------------------------------------------------------

const CSV: &str = "Level;Part Number;Name;Qty;Unit\n0;TOP;Top;1;\n1;A-1;Plate;2,5;pcs\n\
    2;B-1;Screw;4;\n";

#[test]
fn damaged_bom_tables_never_panic() {
    let t = bomtable::read("bom.csv", CSV.as_bytes()).unwrap();
    assert_eq!(t.root.as_ref().unwrap().number, "TOP");
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[0].quantity, 2.5, "decimal comma");
    for name in [
        "bom.csv", "bom.tsv", "bom.xlsx", "bom.xls", "bom.ods", "bom.txt",
    ] {
        damage(CSV.as_bytes(), 200, |d| {
            if let Ok(t) = bomtable::read(name, d) {
                assert!(t.rows.iter().all(|r| r.quantity.is_finite()), "{name}");
                assert!(t.rows.iter().all(|r| r.line >= 1), "{name}");
            }
        });
    }
}

#[test]
fn bom_tables_without_content_have_no_rows() {
    for data in ["", "\n\n", "Name;Qty\n", ";;;\n;;;\n"] {
        if let Ok(t) = bomtable::read("x.csv", data.as_bytes()) {
            assert!(t.rows.is_empty() && t.root.is_none(), "{data:?}");
        }
    }
    // binary junk with a spreadsheet extension
    assert!(bomtable::read("x.xlsx", b"PK\x03\x04junk").is_err());
    assert!(bomtable::read("x.ods", &[0u8; 64]).is_err());
}

const STEP: &str = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n\
    #11=PRODUCT('TOP','Top','',(#9));\n#12=PRODUCT_DEFINITION_FORMATION('','',#11);\n\
    #13=PRODUCT_DEFINITION('design','',#12,#8);\n\
    #21=PRODUCT('P-1','Part','',(#9));\n#22=PRODUCT_DEFINITION_FORMATION('','',#21);\n\
    #23=PRODUCT_DEFINITION('design','',#22,#8);\n\
    #31=NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','','',#13,#23,$);\nENDSEC;\nEND-ISO-10303-21;\n";

#[test]
fn damaged_step_structure_never_panics() {
    let t = stepbom::read(STEP.as_bytes()).unwrap();
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0].number, "P-1");
    damage(STEP.as_bytes(), 600, |d| {
        if let Ok(t) = stepbom::read(d) {
            assert!(t.rows.iter().all(|r| r.quantity >= 1.0 && r.level >= 1));
        }
    });
    // a reference that points to nothing, and to itself
    let broken = STEP.replace("#13,#23,$", "#13,#999,$");
    let _ = stepbom::read(broken.as_bytes());
    let selfref = STEP.replace("#13,#23,$", "#13,#13,$");
    let _ = stepbom::read(selfref.as_bytes());
}

// ---------------------------------------------------------------------------
// Text index, reference scan, containers
// ---------------------------------------------------------------------------

#[test]
fn text_extraction_survives_any_bytes() {
    let mut rng = Rng(7);
    for name in [
        "a.txt", "a.step", "a.iges", "a.csv", "a.md", "a.pdf", "a.bin", "",
    ] {
        for len in [0usize, 1, 3, 64, 4096] {
            let d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let _ = textindex::extract(name, &d);
        }
    }
    damage(STEP.as_bytes(), 100, |d| {
        let _ = textindex::extract("a.step", d);
    });
    let html: &[u8] = b"<html><head><meta charset=windows-1252><style>p{}</style></head><body>\
        <table><tr><th>R</th><td>1.5&nbsp;MPa</td></tr></table><!-- c --><img alt=\"x\" \
        src=\"data:,AA\"><p hidden>h</p>&#x110000;\xb0</body></html>";
    damage(html, 100, |d| {
        let _ = htmltext::extract(d);
    });
    for len in [0usize, 1, 3, 64, 4096] {
        let d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = htmltext::extract(&d);
    }
    for q in [
        "", "  ", "a&b", "a | b", "!x", "(x)", "x:*", "'; DROP", "\u{0}", "ä ö ü",
    ] {
        let _ = textindex::prefix_query(q);
    }
}

#[test]
fn reference_scan_survives_any_bytes() {
    let mut rng = Rng(99);
    for _ in 0..300 {
        let len = (rng.next() % 600) as usize;
        let d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        for r in cadrefs::find(&d, "own.sldasm") {
            assert!(!r.is_empty() && !r.contains(['/', '\\']), "{r:?}");
        }
    }
}

#[test]
fn damaged_containers_never_panic() {
    // the NX container's magic and a footer pointer that leads nowhere
    let mut nx = b"SPLMSSTR".to_vec();
    nx.resize(512, 0);
    assert!(nxpart::is_nx_part(&nx));
    damage(&nx, 300, |d| {
        for s in &nxpart::streams(d) {
            if let Some(b) = nxpart::data(d, s) {
                assert!(b.len() <= d.len());
            }
        }
        let _ = nxpart::reference_streams_in(d);
        let _ = nxpart::reference_streams(Cursor::new(d), 1 << 20);
    });
    // SOLIDWORKS: the NIST files in `swpart`'s unit tests are the valid
    // side; the magic with garbage must not panic
    let mut rng = Rng(5);
    for _ in 0..300 {
        let len = (rng.next() % 2000) as usize;
        let mut d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        if d.len() >= 4 && rng.next() % 2 == 0 {
            d[..4].copy_from_slice(&[0x14, 0x00, 0x06, 0x00]);
        }
        let _ = swpart::is_container(&d);
        let _ = swpart::SwContainer::open(Cursor::new(&d));
        let _ = swpart::reference_data(Cursor::new(&d), 1 << 20);
    }
    // inflate: a size and checksum that do not fit the data are refused
    let r = swpart::inflate(Cursor::new(vec![0xFFu8; 16]), 1 << 40, 0);
    assert!(!matches!(r, Ok(Some(_))));
}

#[test]
fn component_trees_in_broken_xml_never_panic() {
    let xml = "<Tree><Model id=\"1\" file=\"top.sldasm\"/><Inst model=\"1\" ref=\"2\"/></Tree>";
    damage(xml.as_bytes(), 200, |d| {
        let _ = swpart::components(d);
    });
    for junk in ["", "<", "<<<>>>", "\u{feff}<a>", "<a b='"] {
        let _ = swpart::components(junk.as_bytes());
    }
}

#[test]
fn thumbnail_extraction_survives_damaged_files() {
    use teamcraft::thumbnail;
    let dir = std::env::temp_dir().join(format!("tc-robust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut rng = Rng(11);
    let heads: [&[u8]; 4] = [
        b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
        b"SPLMSSTR",
        b"\x14\x00\x06\x00",
        b"\x89PNG\r\n\x1a\n",
    ];
    for (i, head) in heads.iter().enumerate() {
        for k in 0..60 {
            let mut d = head.to_vec();
            let len = (rng.next() % 1500) as usize;
            d.extend((0..len).map(|_| rng.next() as u8));
            let p = dir.join(format!("f{i}-{k}.bin"));
            std::fs::write(&p, &d).unwrap();
            let _ = thumbnail::extract(&p);
        }
    }
    let empty = dir.join("empty");
    std::fs::write(&empty, b"").unwrap();
    assert!(matches!(thumbnail::extract(&empty), Ok(None)));
    assert!(thumbnail::extract(&dir.join("missing")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_detection_survives_odd_names_and_heads() {
    let names = [
        "",
        ".",
        "..",
        ".step",
        "a.",
        "a..b",
        "ä.stp",
        "A.STEP",
        "x.prt.1",
        "x.prt.999999999999",
        "C:\\a\\b.sldprt",
        "a b.ipt",
        "\u{0}.dwg",
    ];
    for n in names {
        let _ = cadformat::extension(n);
        let _ = cadformat::normalize_name(n);
        let _ = cadformat::creo_iteration(n);
        let _ = cadformat::detect(n, b"");
        let _ = cadformat::detect(n, b"\x00\xFF\xD0\xCF\x11\xE0");
        let _ = cadformat::content_type(None, n);
    }
    assert_eq!(cadformat::extension("A.STEP").as_deref(), Some("step"));
    assert_eq!(cadformat::extension("noext"), None);
}

/// Found by the damage tests: a primitive naming an accessor that does not
/// exist made the glTF library panic while validating.
#[test]
fn gltf_accessor_index_out_of_range_is_an_error() {
    let bad = GLTF.replace("\"POSITION\":0", "\"POSITION\":7");
    assert!(mesh::read("gltf", bad.as_bytes()).is_err());
}
