# FieldCraft and SolidCraft

[SolidCraft](https://github.com/tangency-labs/solidcraft) (parametric CAD)
and [FieldCraft](https://github.com/tangency-labs/fieldcraft) (FEA/CFD) are
open-source engineering tools in pure Rust by Tangency Labs (MIT OR
Apache-2.0). TeamCraft manages their files like those of any other CAD
program:

| File                                                                                                                                | TeamCraft                                                                                                                        |
|-------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------|
| SolidCraft design `.sc` (ZIP container with `container.json`, or JSON with `"format": "solidcraft/N"`) and the legacy `.solidcraft` | recognised as *SolidCraft design* (`.sc` is checked by content, so scripts with the same extension are not mistaken for designs) |
| FieldCraft model `.fieldcraft` (ZIP: `fieldcraft.json`, `model.json`, `mesh.bin`, `journal.json`)                                   | recognised as *FieldCraft analysis model*                                                                                        |
| STEP, 3MF (incl. SolidCraft's face-tagged 3MF), STL, OBJ exports                                                                    | 3D preview in the browser and the desktop app; STEP is searchable and scanned for references                                     |
| Abaqus input decks (`.inp`) written by FieldCraft                                                                                   | recognised as *Abaqus input file*                                                                                                |

A typical workflow: keep the `.sc` design together with its STEP or 3MF
export in one TeamCraft revision – the design for editing in SolidCraft, the
export for the preview, the BOM and downstream tools; attach the
`.fieldcraft` model to the same item, or to an analysis document that
references it.

The three projects share their approach – one Rust build, a desktop app and
a command line, open formats – but not code: TeamCraft is licensed under the
MPL-2.0 and does not include SolidCraft or FieldCraft code.
