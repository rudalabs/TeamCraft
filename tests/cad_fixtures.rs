//! Real CAD files from NIST and the MIT-licensed Rhino models of rhino3dm
//! (`tests/fixtures/cad`, see its README): the
//! copies are unchanged, TeamCraft recognises their formats, finds the
//! preview images in Inventor, NX and SOLIDWORKS files and the parts of
//! the NX and SOLIDWORKS assemblies, and turns every STEP file (AP203 and
//! AP242) into a complete 3D preview. No database needed.
//!
//! Gaps found with these files are kept as ignored tests – run them with
//! `cargo test --test cad_fixtures -- --ignored`; see *Fixes* in
//! `docs/OPEN_WORK.md`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use teamcraft::{cadformat, cadrefs, mesh, preview, thumbnail};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cad")
}

fn fixture(rel: &str) -> PathBuf {
    root().join("nist").join(rel)
}

struct Entry {
    sha256: String,
    size: u64,
    path: PathBuf,
    format: String,
}

/// `manifest.tsv`: sha256, size, path, TeamCraft format id, origin.
fn manifest() -> Vec<Entry> {
    std::fs::read_to_string(root().join("manifest.tsv"))
        .expect("manifest.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            Entry {
                sha256: c[0].to_string(),
                size: c[1].parse().expect("size"),
                path: root().join(c[2]),
                format: c[3].to_string(),
            }
        })
        .collect()
}

#[tokio::test]
async fn files_are_unchanged_and_recognised() {
    let entries = manifest();
    assert_eq!(entries.len(), 35, "every fixture is listed");
    for e in &entries {
        let shown = e.path.display();
        let data = std::fs::read(&e.path).unwrap_or_else(|err| panic!("{shown}: {err}"));
        assert_eq!(data.len() as u64, e.size, "{shown}");
        assert_eq!(
            hex::encode(Sha256::digest(&data)),
            e.sha256,
            "{shown} changed"
        );
        let name = e.path.file_name().unwrap().to_string_lossy().to_string();
        let found = cadformat::detect_stored(&e.path, &name).await.map(|f| f.id);
        assert_eq!(found, Some(e.format.as_str()), "{shown}");
    }
}

#[test]
fn inventor_files_carry_a_preview_image() {
    for rel in [
        "ctc-01/inventor-2021/INV_nist_ctc_01_asme1_2021.ipt",
        "ftc-11/inventor-2021/INV_nist_ftc_11_asme1_2021.ipt",
    ] {
        let image = thumbnail::extract(&fixture(rel))
            .unwrap()
            .unwrap_or_else(|| panic!("{rel}: no preview image"));
        assert_eq!(image.content_type, "image/png", "{rel}");
        assert!(
            image.bytes.starts_with(b"\x89PNG") && image.bytes.len() > 10_000,
            "{rel}"
        );
    }
}

#[test]
fn nx_parts_carry_a_preview_image() {
    let dir = fixture("mtc-assembly/nx");
    let mut parts: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "prt"))
        .collect();
    parts.push(fixture("ctc-01/nx-1980/nist_ctc_01_asme1_nx1980_rd.prt"));
    assert!(parts.len() >= 10);
    for path in parts {
        let image = thumbnail::extract(&path)
            .unwrap()
            .unwrap_or_else(|| panic!("{}: no preview image", path.display()));
        assert_eq!(image.content_type, "image/jpeg", "{}", path.display());
        assert!(image.bytes.starts_with(&[0xFF, 0xD8, 0xFF]));
        assert!(image.bytes.len() > 2_000, "{}", path.display());
    }
    // A JT file is not an NX part container.
    assert!(thumbnail::extract(&dir.join("NIST mtc crada assembly.jt"))
        .unwrap()
        .is_none_or(|t| t.content_type != "image/jpeg"));
}

