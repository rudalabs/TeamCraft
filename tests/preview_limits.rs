//! Previews never read what is too large: size caps before opening and a
//! memory limit for the conversion process (no database needed).

use std::process::Command;

use teamcraft::{
    error::AppError,
    preview::{PreviewFormat, PreviewLimits, Previews},
    vault::Vault,
};

const MIB: u64 = 1024 * 1024;

fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("teamcraft-limits-{}", uuid::Uuid::new_v4()))
}

/// A (sparse) file of `size` bytes at the place of the blob `sha256`.
fn sparse_blob(vault: &Vault, sha256: &str, size: u64) {
    let path = vault.object_path(sha256).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(&path).unwrap().set_len(size).unwrap();
}

fn limits(result_mb: u64) -> PreviewLimits {
    PreviewLimits {
        max_result_bytes: result_mb * MIB,
        ..PreviewLimits::default()
    }
}

#[tokio::test]
async fn a_file_above_the_cap_is_never_opened_and_the_refusal_is_cached() {
    let root = temp_dir();
    let vault = Vault::open(&root).await.unwrap();
    // 64 GiB: reading it would exhaust the memory of any machine
    let sha = "ab".repeat(32);
    sparse_blob(&vault, &sha, 64 * 1024 * MIB);
    let previews = Previews::new(&vault, PreviewLimits::default());
    for format in [PreviewFormat::Vtu, PreviewFormat::Frd, PreviewFormat::Stl] {
        let first = previews.mesh(&vault, &sha, format).await;
        let Err(AppError::BadRequest(message)) = first else {
            panic!("{format:?}: {first:?}");
        };
        assert!(message.contains("too large for a 3D preview"), "{message}");
        assert!(message.contains("rendered picture"), "{message}");
    }
    // cached: the refusal counts as a finished conversion
    assert!(previews.is_converted(&sha).await);
    assert!(!previews.has_mesh(&sha).await);
    // the signature path ends in the same refusal
    assert!(previews
        .signature(&vault, &sha, PreviewFormat::Vtu)
        .await
        .is_err());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn caps_are_per_kind_and_a_raised_cap_lifts_a_cached_refusal() {
    let sha = "cd".repeat(32);
    let root = temp_dir();
    let vault = Vault::open(&root).await.unwrap();
    sparse_blob(&vault, &sha, 3 * MIB);
    // results: 1 MB cap, refused; meshes keep their larger cap (and the
    // zero-filled file is refused as a broken mesh, not as too large)
    let strict = Previews::new(&vault, limits(1));
    let Err(AppError::BadRequest(m)) = strict.mesh(&vault, &sha, PreviewFormat::Vtu).await else {
        panic!("not refused");
    };
    assert!(m.contains("too large"), "{m}");
    // the administrator raises the cap: the old refusal does not stick
    let relaxed = Previews::new(&vault, limits(64));
    let Err(AppError::BadRequest(m)) = relaxed.mesh(&vault, &sha, PreviewFormat::Vtu).await else {
        panic!("a zero-filled file is no VTU");
    };
    assert!(!m.contains("too large"), "{m}");
    std::fs::remove_dir_all(&root).ok();

    let root = temp_dir();
    let vault = Vault::open(&root).await.unwrap();
    sparse_blob(&vault, &sha, 3 * MIB);
    let strict = Previews::new(&vault, limits(1));
    let Err(AppError::BadRequest(m)) = strict.mesh(&vault, &sha, PreviewFormat::Stl).await else {
        panic!("not refused");
    };
    assert!(!m.contains("too large"), "3 MB is within the mesh cap: {m}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_conversion_process_refuses_a_large_input_before_opening_it() {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("big.vtu");
    std::fs::File::create(&input)
        .unwrap()
        .set_len(16 * 1024 * MIB)
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_teamcraft"))
        .args(["preview-mesh", "--format", "vtk-unstructured", "--input"])
        .arg(&input)
        .arg("--output")
        .arg(dir.join("out.tcm"))
        .args(["--max-input-bytes", &(64 * MIB).to_string()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("too large for a 3D preview"), "{stderr}");
    assert!(!dir.join("out.tcm").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(target_os = "linux")]
#[test]
fn the_conversion_process_stops_at_its_memory_limit() {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("part.stl");
    // a binary STL of 100 000 triangles (5 MB): more than the process holds
    let n = 100_000u32;
    let mut stl = vec![0u8; 80];
    stl.extend_from_slice(&n.to_le_bytes());
    for i in 0..n {
        stl.extend_from_slice(&[0u8; 12]);
        for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in v {
                stl.extend_from_slice(&(c + i as f32).to_le_bytes());
            }
        }
        stl.extend_from_slice(&[0u8; 2]);
    }
    std::fs::write(&input, stl).unwrap();
    let run = |memory: &str| {
        Command::new(env!("CARGO_BIN_EXE_teamcraft"))
            .args(["preview-mesh", "--format", "stl", "--input"])
            .arg(&input)
            .arg("--output")
            .arg(dir.join("out.tcm"))
            .args(["--max-memory-bytes", memory])
            .output()
            .unwrap()
    };
    assert!(run("0").status.success(), "no limit");
    assert!(
        run(&(4096 * MIB).to_string()).status.success(),
        "room enough"
    );
    // far below what the process already maps: nothing more can be allocated
    let limited = run(&(1024 * 1024).to_string());
    assert!(!limited.status.success(), "stopped at the limit");
    std::fs::remove_dir_all(&dir).ok();
}

/// The PDF text process (the PDF on stdin, the text on stdout): size cap,
/// memory limit, bad input.
#[test]
fn the_pdf_text_process_is_bounded() {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let run = |input: &std::path::Path, max_input: u64, memory: u64| {
        Command::new(env!("CARGO_BIN_EXE_teamcraft"))
            .arg("text-extract")
            .args(["--max-input-bytes", &max_input.to_string()])
            .args(["--max-memory-bytes", &memory.to_string()])
            .stdin(std::fs::File::open(input).unwrap())
            .output()
            .unwrap()
    };
    // a huge (sparse) file is refused after at most the cap has been read
    let big = dir.join("big.pdf");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(16 * 1024 * MIB)
        .unwrap();
    let out = run(&big, 8 * MIB, 0);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("too large to read"));
    assert!(out.stdout.is_empty());
    // not a PDF: a clean error, no crash
    let junk = dir.join("junk.pdf");
    std::fs::write(&junk, b"this is not a pdf").unwrap();
    let out = run(&junk, 32 * MIB, 0);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a readable PDF"));
    // a memory limit far below what the process already maps stops it
    #[cfg(target_os = "linux")]
    {
        let small = dir.join("small.pdf");
        std::fs::write(&small, vec![b'%'; 2 * MIB as usize]).unwrap();
        let out = run(&small, 32 * MIB, 1024 * 1024);
        assert!(!out.status.success(), "stopped at the limit");
    }
    std::fs::remove_dir_all(&dir).ok();
}
