//! One entry point per reader of untrusted files, shared by the fuzz targets
//! in `fuzz/` (`cargo fuzz`, nightly) and the corpus test
//! `tests/parser_corpus.rs` (stable `cargo test`), so both feed the readers
//! exactly the same way.
//!
//! Every function hands the bytes to the reader(s) and ignores the result:
//! an error is a correct answer to a damaged file. What counts is that no
//! reader panics through to here, hangs or allocates what the file only
//! claims. Two readers catch panics of a library on purpose (the STEP
//! kernel `truck` in `mesh::step`, `gltf-json` in `mesh::gltf`); libFuzzer
//! would abort on those too, so the fuzz targets run through [`guarded`]
//! with [`quiet_panics`] instead of libFuzzer's own panic hook.
//!
//! Not part of the server's behaviour; public only for the fuzz crate.

use std::io::Cursor;
use std::panic::{self, AssertUnwindSafe};

/// What a container reader may inflate per section or stream here.
const LIMIT: u64 = 4 * 1024 * 1024;

/// The mesh formats of `mesh::read`, without `step` (see [`mesh_step`]).
pub const MESH_FORMATS: &[&str] = &[
    "stl",
    "obj",
    "ply",
    "off",
    "3mf",
    "amf",
    "gltf",
    "3ds",
    "pts",
    "las",
    "vtk-legacy",
    "vtk-unstructured",
    "calculix-frd",
];

/// File names `textindex` treats differently (quoted strings, plain text,
/// not indexed).
const TEXT_NAMES: &[&str] = &["part.stp", "notes.txt", "board.kicad_pcb", "model.bin"];

/// File names `bomtable` reads differently (delimited text, Excel, ODS).
const TABLE_NAMES: &[&str] = &["bom.csv", "bom.tsv", "bom.xlsx", "bom.xls", "bom.ods"];

/// Product structure of a STEP assembly.
pub fn stepbom(data: &[u8]) {
    let _ = crate::stepbom::read(data);
}

/// SOLIDWORKS 2015+ container: reference sections, the component tree as
/// XML, the preview section.
pub fn swpart(data: &[u8]) {
    let _ = crate::swpart::reference_data(Cursor::new(data), LIMIT);
    let _ = crate::swpart::components(data);
    if let Ok(Some(mut sw)) = crate::swpart::SwContainer::open(Cursor::new(data)) {
        let _ = sw.read(crate::swpart::PREVIEW_SECTION, LIMIT);
    }
}

/// NX part stream container: directory, every stream, the reference
/// streams and the JT tessellation of the 3D preview.
pub fn nxpart(data: &[u8]) {
    for stream in crate::nxpart::streams(data) {
        let _ = crate::nxpart::data(data, &stream);
    }
    let _ = crate::nxpart::reference_streams_in(data);
    let _ = crate::nxpart::reference_streams(Cursor::new(data), LIMIT);
    let _ = crate::mesh::read("nx-part", data);
    let _ = crate::nxasm::components(data);
    let _ = crate::nxasm::visible_layers(data);
}

/// Referenced file names (every container and the text heuristic).
pub fn cadrefs(data: &[u8]) {
    let _ = crate::cadrefs::find(data, "assembly.iam");
}

/// BOM tables exported by CAD programs, read as every supported type.
pub fn bomtable(data: &[u8]) {
    for name in TABLE_NAMES {
        let _ = crate::bomtable::read(name, data);
    }
}

/// Preview images stored in CAD files (OLE, NX, SOLIDWORKS, embedded PNG/DIB).
pub fn thumbnail(data: &[u8]) {
    let _ = crate::thumbnail::extract_from(Cursor::new(data));
}

/// Text extraction for the full-text search.
pub fn textindex(data: &[u8]) {
    for name in TEXT_NAMES {
        let _ = crate::textindex::extract(name, data);
    }
}

/// HTML report text (charset detection, tokenizer, tables) and the plain
/// text decoding of `textindex`.
pub fn htmltext(data: &[u8]) {
    let _ = crate::htmltext::extract(data);
    let _ = crate::htmltext::decode(data, false);
}

/// Every mesh and simulation result reader except STEP.
pub fn mesh(data: &[u8]) {
    for format in MESH_FORMATS {
        let _ = crate::mesh::read(format, data);
    }
}

/// The STEP tessellation through `truck` (slow, catches the kernel's
/// panics itself; the server runs it in a child process with a time limit).
pub fn mesh_step(data: &[u8]) {
    let _ = crate::mesh::read("step", data);
}

/// A target: its name (as in `fuzz/fuzz_targets/<name>.rs`) and entry point.
pub type Target = (&'static str, fn(&[u8]));

/// Every target by name: the fuzz targets and the corpus test use this list.
pub const TARGETS: &[Target] = &[
    ("stepbom", stepbom),
    ("swpart", swpart),
    ("nxpart", nxpart),
    ("cadrefs", cadrefs),
    ("bomtable", bomtable),
    ("thumbnail", thumbnail),
    ("textindex", textindex),
    ("htmltext", htmltext),
    ("mesh", mesh),
    ("mesh_step", mesh_step),
];

/// A panic hook that prints the message and nothing else: panics a reader
/// catches itself stay harmless, [`guarded`] decides about the rest.
pub fn quiet_panics() {
    panic::set_hook(Box::new(|info| eprintln!("panic: {info}")));
}

/// Runs `target` and aborts the process if a panic reaches it – the crash
/// libFuzzer records.
pub fn guarded(target: fn(&[u8]), data: &[u8]) {
    if panic::catch_unwind(AssertUnwindSafe(|| target(data))).is_err() {
        std::process::abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_takes_empty_and_odd_input() {
        for (_, target) in TARGETS {
            for data in [&b""[..], b"\0", b"ISO-10303-21;", &[0xD0, 0xCF, 0x11, 0xE0]] {
                target(data);
            }
        }
    }

    #[test]
    fn target_names_are_unique() {
        let mut names: Vec<&str> = TARGETS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TARGETS.len());
    }
}