#[tokio::test]
async fn the_nx_assembly_references_its_parts() {
    let dir = fixture("mtc-assembly/nx");
    let name = "NIST mtc crada assembly.prt";
    let refs = cadrefs::scan_file(&dir.join(name), name, "nx-part").await;
    for part in [
        "NIST mtc crada box.prt",
        "NIST mtc crada cover.prt",
        "NIST mtc crada plate.prt",
        "NIST mtc crada polycarb.prt",
        "90591A141 HEX NUT.prt",
        "91274A118 SHCS.prt",
        "91274A141 SHCS.prt",
        "91304A112 FHSCS.prt",
    ] {
        assert!(
            refs.iter().any(|r| r.name == part),
            "{part} missing in {refs:?}"
        );
        assert!(dir.join(part).is_file(), "{part} is part of the fixture");
    }
    // How often the assembly places each part (#247) – the same counts as
    // its SOLIDWORKS version.
    let count = |part: &str| refs.iter().find(|r| r.name == part).unwrap().instances;
    assert_eq!(count("90591A141 HEX NUT.prt"), Some(10));
    assert_eq!(count("91274A118 SHCS.prt"), Some(14));
    assert_eq!(count("91274A141 SHCS.prt"), Some(4));
    assert_eq!(count("91304A112 FHSCS.prt"), Some(4));
    assert_eq!(count("NIST mtc crada box.prt"), Some(1));
}

/// Every NX part gets a 3D preview from the JT tessellation NX stores in
/// it (`/Root/FastLoad/JT`): closed, consistently oriented, in millimetres,
/// refined along the stored normals. The assembly has no such stream and
/// says so (its preview image is shown instead).
#[test]
fn nx_parts_get_a_closed_3d_preview() {
    for (rel, max_extent_mm) in [
        ("ctc-01/nx-1980/nist_ctc_01_asme1_nx1980_rd.prt", 1000.0),
        ("mtc-assembly/nx/90591A141 HEX NUT.prt", 30.0),
        ("mtc-assembly/nx/91274A118 SHCS.prt", 60.0),
        ("mtc-assembly/nx/91274A141 SHCS.prt", 60.0),
        ("mtc-assembly/nx/91304A112 FHSCS.prt", 60.0),
        ("mtc-assembly/nx/NIST mtc crada box.prt", 1000.0),
        ("mtc-assembly/nx/NIST mtc crada cover.prt", 1000.0),
        ("mtc-assembly/nx/NIST mtc crada plate.prt", 1000.0),
        ("mtc-assembly/nx/NIST mtc crada polycarb.prt", 1000.0),
    ] {
        assert_eq!(
            preview::PreviewFormat::of(rel),
            Some(preview::PreviewFormat::NxPart)
        );
        let mesh = preview::convert(preview::PreviewFormat::NxPart, &fixture(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
        mesh.check().unwrap();
        // LOD 0 has 1 350 to 6 450 triangles; refined 3 × 3
        assert!(
            mesh.triangle_count() > 9_000,
            "{rel}: {}",
            mesh.triangle_count()
        );
        // every edge once in each direction: closed and consistently oriented
        let mut edges = std::collections::HashMap::<(u32, u32), u32>::new();
        for t in mesh.indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a, b)).or_default() += 1;
            }
        }
        assert!(
            edges
                .iter()
                .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1)),
            "{rel}: the mesh is not closed"
        );
        let b = mesh.bounds();
        let extent = (0..3).map(|i| b[i + 3] - b[i]).fold(0.0, f32::max);
        assert!(extent > 5.0 && extent < max_extent_mm, "{rel}: {b:?}");
        let s = teamcraft::shape::compute(&mesh).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert!(s.is_valid() && s.closed && s.volume > 0.0, "{rel}: {s:?}");
        let bytes = mesh.encode();
        assert_eq!(mesh::Mesh::decode(&bytes).unwrap(), mesh);
    }
    let err = preview::convert(
        preview::PreviewFormat::NxPart,
        &fixture("mtc-assembly/nx/NIST mtc crada assembly.prt"),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("no lightweight representation"),
        "{err}"
    );
}

/// Every edge once in each direction: closed and consistently oriented.
fn is_closed(mesh: &mesh::Mesh) -> bool {
    let mut edges = std::collections::HashMap::<(u32, u32), u32>::new();
    for t in mesh.indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    edges
        .iter()
        .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1))
}

