//! Simulation results (FEA/CFD) for the 3D preview – pure Rust, no ParaView.
//!
//! Readers for the open result formats most solvers can write:
//!
//! * legacy VTK (`.vtk`): ASCII and BINARY (big endian), `DATASET
//!   UNSTRUCTURED_GRID` and `POLYDATA`, the classic `CELLS` layout and the
//!   `OFFSETS`/`CONNECTIVITY` layout of file version 5;
//! * VTK XML `UnstructuredGrid` (`.vtu`) and `PolyData` (`.vtp`): `ascii`,
//!   inline `binary` (base64) and appended data (`raw` or `base64`), optional
//!   zlib compression, `UInt32`/`UInt64` headers, every integer and float type;
//! * CalculiX results (`.frd`, ASCII): nodes (`2C`), elements (`3C`) and the
//!   nodal result blocks (`-4 DISP`, `-4 STRESS`, …).
//!
//! Every reader fills one `Grid` (points, cells, point and cell data);
//! `Grid::into_mesh` turns it into the preview [`Mesh`]: volume cells
//! (tetrahedra, hexahedra, wedges, pyramids – quadratic ones by their corner
//! nodes) contribute only their outer faces (faces used by exactly one cell),
//! surface cells are drawn as they are, and interior points are dropped.
//!
//! Result values become scalar [`Field`]s on the vertices: one-component
//! data as is, six- or nine-component tensors as von Mises equivalent,
//! other vectors as magnitude. Cell data is averaged to the points. At most
//! [`MAX_FIELDS`] fields are kept – the viewer offers them in a selector.
//!
//! Like the mesh readers, everything is defensive: counts are limited and
//! checked against the data before anything is allocated, decompression is
//! bounded by the sizes the arrays need, indices are checked – a hostile file
//! is an error, never a panic, an endless loop or an unbounded allocation.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

mod frd;
mod grid;
mod numbers;
mod vtk_legacy;
mod vtk_xml;

use grid::*;
use numbers::*;

pub use frd::frd;
pub use vtk_legacy::vtk_legacy;
pub use vtk_xml::vtk_xml;

/// Points of one result file.
pub const MAX_NODES: usize = 20_000_000;
/// Cells (elements) of one result file.
pub const MAX_CELLS: usize = 20_000_000;
/// Node references of all cells together.
pub const MAX_CONNECTIVITY: usize = 160_000_000;
/// Surfaces with more triangles are thinned out evenly (gaps are visible,
/// the viewer stays fast).
pub const MAX_RESULT_TRIANGLES: usize = 2_000_000;
/// Field values carried at most (fields × vertices).
const FIELD_BUDGET: usize = 24_000_000;
/// Components of one data array.
const MAX_COMPONENTS: usize = 64;
/// Decoded bytes of one XML data array.
const MAX_ARRAY_BYTES: usize = 1 << 31;

#[cfg(test)]
mod tests;
