# HTML report fixtures

Small, hand-written **synthetic** HTML reports in the style of the report
generators of FEA/CFD tools, for `src/htmltext.rs` (unit tests), the fuzz
corpus (`tests/parser_corpus.rs`) and the upload tests. They are original work
of the TeamCraft contributors, dedicated to the public domain (CC0-1.0); no
vendor report was copied. The numbers are made up.

| File | What it covers |
|------|----------------|
| `results-table.html` | result table with decimals, an exponent, signs, units, `&nbsp;`, `&deg;`, an image with `alt` |
| `nested-tables.html` | a layout table around the result table |
| `entities.html` | named and numeric character references, `<sup>` |
| `styled-embedded-image.html` | a large `<style>` block and an embedded base64 image (`data:` URI) before the table |
| `windows-1252.html` | Windows-1252 bytes declared with `<meta http-equiv>` |
| `utf8-bom.html` | UTF-8 with a byte order mark, no declaration |
| `scripts-comments.html` | numbers in scripts, comments, a hidden row and hidden text that must not appear |