/// SOLIDWORKS parts show the display tessellation they store, refined
/// along its normals; the NIST parts match their STEP exports.
#[test]
fn solidworks_parts_get_a_3d_preview() {
    // (file, closed, (min, max) in mm)
    let box_of = |min: [f32; 3], max: [f32; 3]| Some((min, max));
    for (rel, closed, bounds) in [
        (
            "ctc-01/solidworks-2018/nist_ctc_01_asme1_rd_sw1802.SLDPRT",
            true,
            box_of([-400.0, -225.0, -100.0], [400.0, 225.0, 50.0]),
        ),
        (
            "ftc-11/solidworks-2018/nist_ftc_11_asme1_rb_sw1802.SLDPRT",
            true,
            box_of([-31.5, -31.5, -1.5], [31.5, 31.5, 1.5]),
        ),
        (
            "mtc-assembly/solidworks/nist_mtc_crada_box_rev-D.SLDPRT",
            true,
            box_of([0.0, 0.0, -3.0], [152.4, 101.6, 70.0]),
        ),
        (
            "mtc-assembly/solidworks/90591A141_ZINC PLATED STEEL HEX NUT.SLDPRT",
            true,
            None,
        ),
        (
            "mtc-assembly/solidworks/91274A118_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT",
            true,
            None,
        ),
        (
            "mtc-assembly/solidworks/91274A141_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT",
            true,
            None,
        ),
        (
            "mtc-assembly/solidworks/91304A112_BLUE FLAT-HEAD SOCKET CAP SCREW.SLDPRT",
            true,
            None,
        ),
        (
            "mtc-assembly/solidworks/nist_mtc_crada_polycarb-window.SLDPRT",
            true,
            None,
        ),
        // two faces sample a common edge with different vertices (cracks of
        // zero width): shown, but not closed
        (
            "mtc-assembly/solidworks/nist_mtc_crada_cover_rev-B.SLDPRT",
            false,
            None,
        ),
        (
            "mtc-assembly/solidworks/nist_mtc_crada_plate_rev-A.SLDPRT",
            false,
            None,
        ),
    ] {
        assert_eq!(
            preview::PreviewFormat::of(rel),
            Some(preview::PreviewFormat::SwPart)
        );
        let mesh = preview::convert(preview::PreviewFormat::SwPart, &fixture(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
        mesh.check().unwrap();
        assert!(
            mesh.triangle_count() > 9_000,
            "{rel}: {}",
            mesh.triangle_count()
        );
        assert_eq!(is_closed(&mesh), closed, "{rel}");
        if closed {
            let s = teamcraft::shape::compute(&mesh).unwrap_or_else(|e| panic!("{rel}: {e}"));
            assert!(s.is_valid() && s.closed && s.volume > 0.0, "{rel}: {s:?}");
        }
        if let Some((min, max)) = bounds {
            let b = mesh.bounds();
            for i in 0..3 {
                assert!((b[i] - min[i]).abs() < 0.05, "{rel}: {b:?}");
                assert!((b[i + 3] - max[i]).abs() < 0.05, "{rel}: {b:?}");
            }
        }
    }
    let err = preview::convert(
        preview::PreviewFormat::SwAssembly,
        &fixture("mtc-assembly/solidworks/nist_mtc_crada_assembly_rev-D.SLDASM"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("previews of its parts"), "{err}");
}

/// A damaged display list is an error, never a panic: bytes all over a
/// real part are overwritten, and the file is cut short.
#[test]
fn damaged_solidworks_parts_never_panic() {
    let rel = "ftc-11/solidworks-2018/nist_ftc_11_asme1_rb_sw1802.SLDPRT";
    let bytes = std::fs::read(fixture(rel)).unwrap();
    for cut in (0..bytes.len()).step_by(bytes.len() / 40) {
        let _ = mesh::read("solidworks-part", &bytes[..cut]);
    }
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..40 {
        let mut damaged = bytes.clone();
        for _ in 0..16 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let at = (state % damaged.len() as u64) as usize;
            damaged[at] = (state >> 32) as u8;
        }
        if let Ok(mesh) = mesh::read("solidworks-part", &damaged) {
            mesh.check().unwrap();
        }
    }
}

/// A damaged JT stream is an error, never a panic: every byte of the
/// shape segment of a real part is flipped in turn (in steps).
#[test]
fn damaged_nx_parts_never_panic() {
    let data = std::fs::read(fixture("mtc-assembly/nx/90591A141 HEX NUT.prt")).unwrap();
    let streams = teamcraft::nxpart::streams(&data);
    let jt = streams
        .iter()
        .find(|s| s.name == teamcraft::nxpart::JT_STREAM)
        .unwrap();
    let start = jt.offset as usize;
    let end = start + jt.len as usize;
    for at in (start..end).step_by(97) {
        let mut copy = data.clone();
        copy[at] ^= 0x5A;
        let _ = mesh::read("nx-part", &copy);
    }
    for cut in (start..end).step_by(997) {
        let _ = mesh::read("nx-part", &data[..cut]);
    }
}

/// The stand-alone JT 10.5 export of the NIST assembly: all 36 instances of
/// its 8 parts at their places (checked against NX to 6e-6 mm), every
/// part closed and consistently oriented, in millimetres. The box measures
/// 152.4 × 101.6 mm like NIST's STEP file; the screws stand out on top.
#[test]
fn jt_assemblies_get_a_placed_3d_preview() {
    let rel = "mtc-assembly/nx/NIST mtc crada assembly.jt";
    assert_eq!(
        preview::PreviewFormat::of(rel),
        Some(preview::PreviewFormat::Jt)
    );
    let mesh = preview::convert(preview::PreviewFormat::Jt, &fixture(rel)).unwrap();
    mesh.check().unwrap();
    assert_eq!(mesh.parts, 36);
    // LOD 0 of the 36 instances; too many triangles for a refinement
    assert_eq!(mesh.triangle_count(), 269_304);
    let b = mesh.bounds();
    let expect = [-4.0, -4.0, -8.6, 148.4, 97.6, 99.05];
    for (got, want) in b.iter().zip(expect) {
        assert!((got - want).abs() < 0.001, "{b:?}");
    }
    let mut edges = std::collections::HashMap::<(u32, u32), u32>::new();
    for t in mesh.indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    assert!(
        edges
            .iter()
            .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1)),
        "every part is closed and consistently oriented"
    );
    let s = teamcraft::shape::compute(&mesh).unwrap();
    assert!(s.is_valid() && s.closed && s.volume > 0.0, "{s:?}");
    let bytes = mesh.encode();
    assert_eq!(mesh::Mesh::decode(&bytes).unwrap(), mesh);
    assert!(thumbnail::rendered("jt") && thumbnail::supports("jt"));
}

