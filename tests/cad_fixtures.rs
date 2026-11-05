//! Real CAD files from NIST (`tests/fixtures/cad`, see its README): the
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
    assert_eq!(entries.len(), 32, "every fixture is listed");
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
    let refs = cadrefs::scan_file(&dir.join(name), name).await;
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
        assert!(refs.iter().any(|r| r == part), "{part} missing in {refs:?}");
        assert!(dir.join(part).is_file(), "{part} is part of the fixture");
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
        let refs = cadrefs::scan_file(&fixture(rel), &name).await;
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
    let mut refs = cadrefs::scan_file(&dir.join(name), name).await;
    refs.sort();
    assert_eq!(refs.len(), 8, "{refs:?}");
    assert!(refs.iter().all(|r| dir.join(r).is_file()), "{refs:?}");
    // Scanning the bytes in memory gives the same result.
    let bytes = std::fs::read(dir.join(name)).unwrap();
    let mut in_memory = cadrefs::find(&bytes, name);
    in_memory.sort();
    assert_eq!(in_memory, refs);
    // Parts reference nothing (no noise from their compressed data).
    for path in files.iter().filter(|p| p != &&dir.join(name)) {
        let own = path.file_name().unwrap().to_string_lossy().into_owned();
        let refs = cadrefs::scan_file(path, &own).await;
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
        let refs = cadrefs::scan_file(&dir.join(part), part).await;
        assert!(refs.is_empty(), "{part}: {refs:?}");
    }
    let rel = "ctc-01/nx-1980/nist_ctc_01_asme1_nx1980_rd.prt";
    let refs = cadrefs::scan_file(&fixture(rel), "nist_ctc_01_asme1_nx1980_rd.prt").await;
    assert!(refs.is_empty(), "{rel}: {refs:?}");
    // The assembly names exactly its eight components.
    let name = "NIST mtc crada assembly.prt";
    let mut refs = cadrefs::scan_file(&dir.join(name), name).await;
    refs.sort();
    assert_eq!(refs.len(), 8, "{refs:?}");
    assert!(refs.iter().all(|r| dir.join(r).is_file()), "{refs:?}");
    // Scanning the bytes in memory gives the same result.
    let bytes = std::fs::read(dir.join(name)).unwrap();
    let mut in_memory = cadrefs::find(&bytes, name);
    in_memory.sort();
    assert_eq!(in_memory, refs);
}

/// Gap: binary noise in JT files is read as file names.
#[tokio::test]
#[ignore = "known gap: false references of the heuristic scan in JT files"]
async fn jt_files_report_no_false_references() {
    let rel = "mtc-assembly/nx/NIST mtc crada assembly.jt";
    let refs = cadrefs::scan_file(&fixture(rel), "NIST mtc crada assembly.jt").await;
    assert!(refs.is_empty(), "{rel}: {refs:?}");
}
