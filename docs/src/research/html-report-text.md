# Text from HTML reports (for the assistant, the search and templates)

**Question:** many FEA/CFD tools write their reports as HTML (a single file,
often with embedded CSS and base64 images, or an HTML file with an assets
folder). The result values sit in tables. How does TeamCraft get readable text
with the table rows intact, in pure Rust, without a browser engine and without
letting a hostile file hurt the server? Investigated on 2026-11-07 for
[#141](https://github.com/rudalabs/TeamCraft/issues/141).

## What was wrong before

`.html` was indexed as plain text: the raw markup (tags, inline CSS, scripts)
went into the search text, and the plain-text rule dropped every decimal
number, so `read_file_text` returned markup without the values and
`propose_result_values` had nothing to quote. The extraction templates ran a
crude tag stripper over the raw bytes in the server process.

## Candidates

| Option                                                                                                                   | Licence                                   | Notes                                                                                                                                                                                                          |
|--------------------------------------------------------------------------------------------------------------------------|-------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| [`html5ever`](https://crates.io/crates/html5ever) + `markup5ever_rcdom` or [`scraper`](https://crates.io/crates/scraper) | MIT/Apache-2.0 (html5ever), ISC (scraper) | the full HTML5 tokenizer and tree builder of Servo: exact, but builds a whole DOM in memory and pulls in a larger dependency tree; tables, hidden elements and the line layout still have to be written on top |
| [`tl`](https://crates.io/crates/tl)                                                                                      | MIT                                       | fast zero-copy parser, not spec-complete; a DOM again                                                                                                                                                          |
| [`lol_html`](https://crates.io/crates/lol_html)                                                                          | BSD-3-Clause                              | streaming rewriter (Cloudflare); built to rewrite, not to lay out text                                                                                                                                         |
| [`html2text`](https://crates.io/crates/html2text)                                                                        | MIT                                       | renders HTML to text with table drawing on top of html5ever; table output is box art, wider than needed and hard to quote                                                                                      |
| own bounded tokenizer                                                                                                    | MPL-2.0 (ours)                            | a few hundred lines; only text, block breaks, table rows and the skip rules matter                                                                                                                             |

## Decision

**An own tokenizer (`src/htmltext.rs`) plus `encoding_rs`**, which was already
a direct dependency – no new crate, nothing for `deny.toml` to judge. A full
HTML5 tree builder is not needed: TeamCraft needs the text, line breaks at
block elements and table rows as lines, and every input byte is hostile. An
own reader is small enough to read in a review, carries the no-panic lints of
the other untrusted-file readers, and is fuzzed (`htmltext` target).

Rules:

* a table row is one line, cells separated by ` | ` (empty cells keep their
  column, nested tables give their own rows, block elements inside a cell
  become spaces);
* entities: the named references reports use (units, symbols, Latin and Greek
  letters) and all numeric ones; invalid code points become U+FFFD;
* skipped: `script`, `style`, `iframe`, `noscript`, `template`, `svg`,
  `canvas`, `math`, `object`, comments, and hidden elements (`hidden`,
  `aria-hidden="true"`, inline `display:none` / `visibility:hidden`); text that
  a style sheet hides or colours invisible is *not* detected (no CSS engine) –
  the untrusted marker of `read_file_text` and the human review of drafts stay
  the safeguard;
* attribute values are passed over without being copied, except `alt` (image
  descriptions are text) and the few that decide about hiding, each at most
  1 024 characters – a multi-megabyte `data:` URI costs a scan, no allocation;
* charset: byte order mark, `<meta charset>` / `http-equiv` in the first
  1 024 bytes (a declared UTF-16 means UTF-8, as in the HTML standard), valid
  UTF-8, else Windows-1252; an unknown label is ignored;
* nothing is fetched or followed (`src`, `href`, `@import`, `<base>`,
  `<meta http-equiv=refresh>`); MHTML and ZIP-packed reports are out of scope.

Isolation as for PDF: the `teamcraft text-extract --format html` child reads
the file from stdin (the first 16 MB – more is cut, not refused, so a big
report keeps its first tables), with the time limit, the memory limit of the
previews, a cleared environment, `kill_on_drop` and the two shared slots. At
most 256 open elements are tracked (deeper ones are flattened) and the output
stops at 200 000 characters.

**Templates:** the per-group extraction templates of an `.html` file now run
on the text the child extracted (at most 200 000 characters), not on the raw
bytes –
`Maximum stress \| ([0-9.]+)` works, and a `csv` template reads the ` | `
separated rows (`|` is always the delimiter of an HTML file, quotes are plain
characters, an empty cell keeps its column). The line of a value's source (`report.html:12`) is the line in the
extracted text, not in the HTML file.

**Plain text**, fixed at the same time: text and CSV reports keep their lines
and decimal numbers in the stored text (`read_file_text` used to show the
search text without decimals). Only geometry stored as text – ASCII STL, OBJ,
KiCad boards and symbols – still drops decimal numbers, as coordinates are
noise in a search. Files uploaded before keep their old text until a new
version is uploaded.

## Sources

* WHATWG HTML Living Standard: tokenization, named character references,
  "determining the character encoding" (prescan of 1 024 bytes).
* Encoding Standard (labels, Windows-1252 for ISO-8859-1) as implemented by
  `encoding_rs`.
* The crates' pages on crates.io for their licences (checked 2026-11-07).