/// A damaged JT file is an error, never a panic: bytes all over the file
/// (scene graph, shape segments, TOC) are flipped in turn, and the file is
/// cut short.
#[test]
fn damaged_jt_files_never_panic() {
    let data = std::fs::read(fixture("mtc-assembly/nx/NIST mtc crada assembly.jt")).unwrap();
    let step = data.len() / 40;
    for at in (0..data.len()).step_by(step) {
        let mut copy = data.clone();
        copy[at] ^= 0x5A;
        let _ = mesh::read("jt", &copy);
    }
    for cut in (0..data.len()).step_by(data.len() / 12) {
        let _ = mesh::read("jt", &data[..cut]);
    }
}

/// Every STEP fixture – AP203 geometry only and AP242 with PMI – gets a
/// complete 3D preview. The AP242 file used to fail: its tessellated PMI
/// (empty lists `()`) made the B-rep reader's parser reject the whole file.
#[test]
fn step_files_get_a_complete_3d_preview() {
    for (rel, min_triangles) in [
        ("ftc-11/step/nist_ftc_11_asme1_rb.stp", 500),
        ("ctc-01/step/nist_ctc_01_asme1_rd.stp", 500),
        ("ctc-01/step/nist_ctc_01_asme1_ap242-e1.stp", 500),
    ] {
        let mesh = preview::convert(preview::PreviewFormat::Step, &fixture(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert!(
            mesh.triangle_count() >= min_triangles,
            "{rel}: {} triangles",
            mesh.triangle_count()
        );
        assert_eq!(mesh.faces, None, "{rel}: every face is shown");
        mesh.check().unwrap();
        let bytes = mesh.encode();
        assert_eq!(mesh::Mesh::decode(&bytes).unwrap(), mesh);
        // the shape signature of the similar part search, from the same mesh
        let s = teamcraft::shape::compute(&mesh).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert!(s.is_valid(), "{rel}: {s:?}");
        assert!(s.extents[0] >= s.extents[1] && s.extents[1] >= s.extents[2]);
        assert!(s.area > 0.0 && s.triangles > 0, "{rel}: {s:?}");
        assert_eq!(s.triangles as usize, mesh.triangle_count(), "{rel}");
        let same = teamcraft::shape::compare(&s, &s, false);
        assert!(same.score > 0.999, "{rel}: {same:?}");
    }
}

/// The AP242 file carries its PMI (dimensions, notes) as tessellated
/// triangles; they are not the part. Without the B-rep there is nothing
/// to show, rather than a preview of floating text.
#[test]
fn step_pmi_is_not_shown_as_geometry() {
    let rel = "ctc-01/step/nist_ctc_01_asme1_ap242-e1.stp";
    let text = std::fs::read_to_string(fixture(rel)).unwrap();
    assert!(text.contains("COMPLEX_TRIANGULATED_SURFACE_SET"));
    let without_brep = text.replace("CLOSED_SHELL(", "UNUSED_SHELL(");
    let err = mesh::read("step", without_brep.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("no solids"), "{err}");
}

/// A face the kernel cannot handle costs that face: the preview is marked
/// partial and still shows the rest of the part.
#[test]
fn a_bad_face_in_a_real_step_file_gives_a_partial_preview() {
    let rel = "ctc-01/step/nist_ctc_01_asme1_ap242-e1.stp";
    let text = std::fs::read_to_string(fixture(rel)).unwrap();
    let broken = text.replacen("=CYLINDRICAL_SURFACE(", "=UNKNOWN_SURFACE(", 1);
    let mesh = mesh::read("step", broken.as_bytes()).unwrap();
    let (shown, total) = mesh.faces.expect("partial");
    assert!(shown < total && shown + 3 >= total, "{shown} of {total}");
    assert!(mesh.triangle_count() > 500);
}

/// AP242 tessellated geometry (no B-rep at all): the B-rep preview of a
/// real part written as `TRIANGULATED_FACE`s and triangle fans is read back
/// triangle for triangle.
#[test]
fn step_tessellated_geometry_round_trip() {
    let rel = "ctc-01/step/nist_ctc_01_asme1_rd.stp";
    let brep = preview::convert(preview::PreviewFormat::Step, &fixture(rel)).unwrap();
    let mut text =
        String::from("ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nENDSEC;\nDATA;\n");
    let points: Vec<String> = brep
        .positions
        .chunks(3)
        .map(|p| format!("({:?},{:?},{:?})", p[0] as f64, p[1] as f64, p[2] as f64))
        .collect();
    text += &format!(
        "#1=COORDINATES_LIST('',{},({}));\n",
        points.len(),
        points.join(",")
    );
    let mut faces = Vec::new();
    for (k, chunk) in brep.indices.chunks(3 * 50).enumerate() {
        let id = 10 + k;
        let tris: Vec<String> = chunk
            .chunks(3)
            .map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1))
            .collect();
        if k % 2 == 0 {
            text += &format!(
                "#{id}=TRIANGULATED_FACE('',#1,0,(),$,(),({}));\n",
                tris.join(",")
            );
        } else {
            // the same triangles as one-triangle fans
            text += &format!(
                "#{id}=COMPLEX_TRIANGULATED_FACE('',#1,0,(),$,(),(),({}));\n",
                tris.join(",")
            );
        }
        faces.push(format!("#{id}"));
    }
    text += &format!("#2=TESSELLATED_SOLID('',({}),$);\n", faces.join(","));
    text += "ENDSEC;\nEND-ISO-10303-21;\n";
    let mesh = mesh::read("step", text.as_bytes()).unwrap();
    assert_eq!(mesh.triangle_count(), brep.triangle_count());
    assert_eq!(mesh.bounds(), brep.bounds());
    assert_eq!(mesh.faces, None);
}

