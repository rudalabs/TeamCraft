# Text from PDF reports (for the assistant and the search)

**Question:** a CFD or FEA report is usually a PDF. For an agent to propose
result values from it (`read_file_text`, `propose_result_values`), TeamCraft
needs the *text* of a PDF – in pure Rust, with a licence compatible with
MPL-2.0, and without letting an untrusted file hurt the server. Investigated
on 2026-11-05.

## Candidates

| Crate                                                      | Licence            | What it is                                                               | Notes                                                                                                                                                                                  |
|------------------------------------------------------------|--------------------|--------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| [`lopdf`](https://crates.io/crates/lopdf) 0.36             | MIT                | low-level PDF object model; `extract_text` is a helper on top            | few dependencies; the text helper is basic                                                                                                                                             |
| [`pdf-extract`](https://crates.io/crates/pdf-extract) 0.12 | MIT                | text extraction with font and CMap handling, built on `lopdf`            | long-established; panics on damaged input                                                                                                                                              |
| [`pdf_oxide`](https://crates.io/crates/pdf_oxide) 0.3      | MIT OR Apache-2.0  | parser and text extraction with recovery of damaged files, reading order | young (one maintainer), larger dependency tree (about 150 crates with default features, all MIT/Apache/Zlib/Unlicense/Unicode); optional features (OCR, rendering, signatures) are off |
| [`pdf`](https://crates.io/crates/pdf) 0.10 (pdf-rs)        | MIT                | object model, no text extraction                                         | not a text extractor on its own                                                                                                                                                        |
| `pdfium-render`, `mupdf`                                   | C libraries / AGPL | –                                                                        | excluded: no C dependencies (pure Rust rule), AGPL                                                                                                                                     |
| `oxidize-pdf`                                              | GPL                | –                                                                        | excluded: licence                                                                                                                                                                      |

## Test

Three real documents (LaTeX manuals of 0.3–0.8 MB, a 1.8 MB scanned-text NASA
report) and, for each, a copy truncated to 60 %, three copies with 40 flipped
bytes, plus 10 bytes of garbage. Release build, text of the first 50 pages,
panics caught.

| Case                | `lopdf`                                       | `pdf-extract`               | `pdf_oxide`                                   |
|---------------------|-----------------------------------------------|-----------------------------|-----------------------------------------------|
| undamaged files (4) | 3 of 4 ok (1 error: ToUnicode CMap), 15–35 ms | 4 of 4 ok, 40–230 ms        | 4 of 4 ok, 55–185 ms                          |
| truncated (4)       | 4 errors (cross-reference table)              | 4 errors                    | **4 of 4 ok** (page tree rebuilt by scanning) |
| damaged (9)         | 3 ok, 5 errors                                | **5 panics**, 1 error, 3 ok | 8 ok, 1 error, no panic                       |
| garbage             | error                                         | error                       | error                                         |

The text quality of the three agrees where all succeed (the character counts
differ by a few per cent: ligatures, hyphenation, spacing).

## Decision

**`pdf_oxide`, with default features off except `legacy-crypto`** (no ICC colour
management; the old RC4/MD5 encryption stays supported, because PDFs from
solvers and Office are sometimes encrypted with an empty user password). Reasons: it is the only candidate that returned
text from
truncated and damaged files without panicking – reports that were cut off by an
interrupted upload or a flaky export are exactly the files people have – and it
is as fast as the others. The price is a young crate and a bigger dependency
tree; that is why it does not run in the server process.

PDFs are untrusted input (a "report" can be hostile). The same rules as for
the 3D previews apply:

* **Never in the server process.** The extraction runs in the hidden
  `teamcraft text-extract` child process with a time limit and the address
  space limit of the previews (`TEAMCRAFT_PREVIEW_MAX_MEMORY_MB`); a crash,
  panic or out-of-memory kill of the child fails that one extraction, nothing
  else.
* **Size before opening:** a PDF above 32 MB is not opened (the text of
  a CFD report is small; large PDFs are scans).
* **Bounded output:** at most 100 pages and 200 000 characters, the same cap
  as every other extracted text; the result (or the reason for none) is
  stored with the file version, so a file is extracted once.
* **No OCR:** scanned PDFs have no text layer; they give an empty text with a
  note, an agent can still be given the pictures by a person.
* The text is shown to the assistant as *data written by users* (the existing
  `untrusted` marker of `read_file_text`); proposals from it are never final
  values (see the simulation guide).

## Sources

* crates.io pages and licences of the crates above (checked 2026-11-05).
* The comparison program and its output are described in the table; the
  documents were local PDFs of the author's machine, not part of the repository.
