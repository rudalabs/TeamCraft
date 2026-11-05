# Finding similar parts

Before you model a new part, check whether one like it already exists.
TeamCraft compares the **shape** of 3D models – no AI, no training data, the
same answer every time – and lists parts of similar shape and size with a
reason you can check.

## Where you see it

* **Revision page → Similar parts** (below *Files*): up to six parts with a
  score, the matching file and a one-line explanation, e.g.
  *extents 118 × 40 × 8 vs 120 × 40 × 8, shape distribution 94 % similar*.
  On a brand-new part (only revision A, in work) the section is called
  *Similar existing parts*: if one fits, reuse it instead of creating a new
  item. Create the item with its STEP or mesh file and the list appears on
  the page that opens.
* **Desktop app**: the revision overview links to the list in the browser.
* **REST API**: `GET /api/v1/items/{number}/revisions/{code}/similar`
  ([REST API](../reference/api.md#similar-parts)).
* **AI assistants**: the MCP tool `find_similar_parts`
  ([MCP server](../reference/mcp.md)); `find_duplicate_files` is its sibling
  for byte-identical copies.

The comparison needs a 3D model with a preview: STEP or a mesh (STL, OBJ,
PLY, OFF, 3MF, AMF, glTF/GLB, 3DS). Native CAD files are compared through
their STEP export – attach one. Point clouds are not compared.

## How it works

When a 3D file is uploaded (or first shown), the preview process computes a
small **shape signature** of its mesh, once per file content:

| Part of the signature  | What it is                                                                                     |
|------------------------|------------------------------------------------------------------------------------------------|
| Principal extents      | edge lengths of the tightest box around the part, largest first – independent of how the part lies in the file |
| Surface area, volume   | the volume only counts when the mesh is watertight; B-rep exports often are not, then it is left out |
| Shape distribution (D2) | histogram of the distances between 32 768 random point pairs on the surface, relative to the box diagonal – a fingerprint of the form that ignores position, rotation and size |
| Triangles              | the size of the mesh                                                                            |

Two parts are compared with

* **shape** = 1 − ½ · Σ |difference of the D2 histograms| (100 % = same form),
* **size** = exp(−2 · mean |ln(extent ratio)|) over the three extents
  (100 % = same size; 5 % larger in every direction gives about 90 %, twice
  the size about 25 %),
* **score** = shape × size.

Hits below 50 % are not shown; each item appears once (its best file), and
only its latest revision counts. Units are the file's model units (usually
millimetres); a part exported in inches is therefore "smaller".

**Ignore scale** (`ignore_scale=true` in the API and the MCP tool) compares
the proportions (middle and smallest extent relative to the largest) instead
of the extents, so scaled variants – M6 and M8 versions of a part – score
like copies.

Everyone who can sign in sees the hits, exactly like the item search.

## Limits

The signature looks at the outer form. Parts that differ only in small
features (a hole more, a different chamfer) score high – that is the point
of the search, but it is no proof that they are interchangeable. Mirror
images score as the same shape. Very different tessellations of the same
part (a coarse STL vs a fine STEP) agree on extents and differ a little in
the shape distribution.

The search is fast for large vaults: an indexed prefilter on the extents
(within a factor of three) or proportions keeps at most 2000 files, which are
scored in memory.

Files stored before this feature get their signature in the background when
their revision page or 3D view is first opened (the page then says
*reload in a moment*), or all at once with `teamcraft vault signatures` (see
[Operations](../admin/operations.md); `--convert` also converts files that
were never previewed).