/// The name in the STEP header (`FILE_NAME`) is what the exporting program
/// wrote, not a reference to another file.
#[tokio::test]
async fn step_header_name_is_not_a_reference() {
    for rel in [
        "ctc-01/step/nist_ctc_01_asme1_rd.stp",
        "ctc-01/step/nist_ctc_01_asme1_ap242-e1.stp",
    ] {
        let name = Path::new(rel)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let refs = cadrefs::scan_file(&fixture(rel), &name, "step").await;
        assert!(refs.is_empty(), "{rel}: {refs:?}");
    }
}

/// SolidWorks 2015 and newer keep the preview image and the names of the
/// referenced files in Deflate-compressed sections of their own container
/// (`swpart`).
#[tokio::test]
async fn solidworks_files_carry_previews_and_references() {
    let dir = fixture("mtc-assembly/solidworks");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| {
                e.eq_ignore_ascii_case("sldprt") || e.eq_ignore_ascii_case("sldasm")
            })
        })
        .collect();
    files.push(fixture(
        "ctc-01/solidworks-2018/nist_ctc_01_asme1_rd_sw1802.SLDPRT",
    ));
    files.push(fixture(
        "ftc-11/solidworks-2018/nist_ftc_11_asme1_rb_sw1802.SLDPRT",
    ));
    assert_eq!(files.len(), 11);
    for path in &files {
        let image = thumbnail::extract(path)
            .unwrap()
            .unwrap_or_else(|| panic!("{}: no preview image", path.display()));
        assert_eq!(image.content_type, "image/png", "{}", path.display());
        assert!(image.bytes.len() > 10_000, "{}", path.display());
    }

    // The assembly names exactly its eight parts – not the old names of
    // renamed components that the rest of the file still contains.
    let name = "nist_mtc_crada_assembly_rev-D.SLDASM";
    let mut scanned = cadrefs::scan_file(&dir.join(name), name, "solidworks-assembly").await;
    scanned.sort_by(|a, b| a.name.cmp(&b.name));
    let refs: Vec<String> = scanned.iter().map(|r| r.name.clone()).collect();
    assert_eq!(refs.len(), 8, "{refs:?}");
    assert!(refs.iter().all(|r| dir.join(r).is_file()), "{refs:?}");
    // Scanning the bytes in memory gives the same result.
    let bytes = std::fs::read(dir.join(name)).unwrap();
    let mut in_memory = cadrefs::find(&bytes, name);
    in_memory.sort();
    assert_eq!(in_memory, refs);
    // How often the assembly places each part (#247).
    let counts: Vec<Option<u32>> = scanned.iter().map(|r| r.instances).collect();
    assert_eq!(counts, [10, 14, 4, 4, 1, 1, 1, 1].map(Some), "{scanned:?}");
    // Parts reference nothing (no noise from their compressed data).
    for path in files.iter().filter(|p| p != &&dir.join(name)) {
        let own = path.file_name().unwrap().to_string_lossy().into_owned();
        let refs = cadrefs::scan_file(path, &own, "solidworks-part").await;
        assert!(refs.is_empty(), "{own}: {refs:?}");
    }
}

