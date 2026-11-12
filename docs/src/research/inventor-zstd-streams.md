# Inventor 2026 and later: Zstandard streams and what else changed

*2026-11-12*

## Question

The 3D preview of Inventor parts and assemblies
([3D preview of Inventor files](inventor-display-preview.md)) fails for
files of current Inventor releases with *Inventor file: damaged compressed
data* ([#271](https://github.com/rudalabs/TeamCraft/issues/271)). The
report found Zstandard frames (`28 b5 2f fd`) where the reader expects
zlib. Is the compression the only change, and what does the reader need
to show such files?

## Test data

* The earlier work was checked against files saved with Inventor 2019 to
  2022 (the Autodesk sample set and the two NIST parts of
  `tests/fixtures/cad`). All of them use zlib.
* For this note: the test corpus of the
  [Corliss](https://github.com/Oblikovati/Corliss) project, **229 parts and
  assemblies saved by Inventor 2027** (196 `.ipt`, 33 `.iam`; its 69
  drawings were not used). They are small files written through Inventor's
  API, one per modelling feature or assembly relation.
* The corpus has an index (`models.json`) with what Inventor's API reports
  for every file. For the assemblies that is every occurrence with its
  file, its 4 × 4 placement and whether it is hidden or suppressed. This
  is the ground truth used below.
* Corliss is GPL-2.0-only. TeamCraft uses **none of its code**, and its
  files are **not copied into this repository**: they were downloaded only
  to check the reader. The unit tests build files with the same layout in
  the test code.
* **No file saved by Inventor 2023, 2024, 2025 or 2026 was available.**
  What is said about those releases below comes from other sources and is
  marked as such.

## Findings

### The compression

The streams keep their layout. Only one byte and the body differ:

| | Up to Inventor 2025 | Inventor 2027 |
|---|---|---|
| Data stream `B<id>` | 16 bytes, `04`, `01`, zlib body | 16 bytes, `04`, `02`, Zstandard body |
| Meta stream `M<id>` | …, two dates, `01`, zlib body | …, two dates, `02`, Zstandard body |

* The byte before the body names the codec: 1 = zlib, 2 = Zstandard.
* The body is **one Zstandard frame** ([RFC 8878](https://datatracker.ietf.org/doc/rfc8878/)):
  single segment, with the content size in the header, without a checksum
  and without a dictionary.
* The meta stream still starts with `RSe Meta Stream Version 8`. The
  section lengths, the class table and the section framing
  (`u32 class word | payload | u32 length | u8 0`) are unchanged.
* **Which release changed it:** the Corliss README speaks of "the zlib
  codec of Inventor 2025 and earlier", and Autodesk's notes for Inventor
  2026 say that files saved with 2026 are "significantly smaller". So the
  change is most likely Inventor 2026. This is not confirmed with a 2026
  file. The reader does not depend on it: it looks at the first bytes of
  the body.

### The typed arrays

Arrays keep their form `u32 0x30000002, u32 count, u32 capacity, u32 type`.

* In the older files the type word of plain numbers is 0, and key arrays
  have `0x10000000`.
* Inventor 2027 names the kind of number instead of 0. Seen: 4, 8 and 14
  for `u32` values, 16 for refs to sections, 6 for bytes. The triangle
  index lists and the lists of occurrence ids use 8.
* The second count is a capacity. It can be larger than the count (also in
  older files); the data holds `count` elements.

### Parts

* The face meshes of all 137 parts with a body are sections of the simple
  class `A79EACD2`, owned by a face node. None uses `D79AD3F3`, the class
  of most face meshes in the older files. The reader already handled this
  case, because a few older files do the same.
* A face has two such sections: the fine triangulation first, a coarse one
  second. The reader takes the first.
* Body groups (`B32BF6A6`), units (centimetres) and the exact shared
  boundary vertices are as before.

### Assemblies

* **Placements** (`AmRxSegment`, class `232792BC`): unchanged. The packed
  matrix, the 124-byte tail, the occurrence id and the key sit where they
  did.
* **Occurrence records** in `UFRxDoc`: the record no longer carries the
  title. Its length field is 0:
  `u32 0 | file id | occurrence id | number | instance | u32 0 | 6 × 00, 0x80 | flags`.
* **Flag `0x40`** no longer means *not visible*:
  * a hidden occurrence has the same flags as a visible one (`0x20`);
  * the one occurrence with the BOM structure *Reference* has `0x60` and is
    visible.

  This rests on one assembly for *Reference* and two for hidden
  occurrences. For the older record form the earlier finding stands (it
  agreed with Inventor's DWFx exports of 249 assemblies).
* **Hidden occurrences** are stored with the design views, in
  `DesignViewSegment`:
  * class `328FC2EA`, the list of views: `u32 0, u16`, an array of refs to
    the views, then the name of the **active** view;
  * class `9DC2A241`, one view: `u32 0, u16`, its name, an array of refs to
    its camera and, when it hides something, to its hidden list;
  * class `71C22321`, the hidden list: `u32 0, u16`, an array of occurrence
    ids.

  A ref is `(section index + 1) | 1 << 31`, as in the override sets.
* **Suppressed occurrences** and the **occurrence counts** per file
  ([Instance counts of Inventor assemblies](inventor-instance-counts.md))
  are stored as before.
* `AmAppSegment` has sections whose trailer byte is not 0. The preview does
  not read that segment.

### The decoder

| Option | Licence | Verdict |
|---|---|---|
| [`ruzstd`](https://github.com/KillingSpark/zstd-rs) | MIT | **use**: a decoder in pure Rust, no C code, reads from a stream so the output can be capped. The planned Fusion preview ([3D preview of Fusion designs](fusion-ogs-preview.md)) chose it too. |
| `zstd` (bindings to the reference library) | MIT / BSD | no: C code and a C toolchain in the build, against the pure-Rust rule |
| An own decoder | – | no: Huffman and FSE decoding are a lot of code to get right on hostile input |

* `ruzstd` is built without its `hash` feature: the frames carry no
  checksum, so the extra dependency would do nothing.
* The decoder refuses frames whose window is larger than 128 MiB. The
  reader also refuses a frame whose declared content is larger than its
  limit before it decodes anything, stops reading at the limit, and treats
  a frame that yields another length than it declares as damaged.

## Results with the fixed reader

Checked on the 229 files, against the index of the corpus:

| Check | Result |
|---|---|
| Parts with a body that read | 137 of 137 |
| … closed and consistently oriented | 132 |
| … open | 5, all true surface models (mesh features, a deleted face, a loft surface) |
| Parts without a body (sketches, work features), refused with that message | 59 |
| Assemblies whose shown components match Inventor: same files, same placements to 10⁻⁶ | 33 of 33 |
| Occurrences in those assemblies / shown | 82 / 79 (two hidden, one hidden and suppressed) |
| Occurrence counts per file equal to Inventor's | 33 of 33 (the suppressed one counted as 0, as before) |
| Stored preview image found | 229 of 229 |

Sizes agree with the model parameters, for example 20 × 20 × 10 mm for a
2 × 2 cm sketch extruded 1 cm.

Before the fix, none of the 229 files gave a 3D preview.

## Decision

* Read both codecs in `src/invpart.rs`, chosen by the first bytes of the
  body: a Zstandard frame with `ruzstd`, anything else as zlib.
* Accept the `u32` type words of Inventor 2027 in arrays.
* For occurrence records without a title, take the visibility from the
  active design view and ignore flag `0x40`. For records with a title,
  keep the flag. The two forms are told apart per record, not by a version
  number, so a file that mixes the old and the new parts still reads.
* The files of Inventor 2019 to 2022 must read exactly as before: the NIST
  fixtures and the existing tests stay as they are.

## Not verified

* Files saved by Inventor 2023 to 2026. In particular it is not known
  whether Inventor 2026 already uses the new type words, the untitled
  occurrence records and the design-view visibility, or only the new
  compression.
* Whether files with titled records also store what is hidden in
  `DesignViewSegment` in this form. If they do, reading it for them too
  would show the active view exactly; today the flag is used for them.
* Flexible sub-assemblies in Inventor 2027 files: the corpus has
  positional overrides (kind 1, not applied) but no re-posed child
  (kind 2).
* A sub-assembly is shown with the view that was active when it was
  saved, not with the view its parent selects for it.
* Large real-world models of Inventor 2026 or 2027.

## Sources

* [Oblikovati/Corliss](https://github.com/Oblikovati/Corliss): README and
  the test corpus `tests/fixtures` with `models.json` (GPL-2.0-only; used
  as test data only)
* [What's new in Inventor 2026](https://help.autodesk.com/cloudhelp/2026/ENU/Inventor-WhatsNew/files/IN2026-Overview.htm)
  (file size and save performance; read through a search result, the page
  itself was not reachable)
* [RFC 8878 – Zstandard Compression](https://datatracker.ietf.org/doc/rfc8878/)
* [ruzstd](https://crates.io/crates/ruzstd) (MIT)
* "Inventor" is a trademark of Autodesk; it is named only to say which
  files are read.
