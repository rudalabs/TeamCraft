# 3D preview of CATIA V5 products

*2026-11-12*

## Question

Can TeamCraft put together the 3D preview of a CATIA V5 product
(`.CATProduct`) from the previews of its parts, as it does for NX,
SOLIDWORKS and Inventor assemblies? What does a product store about its
components, and how are the component files found?

## Findings

* A product is a `V5_CFV2` document like a part (see
  [3D preview of CATIA V5 parts](catia-cgr-preview.md)). It has no
  visualisation data of its own; its component instances and their
  placements are in the object stream of its **`CATProdCont`** container.
* **No file names are stored.** A full scan for Latin-1 and UTF-16 strings
  finds no `.CATPart` and no component file name. Each instance points to
  the **UUID of the referenced document's `CATProdCont` container**
  (plus the object id 2, that document's root product).
  * Every CATIA document lists its containers in a table near the start of
    the file (the first 64 KiB are enough):
    `02 00 81 20 <u32 n> <name>\0 <type>\0 [up to 8 × 00] 03 00 f7 00 03 00 00 00 <uuid: 4 × u32 big-endian>`.
    The entry *named* `CATProdCont` is the one products link to; parts also
    have a `CATPrtCont` whose *type* is `CATProdCont`.
  * A copied file keeps its UUID; a renamed one too.
  * The *part number* is stored as well. In the sample it equals the file
    name without extension for 67 of 68 instances – not for the one whose
    part number is a description ("5 gallon bucket" → `Bucket.CATPart`).
* **The object stream** begins with `OSMX` and a version string and is
  divided into sections `7c <kind> <u32 length>` (the length counts from the
  `7c`): a header (`0c`), the **objects** (`04`), a **side table** with one
  block per object (`08`), an **extension table** (`0b`) and the
  **string table** (`02`). Between sections there can be up to 2 stray
  bytes.
  * Small numbers are one byte `81..cf` (value − 0x81), two bytes
    `d1..e3 xx` or `80 <u32>`; a string reference is one byte `34..ff`
    (index − 0x34) or `32 <u32 index + 1>`; doubles are `e6 <f64>`, with
    `e7` / `e8` / `e9` for 1, 0 and −1.
  * Every object record (`7c 05`) has a header (`7c 06`: two flag bytes, the
    class, the name, `ea <u32 id>`) and an attribute block (`7c 07`). The
    classes that matter are `ASMPRODUCT` (a product or an instance; its name
    is the part number) and `PRDBAGREP` (holds the placement).
  * Attributes: `<name> <key> e8|e9 <flags> <type> [extra byte after e9] 37
    <value> fe`. Type `0x12` is a string (`_InstanceName`), type `0x07` an
    array of doubles (`_Position`: 12 values).
  * The side table block of an object holds its link into another document
    (`e5 <u32 length> 11 03 <uuid> … <u32 object id> …`) and its
    list-valued attributes: `_Component` of the root lists the instances in
    the order of the specification tree, `_BagRepsList` of an instance names
    the `PRDBAGREP` with its placement.
* **Placement:** the 12 doubles are the instance's X, Y and Z axes and its
  origin, in millimetres, in the coordinates of the product. All 68 sample
  placements are rotations (det = +1): mirrored components are separate
  documents with mirrored geometry. An instance without `_Position` sits at
  the origin (the first, fixed part).
* No public description of the format was found; Dassault's readers are
  proprietary. Everything was read off one sample product (V5R21, 68
  instances of 22 documents, a private hobby project that is not
  committed). The stream primitives were also parsed without error in the
  product container of all 25 CATParts at hand and of the NIST CATPart
  (V5-6R2019).

## Verification

* All 68 instances of the sample are found with their instance name, part
  number, reference and placement; the root's `_Component` list names the
  same 68 objects in the same order.
* All 68 references resolve by UUID to a CATPart of the project folder.
* Composing the parts with the placements gives the drawing of the
  assembly: the mirrored halves meet, the pipes run from connector to
  connector (the convention "stored axes are the columns of the rotation"
  is the only one for which the pipe axes and the 36 mirror pairs agree).

## Decision

* **Implement** the reader in pure Rust (`src/catprod.rs`): container
  table, object stream, the two attributes and the side table, every count
  and offset checked against the bytes left.
* **Resolution of the components** (`services/assemblies.rs`, the CATIA
  family):
  1. by UUID among the CATIA files of the product's own revision and the
     files found by name (step 2) – the first 64 KiB of each are read;
  2. otherwise by name: `<part number>.CATPart` or `.CATProduct`, the way
     NX, SOLIDWORKS and Inventor components are found (own revision first,
     then the latest revision of another visible item).
  A component found neither way is left out, as for the other systems.
* Sub-products are entered like sub-assemblies, placements composed.
* The thumbnail of a product is rendered from its composed preview.
* **Not yet:** hidden components (no sample with one; `VPGlobal` in the
  extension table is the only candidate), components without a document of
  their own, and CATIA V5-6R20xx products (untested). Products with file
  links in a `CATDLName`-style document table were not seen.

## Sources

* Sample files: one private product with its parts (local only) and the
  NIST CATPart in `tests/fixtures/cad`.
* Prior work in TeamCraft: [3D preview of CATIA V5 parts](catia-cgr-preview.md),
  `services/assemblies.rs` (NX, SOLIDWORKS and Inventor assemblies).