/// NX parts are read through their stream container (`nxpart`): only the
/// reference streams count, not the save history ("Part renamed from …").
#[tokio::test]
async fn nx_parts_report_no_false_references() {
    let dir = fixture("mtc-assembly/nx");
    for part in [
        "NIST mtc crada box.prt",
        "91274A118 SHCS.prt",
        "90591A141 HEX NUT.prt",
    ] {
        let refs = cadrefs::scan_file(&dir.join(part), part, "nx-part").await;
        assert!(refs.is_empty(), "{part}: {refs:?}");
    }
    let rel = "ctc-01/nx-1980/nist_ctc_01_asme1_nx1980_rd.prt";
    let refs =
        cadrefs::scan_file(&fixture(rel), "nist_ctc_01_asme1_nx1980_rd.prt", "nx-part").await;
    assert!(refs.is_empty(), "{rel}: {refs:?}");
    // The assembly names exactly its eight components.
    let name = "NIST mtc crada assembly.prt";
    let mut refs: Vec<String> = cadrefs::scan_file(&dir.join(name), name, "nx-part")
        .await
        .into_iter()
        .map(|r| r.name)
        .collect();
    refs.sort();
    assert_eq!(refs.len(), 8, "{refs:?}");
    assert!(refs.iter().all(|r| dir.join(r).is_file()), "{refs:?}");
    // Scanning the bytes in memory gives the same result.
    let bytes = std::fs::read(dir.join(name)).unwrap();
    let mut in_memory = cadrefs::find(&bytes, name);
    in_memory.sort();
    assert_eq!(in_memory, refs);
}

