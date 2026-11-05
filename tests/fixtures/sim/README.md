# Simulation result fixtures (self-made)

Every file here was written for TeamCraft's tests – no solver output, no third-party
data, same licence as the repository (MPL-2.0). They all describe the same tiny model:
two unit hexahedra side by side along x (12 nodes), so the readers can be checked
against each other (`src/simmesh/tests.rs`).

| File                    | Format                                                         | Made by                    |
|-------------------------|----------------------------------------------------------------|----------------------------|
| `beam-ascii.vtk`        | legacy VTK 3.0, ASCII, point data `temperature`, `displacement`, cell data `material` | hand-written |
| `beam-binary.vtk`       | legacy VTK 3.0, BINARY (big endian), same content              | generator                  |
| `beam-ascii.vtu`        | VTK XML UnstructuredGrid, `ascii`, plus a 6-component `stress` and an `InformationKey` | hand-written |
| `beam-base64.vtu`       | VTK XML, inline `binary` (base64), UInt32 headers              | generator                  |
| `beam-appended-raw.vtu` | VTK XML, raw appended data, UInt64 headers                     | generator                  |
| `beam-zlib.vtu`         | VTK XML, inline base64 with zlib blocks, UInt64 headers        | generator                  |
| `beam.frd`              | CalculiX FRD, ASCII long format: nodes, elements, `DISP` (two increments), `STRESS` | generator |

The generator is the `generate` module in `src/simmesh/tests.rs`; rewrite the files with

```sh
TEAMCRAFT_WRITE_SIM_FIXTURES=1 cargo test --lib write_sim_fixtures -- --ignored
```

The tests check that the uncompressed generated files are current.