/// The NIST STEP files carry the name they were exported under in their
/// header – a second name of the file when it differs from the stored one,
/// never a reference.
#[tokio::test]
async fn step_files_carry_their_export_name() {
    for (rel, export) in [
        (
            "ctc-01/step/nist_ctc_01_asme1_rd.stp",
            Some("nist_ctc_01_asme1.stp"),
        ),
        (
            "ftc-11/step/nist_ftc_11_asme1_rb.stp",
            Some("nist_ftc_11_asme1.stp"),
        ),
        // exported under the name it is stored with: no second name
        ("ctc-01/step/nist_ctc_01_asme1_ap242-e1.stp", None),
    ] {
        let path = fixture(rel);
        let own = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            cadrefs::scan_export_name(&path, &own).await.as_deref(),
            export,
            "{rel}"
        );
        let refs = cadrefs::scan_file(&path, &own, "step").await;
        assert!(refs.is_empty(), "{rel}: {refs:?}");
    }
}

/// A monolithic JT file (the whole assembly in one file) references no
/// other file: the binary noise of its compressed segments (`…\x9f4.JT`)
/// is not read as a file name. A name the file really holds is still found.
#[tokio::test]
async fn jt_files_report_no_false_references() {
    let rel = "mtc-assembly/nx/NIST mtc crada assembly.jt";
    let refs = cadrefs::scan_file(&fixture(rel), "NIST mtc crada assembly.jt", "jt").await;
    assert!(refs.is_empty(), "{rel}: {refs:?}");
    // the same file with a part reference of a shattered JT assembly
    // (UTF-16, as JT stores strings) between its segments
    let mut bytes = std::fs::read(fixture(rel)).unwrap();
    let at = bytes.len() / 2;
    let name: Vec<u8> = "NIST mtc crada box.jt"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    bytes.splice(at..at, [vec![0, 0], name, vec![0, 0]].concat());
    assert_eq!(
        cadrefs::find(&bytes, "NIST mtc crada assembly.jt"),
        ["NIST mtc crada box.jt"]
    );
}

/// The Rhino models of rhino3dm (MIT): a mesh object, a polysurface with
/// its stored render mesh and a SubD (no stored mesh: its control net is
/// subdivided) – each a closed, consistently oriented preview in
/// millimetres with the extent seen in Rhino.
#[test]
fn rhino_models_get_a_closed_3d_preview() {
    for (name, triangles, extent) in [
        // a sphere-like mesh, radius 25.1 mm
        ("mesh.3dm", 276, [50.25, 50.25, 50.25]),
        // render meshes of one polysurface; welded at 1 µm
        ("sphereDecals.3dm", 18_752, [107.83, 107.83, 107.84]),
        // 6 control faces, 4 subdivision steps: 6 · 4⁴ quads
        ("subd_creases.3dm", 3_072, [9.583, 9.583, 9.198]),
    ] {
        let path = root().join("rhino3dm").join(name);
        assert_eq!(
            preview::PreviewFormat::of(name),
            Some(preview::PreviewFormat::Rhino)
        );
        let mesh = preview::convert(preview::PreviewFormat::Rhino, &path)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        mesh.check().unwrap();
        assert_eq!(mesh.triangle_count(), triangles, "{name}");
        let b = mesh.bounds();
        for k in 0..3 {
            let size = b[k + 3] - b[k];
            assert!(
                (size - extent[k]).abs() < 0.01,
                "{name}: axis {k} is {size} mm, not {}",
                extent[k]
            );
        }
        let mut edges = std::collections::HashMap::<(u32, u32), u32>::new();
        for t in mesh.indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a, b)).or_default() += 1;
            }
        }
        assert!(
            edges
                .iter()
                .all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1)),
            "{name}: the mesh is not closed"
        );
        let s = teamcraft::shape::compute(&mesh).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(s.is_valid() && s.closed && s.volume > 0.0, "{name}: {s:?}");
        assert_eq!(mesh::Mesh::decode(&mesh.encode()).unwrap(), mesh);
    }
}

/// Damaged or cut Rhino models are errors, never panics.
#[test]
fn damaged_rhino_models_never_panic() {
    for name in ["mesh.3dm", "subd_creases.3dm"] {
        let data = std::fs::read(root().join("rhino3dm").join(name)).unwrap();
        for at in (32..data.len()).step_by(61) {
            let mut copy = data.clone();
            copy[at] ^= 0x5A;
            let _ = mesh::read("rhino", &copy);
        }
        for cut in (0..data.len()).step_by(211) {
            let _ = mesh::read("rhino", &data[..cut]);
        }
    }
}
