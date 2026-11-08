//! File formats of the common CAD programs.
//!
//! TeamCraft stores files of any type. On top of that it knows the native
//! formats of the common CAD, BIM and 3D programs – SolidWorks, Siemens NX /
//! Solid Edge, Autodesk Inventor / Fusion / AutoCAD / Revit, PTC Creo,
//! CATIA V5, Rhino, Blender, Maya, … – the neutral exchange formats (STEP,
//! IGES, Parasolid, JT, ACIS, IFC, STL, glTF, …) and point clouds (E57, LAS):
//!
//! * every file is classified when it is uploaded; the id is stored in
//!   `files.format` (`''` = unknown) and the file list shows what the file is,
//! * downloads carry the registered content type where one exists,
//! * the item search filters by CAD program and finds "SolidWorks", "NX", …,
//! * Creo iteration suffixes are dropped (`bracket.prt.7` → `bracket.prt`), so
//!   every save in Creo becomes a new *version* of the same file,
//! * extensions used by two programs are told apart by the file content:
//!   `.prt` is Creo (text header `#UGC:`) or NX, `.asm` Creo or Solid Edge
//!   (OLE compound document), `.sch` KiCad 5 (`EESchema`) or Eagle, `.sc` a
//!   SolidCraft design (ZIP container or JSON) or a script, `.pdf` with an
//!   embedded U3D/PRC model is a 3D PDF.
//! * simulation models and results (FEA/CFD) are recognised the same way.
//!   `.sim`, `.fem` and `.afm` are NX when they start with the `SPLMSSTR`
//!   container; a `.sim` that names STAR-CCM+ near its start is STAR-CCM+,
//!   any other `.sim` stays NX; a text `.fem` is an OptiStruct deck. Generic
//!   extensions only count with matching content: `.dat` only with Fluent's
//!   `(0 …)` sections or Nastran bulk-data markers, `.msh` with Fluent
//!   sections or Gmsh's `$MeshFormat`, `.java` only naming STAR-CCM+, `.out`
//!   only as a Fluent report file, `.cas.h5`/`.dat.h5`/`.msh.h5` only with
//!   the HDF5 signature and `.cas.gz`/… with the gzip magic, `.e` only as
//!   netCDF/HDF5 (Exodus II), `.db` not for SQLite/OLE/text files, `.rst` not for text (reStructuredText), `.msg`
//!   and `.sta` only as text (not Outlook mails), `.odb` not as ZIP
//!   (OpenDocument database), `.bdf` not for bitmap fonts. OpenFOAM cases
//!   are recognised by `controlDict` (or a `.foam` marker), LS-DYNA results
//!   by `d3plot`, `d3plot01`, …
//!
//! Native formats are proprietary; TeamCraft identifies and versions them,
//! scans the ones with readable references (see `cadrefs`) and previews the
//! open formats in 3D (see `preview`). Onshape keeps its documents in the
//! cloud – store their STEP/Parasolid exports.
//!
//! Extensions are matched case-insensitively (`.CATPart`, `.SLDPRT`).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use serde::Serialize;

/// What a file of the format contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatKind {
    /// Native part (single component).
    Part,
    /// Native assembly / product structure; references other files.
    Assembly,
    /// 2D drawing (native or DWG/DXF).
    Drawing,
    /// Native file that holds parts, assemblies or drawings (NX, Fusion, …).
    Model,
    /// Neutral B-rep exchange format (STEP, IGES, Parasolid, …).
    Exchange,
    /// Triangle mesh (STL, OBJ, 3MF, …), e.g. for 3D printing.
    Mesh,
    /// Lightweight visualisation format (JT, CGR, glTF, …).
    Visualization,
    /// Electronics design (schematics, boards, Gerber).
    Electronics,
    /// Simulation and analysis models, solver input decks.
    Simulation,
    /// Simulation results (fields, solver output, logs).
    Result,
    /// Manufacturing data (CAM, G-code).
    Manufacturing,
    /// Documents and images.
    Document,
    /// Templates, libraries, project files.
    Other,
}

impl FormatKind {
    pub const ALL: [FormatKind; 13] = [
        FormatKind::Part,
        FormatKind::Assembly,
        FormatKind::Drawing,
        FormatKind::Model,
        FormatKind::Exchange,
        FormatKind::Mesh,
        FormatKind::Visualization,
        FormatKind::Electronics,
        FormatKind::Simulation,
        FormatKind::Result,
        FormatKind::Manufacturing,
        FormatKind::Document,
        FormatKind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FormatKind::Part => "part",
            FormatKind::Assembly => "assembly",
            FormatKind::Drawing => "drawing",
            FormatKind::Model => "model",
            FormatKind::Exchange => "exchange",
            FormatKind::Mesh => "mesh",
            FormatKind::Visualization => "visualization",
            FormatKind::Electronics => "electronics",
            FormatKind::Simulation => "simulation",
            FormatKind::Result => "result",
            FormatKind::Manufacturing => "manufacturing",
            FormatKind::Document => "document",
            FormatKind::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FormatKind::Part => "Part",
            FormatKind::Assembly => "Assembly",
            FormatKind::Drawing => "Drawing",
            FormatKind::Model => "CAD model",
            FormatKind::Exchange => "Exchange format",
            FormatKind::Mesh => "Mesh",
            FormatKind::Visualization => "Visualisation",
            FormatKind::Electronics => "Electronics",
            FormatKind::Simulation => "Simulation",
            FormatKind::Result => "Simulation result",
            FormatKind::Manufacturing => "Manufacturing",
            FormatKind::Document => "Document",
            FormatKind::Other => "Other",
        }
    }
}

/// A CAD program (or a group of neutral formats).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct CadSystem {
    pub id: &'static str,
    pub name: &'static str,
    /// Short name used in labels and the search ("NX", "Creo", …).
    pub short: &'static str,
    pub vendor: &'static str,
    /// Further names the search understands ("Unigraphics", "Pro/E", …).
    #[serde(skip)]
    pub aliases: &'static [&'static str],
}

/// A file format.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct CadFormat {
    /// Stable id, stored in `files.format`.
    pub id: &'static str,
    pub label: &'static str,
    /// Id of the [`CadSystem`].
    pub system: &'static str,
    pub kind: FormatKind,
    /// Lower-case extensions without the dot.
    pub extensions: &'static [&'static str],
    /// Registered content type; `""` keeps the type sent by the client
    /// (documents) and native CAD formats use `application/octet-stream`.
    #[serde(skip)]
    pub mime: &'static str,
}

const OCTET: &str = "application/octet-stream";

macro_rules! system {
    ($id:literal, $name:literal, $short:literal, $vendor:literal $(, $alias:literal)*) => {
        CadSystem { id: $id, name: $name, short: $short, vendor: $vendor, aliases: &[$($alias),*] }
    };
}

macro_rules! cad_format {
    ($id:literal, $label:literal, $system:literal, $kind:ident, [$($ext:literal),+], $mime:expr) => {
        CadFormat {
            id: $id,
            label: $label,
            system: $system,
            kind: FormatKind::$kind,
            extensions: &[$($ext),+],
            mime: $mime,
        }
    };
}

/// The CAD programs, in the order of the help page and the search filter.
#[rustfmt::skip]
pub const SYSTEMS: &[CadSystem] = &[
    system!("solidworks", "SolidWorks", "SolidWorks", "Dassault Systèmes", "sw"),
    system!("nx", "Siemens NX", "NX", "Siemens", "unigraphics", "ug", "nx cad"),
    system!("teamcenter", "Siemens Teamcenter (JT, PLM XML)", "Teamcenter", "Siemens", "tc"),
    system!("solid-edge", "Siemens Solid Edge", "Solid Edge", "Siemens", "solidedge"),
    system!("inventor", "Autodesk Inventor", "Inventor", "Autodesk"),
    system!("fusion", "Autodesk Fusion", "Fusion", "Autodesk", "fusion 360", "fusion360"),
    system!("autocad", "AutoCAD (DWG, DXF)", "AutoCAD", "Autodesk"),
    system!("creo", "PTC Creo (Pro/ENGINEER)", "Creo", "PTC", "pro/engineer", "proe", "pro/e", "creo parametric"),
    system!("catia", "CATIA V5", "CATIA", "Dassault Systèmes", "catia v5", "3dexperience"),
    system!("spaceclaim", "Ansys SpaceClaim / Discovery", "SpaceClaim", "Ansys", "discovery"),
    system!("freecad", "FreeCAD", "FreeCAD", "open source"),
    system!("tangency", "SolidCraft and FieldCraft", "SolidCraft", "Tangency Labs (open source)", "fieldcraft", "tangency"),
    system!("rhino", "Rhino", "Rhino", "Robert McNeel & Associates", "rhinoceros"),
    system!("alias", "Autodesk Alias", "Alias", "Autodesk", "alias autostudio"),
    system!("openscad", "OpenSCAD", "OpenSCAD", "open source"),
    system!("revit", "Autodesk Revit", "Revit", "Autodesk"),
    system!("navisworks", "Autodesk Navisworks", "Navisworks", "Autodesk"),
    system!("microstation", "Bentley MicroStation", "MicroStation", "Bentley Systems", "dgn"),
    system!("archicad", "Graphisoft Archicad", "Archicad", "Graphisoft"),
    system!("sketchup", "SketchUp", "SketchUp", "Trimble"),
    system!("bim", "Open BIM (IFC)", "IFC", "buildingSMART", "open bim", "industry foundation classes"),
    system!("blender", "Blender", "Blender", "Blender Foundation"),
    system!("maya", "Autodesk Maya", "Maya", "Autodesk"),
    system!("3dsmax", "Autodesk 3ds Max", "3ds Max", "Autodesk", "3dsmax", "3d studio max"),
    system!("cinema4d", "Maxon Cinema 4D", "Cinema 4D", "Maxon", "c4d"),
    system!("modo", "Foundry Modo", "Modo", "Foundry"),
    system!("houdini", "SideFX Houdini", "Houdini", "SideFX"),
    system!("exchange", "Neutral exchange formats", "Neutral", "ISO / vendor independent", "neutral"),
    system!("mesh", "Mesh and 3D printing", "Mesh", "vendor independent", "3d printing"),
    system!("visualization", "Visualisation formats", "Visualisation", "vendor independent", "visualization"),
    system!("pointcloud", "Point clouds and scans", "Point cloud", "vendor independent", "scan", "lidar", "point clouds"),
    system!("kicad", "KiCad", "KiCad", "open source"),
    system!("altium", "Altium Designer", "Altium", "Altium"),
    system!("eagle", "Eagle", "Eagle", "Autodesk"),
    system!("pcb-fab", "PCB manufacturing (Gerber, drill)", "PCB fab", "vendor independent", "gerber"),
    system!("rocketry", "Rocketry (OpenRocket, RockSim, RASAero)", "Rocketry", "various", "openrocket", "rocksim", "rasaero"),
    system!("cae", "Simulation (ANSYS, Abaqus, COMSOL, Nastran, CalculiX, LS-DYNA)", "Simulation", "various", "ansys", "abaqus", "comsol", "nastran", "calculix", "ls-dyna", "lsdyna", "fea", "optistruct", "simcenter"),
    system!("cfd", "CFD (OpenFOAM, Fluent, STAR-CCM+)", "CFD", "various", "openfoam", "fluent", "star-ccm+", "starccm"),
    system!("cae-open", "Open simulation data (VTK, CGNS, Exodus II, Gmsh)", "VTK/CGNS", "vendor independent", "vtk", "paraview", "cgns", "exodus", "gmsh"),
    system!("cam", "CNC and 3D printer programs (G-code)", "G-code", "vendor independent", "gcode", "cnc"),
    system!("documents", "Documents and images", "Document", "various"),
];

/// Every known format. Extensions are unique except the ones in
/// [`AMBIGUOUS`], which [`detect`] resolves from the file content.
#[rustfmt::skip]
pub const FORMATS: &[CadFormat] = &[
    // SolidWorks
    cad_format!("solidworks-part", "SolidWorks part", "solidworks", Part, ["sldprt"], OCTET),
    cad_format!("solidworks-assembly", "SolidWorks assembly", "solidworks", Assembly, ["sldasm"], OCTET),
    cad_format!("solidworks-drawing", "SolidWorks drawing", "solidworks", Drawing, ["slddrw"], OCTET),
    cad_format!("solidworks-template", "SolidWorks template / sheet format", "solidworks", Other, ["prtdot", "asmdot", "drwdot", "slddrt"], OCTET),
    cad_format!("solidworks-library", "SolidWorks library feature / block", "solidworks", Other, ["sldlfp", "sldblk"], OCTET),
    // Siemens NX
    cad_format!("nx-part", "NX part", "nx", Model, ["prt"], OCTET),
    cad_format!("nx-simulation", "NX simulation / FE model", "nx", Simulation, ["sim", "fem", "afm"], OCTET),
    // Siemens Teamcenter
    cad_format!("jt", "JT visualisation (ISO 14306)", "teamcenter", Visualization, ["jt"], "model/jt"),
    cad_format!("plmxml", "Teamcenter PLM XML", "teamcenter", Exchange, ["plmxml"], "application/xml"),
    // Siemens Solid Edge
    cad_format!("solidedge-part", "Solid Edge part", "solid-edge", Part, ["par"], OCTET),
    cad_format!("solidedge-sheetmetal", "Solid Edge sheet metal part", "solid-edge", Part, ["psm"], OCTET),
    cad_format!("solidedge-assembly", "Solid Edge assembly", "solid-edge", Assembly, ["asm"], OCTET),
    cad_format!("solidedge-weldment", "Solid Edge weldment", "solid-edge", Assembly, ["pwd"], OCTET),
    cad_format!("solidedge-drawing", "Solid Edge drawing", "solid-edge", Drawing, ["dft"], OCTET),
    // Autodesk Inventor
    cad_format!("inventor-part", "Inventor part", "inventor", Part, ["ipt"], OCTET),
    cad_format!("inventor-assembly", "Inventor assembly", "inventor", Assembly, ["iam"], OCTET),
    cad_format!("inventor-drawing", "Inventor drawing", "inventor", Drawing, ["idw"], OCTET),
    cad_format!("inventor-presentation", "Inventor presentation", "inventor", Other, ["ipn"], OCTET),
    cad_format!("inventor-project", "Inventor project file", "inventor", Other, ["ipj"], OCTET),
    // Autodesk Fusion
    cad_format!("fusion-design", "Fusion design archive", "fusion", Model, ["f3d"], OCTET),
    cad_format!("fusion-distributed", "Fusion distributed design (with references)", "fusion", Model, ["f3z"], OCTET),
    cad_format!("fusion-drawing", "Fusion drawing archive", "fusion", Drawing, ["f2d"], OCTET),
    // AutoCAD
    cad_format!("dwg", "DWG drawing (AutoCAD, Inventor)", "autocad", Drawing, ["dwg"], "image/vnd.dwg"),
    cad_format!("dxf", "DXF drawing exchange", "autocad", Drawing, ["dxf"], "image/vnd.dxf"),
    cad_format!("autocad-template", "AutoCAD drawing template", "autocad", Other, ["dwt"], OCTET),
    // PTC Creo
    cad_format!("creo-part", "Creo part", "creo", Part, ["prt"], OCTET),
    cad_format!("creo-assembly", "Creo assembly", "creo", Assembly, ["asm"], OCTET),
    cad_format!("creo-drawing", "Creo drawing", "creo", Drawing, ["drw"], OCTET),
    cad_format!("creo-format", "Creo drawing format", "creo", Other, ["frm"], OCTET),
    cad_format!("creo-layout", "Creo layout", "creo", Other, ["lay"], OCTET),
    cad_format!("creo-sketch", "Creo sketch", "creo", Other, ["sec"], OCTET),
    cad_format!("creo-manufacturing", "Creo manufacturing model", "creo", Manufacturing, ["mfg"], OCTET),
    cad_format!("creo-neutral", "Creo neutral file", "creo", Exchange, ["neu"], OCTET),
    cad_format!("creo-view", "Creo View visualisation", "creo", Visualization, ["pvz", "pvs"], OCTET),
    // CATIA V5 / 3DEXPERIENCE
    cad_format!("catia-part", "CATIA part", "catia", Part, ["catpart"], OCTET),
    cad_format!("catia-product", "CATIA product (assembly)", "catia", Assembly, ["catproduct"], OCTET),
    cad_format!("catia-drawing", "CATIA drawing", "catia", Drawing, ["catdrawing"], OCTET),
    cad_format!("catia-cgr", "CATIA CGR visualisation", "catia", Visualization, ["cgr"], OCTET),
    cad_format!("catia-analysis", "CATIA analysis", "catia", Simulation, ["catanalysis"], OCTET),
    cad_format!("catia-process", "CATIA process", "catia", Manufacturing, ["catprocess"], OCTET),
    cad_format!("catia-material", "CATIA material library", "catia", Other, ["catmaterial"], OCTET),
    cad_format!("3dxml", "3D XML (3DEXPERIENCE)", "catia", Visualization, ["3dxml"], OCTET),
    // Ansys SpaceClaim / Discovery
    cad_format!("spaceclaim", "SpaceClaim document", "spaceclaim", Model, ["scdoc"], OCTET),
    // Further CAD programs
    cad_format!("freecad", "FreeCAD document", "freecad", Model, ["fcstd"], OCTET),
    cad_format!("solidcraft-design", "SolidCraft design", "tangency", Model, ["sc", "solidcraft"], OCTET),
    cad_format!("fieldcraft-model", "FieldCraft analysis model", "tangency", Simulation, ["fieldcraft"], OCTET),
    cad_format!("rhino", "Rhino 3D model", "rhino", Model, ["3dm"], OCTET),
    cad_format!("alias-wire", "Alias wire file", "alias", Model, ["wire"], OCTET),
    cad_format!("openscad", "OpenSCAD script", "openscad", Model, ["scad"], "text/plain"),
    // BIM / architecture / infrastructure
    cad_format!("revit-project", "Revit project", "revit", Model, ["rvt"], OCTET),
    cad_format!("revit-family", "Revit family (component)", "revit", Part, ["rfa"], OCTET),
    cad_format!("revit-template", "Revit template", "revit", Other, ["rte", "rft"], OCTET),
    cad_format!("navisworks-document", "Navisworks document", "navisworks", Visualization, ["nwd"], OCTET),
    cad_format!("navisworks-cache", "Navisworks cache", "navisworks", Visualization, ["nwc"], OCTET),
    cad_format!("navisworks-file-set", "Navisworks file set (with references)", "navisworks", Other, ["nwf"], OCTET),
    cad_format!("dgn", "MicroStation design (DGN)", "microstation", Model, ["dgn"], OCTET),
    cad_format!("archicad-project", "Archicad project", "archicad", Model, ["pln"], OCTET),
    cad_format!("archicad-archive", "Archicad archive", "archicad", Model, ["pla"], OCTET),
    cad_format!("sketchup", "SketchUp model", "sketchup", Model, ["skp"], OCTET),
    cad_format!("ifc", "IFC building model (ISO 16739)", "bim", Exchange, ["ifc"], OCTET),
    cad_format!("ifc-xml", "IFC building model (XML)", "bim", Exchange, ["ifcxml"], "application/xml"),
    cad_format!("ifc-zip", "IFC building model (zipped)", "bim", Exchange, ["ifczip"], OCTET),
    // 3D / DCC / animation
    cad_format!("blender", "Blender scene", "blender", Model, ["blend"], OCTET),
    cad_format!("maya-ascii", "Maya scene (ASCII)", "maya", Model, ["ma"], OCTET),
    cad_format!("maya-binary", "Maya scene (binary)", "maya", Model, ["mb"], OCTET),
    cad_format!("3dsmax-scene", "3ds Max scene", "3dsmax", Model, ["max"], OCTET),
    cad_format!("3ds", "3DS mesh (legacy 3D Studio)", "3dsmax", Mesh, ["3ds"], "application/x-3ds"),
    cad_format!("cinema4d", "Cinema 4D scene", "cinema4d", Model, ["c4d"], OCTET),
    cad_format!("modo", "Modo scene", "modo", Model, ["lxo"], OCTET),
    cad_format!("houdini", "Houdini scene", "houdini", Model, ["hip", "hipnc", "hiplc"], OCTET),
    // Neutral exchange formats
    cad_format!("step", "STEP (ISO 10303)", "exchange", Exchange, ["step", "stp", "p21"], "model/step"),
    cad_format!("iges", "IGES", "exchange", Exchange, ["iges", "igs"], "model/iges"),
    cad_format!("parasolid-text", "Parasolid (text)", "exchange", Exchange, ["x_t", "xmt_txt"], "model/vnd.parasolid.transmit.text"),
    cad_format!("parasolid-binary", "Parasolid (binary)", "exchange", Exchange, ["x_b", "xmt_bin"], "model/vnd.parasolid.transmit.binary"),
    cad_format!("acis-text", "ACIS (text)", "exchange", Exchange, ["sat"], OCTET),
    cad_format!("acis-binary", "ACIS (binary)", "exchange", Exchange, ["sab"], OCTET),
    cad_format!("brep", "OpenCascade BREP", "exchange", Exchange, ["brep", "brp"], OCTET),
    cad_format!("vda-fs", "VDA-FS", "exchange", Exchange, ["vda"], OCTET),
    // Meshes and 3D printing
    cad_format!("stl", "STL mesh", "mesh", Mesh, ["stl"], "model/stl"),
    cad_format!("obj", "Wavefront OBJ mesh", "mesh", Mesh, ["obj"], "model/obj"),
    cad_format!("3mf", "3MF (3D printing)", "mesh", Mesh, ["3mf"], "model/3mf"),
    cad_format!("ply", "PLY mesh / point cloud", "mesh", Mesh, ["ply"], OCTET),
    cad_format!("off", "OFF mesh", "mesh", Mesh, ["off"], OCTET),
    cad_format!("amf", "AMF (3D printing)", "mesh", Mesh, ["amf"], OCTET),
    // Visualisation
    cad_format!("gltf", "glTF", "visualization", Visualization, ["gltf"], "model/gltf+json"),
    cad_format!("glb", "glTF binary", "visualization", Visualization, ["glb"], "model/gltf-binary"),
    cad_format!("fbx", "FBX", "visualization", Visualization, ["fbx"], OCTET),
    cad_format!("collada", "COLLADA", "visualization", Visualization, ["dae"], "model/vnd.collada+xml"),
    cad_format!("usd", "USD scene (Universal Scene Description)", "visualization", Visualization, ["usd", "usda", "usdc"], OCTET),
    cad_format!("usdz", "USDZ (AR)", "visualization", Visualization, ["usdz"], "model/vnd.usdz+zip"),
    cad_format!("alembic", "Alembic cache", "visualization", Visualization, ["abc"], OCTET),
    cad_format!("vrml", "VRML", "visualization", Visualization, ["wrl", "vrml"], "model/vrml"),
    cad_format!("x3d", "X3D", "visualization", Visualization, ["x3d"], "model/x3d+xml"),
    cad_format!("pdf-3d", "3D PDF (U3D / PRC)", "visualization", Visualization, ["pdf"], "application/pdf"),
    // Point clouds and scans
    cad_format!("e57", "E57 point cloud (ASTM E2807)", "pointcloud", Visualization, ["e57"], OCTET),
    cad_format!("las", "LAS point cloud (LiDAR)", "pointcloud", Visualization, ["las"], OCTET),
    cad_format!("laz", "LAZ point cloud (compressed LAS)", "pointcloud", Visualization, ["laz"], OCTET),
    cad_format!("pts", "PTS point cloud", "pointcloud", Visualization, ["pts"], "text/plain"),
    cad_format!("ptx", "PTX point cloud (scanner stations)", "pointcloud", Visualization, ["ptx"], "text/plain"),
    cad_format!("xyz", "XYZ point cloud", "pointcloud", Visualization, ["xyz"], "text/plain"),
    cad_format!("pcd", "PCD point cloud (PCL)", "pointcloud", Visualization, ["pcd"], OCTET),
    // Electronics
    cad_format!("kicad-pcb", "KiCad board", "kicad", Electronics, ["kicad_pcb"], OCTET),
    cad_format!("kicad-schematic", "KiCad schematic", "kicad", Electronics, ["kicad_sch"], OCTET),
    cad_format!("kicad-project", "KiCad project", "kicad", Electronics, ["kicad_pro"], OCTET),
    cad_format!("kicad-library", "KiCad symbol / footprint library", "kicad", Electronics, ["kicad_sym", "kicad_mod"], OCTET),
    cad_format!("altium-pcb", "Altium board", "altium", Electronics, ["pcbdoc"], OCTET),
    cad_format!("altium-schematic", "Altium schematic", "altium", Electronics, ["schdoc"], OCTET),
    cad_format!("altium-project", "Altium project", "altium", Electronics, ["prjpcb"], OCTET),
    cad_format!("altium-library", "Altium library", "altium", Electronics, ["pcblib", "schlib", "intlib"], OCTET),
    cad_format!("eagle-board", "Eagle board", "eagle", Electronics, ["brd"], OCTET),
    cad_format!("eagle-schematic", "Eagle schematic", "eagle", Electronics, ["sch"], OCTET),
    cad_format!("eagle-library", "Eagle library", "eagle", Electronics, ["lbr"], OCTET),
    cad_format!("gerber", "Gerber (PCB manufacturing)", "pcb-fab", Electronics, ["gbr", "gerber", "ger"], OCTET),
    cad_format!("excellon", "Excellon drill file", "pcb-fab", Electronics, ["drl", "xln"], OCTET),
    // Rocketry
    cad_format!("openrocket", "OpenRocket design", "rocketry", Simulation, ["ork"], OCTET),
    cad_format!("rocksim", "RockSim design", "rocketry", Simulation, ["rkt"], OCTET),
    cad_format!("rasaero", "RASAero II project", "rocketry", Simulation, ["cdx1"], OCTET),
    cad_format!("motor-eng", "Motor thrust curve (RASP .eng)", "rocketry", Simulation, ["eng"], "text/plain"),
    cad_format!("motor-rse", "Motor thrust curve (RockSim .rse)", "rocketry", Simulation, ["rse"], "application/xml"),
    // Simulation
    cad_format!("ansys-workbench", "ANSYS Workbench project", "cae", Simulation, ["wbpj"], OCTET),
    cad_format!("abaqus-cae", "Abaqus model database", "cae", Simulation, ["cae"], OCTET),
    cad_format!("abaqus-input", "Abaqus input file", "cae", Simulation, ["inp"], "text/plain"),
    cad_format!("comsol", "COMSOL model", "cae", Simulation, ["mph"], OCTET),
    cad_format!("abaqus-odb", "Abaqus output database (results)", "cae", Result, ["odb"], OCTET),
    cad_format!("abaqus-status", "Abaqus status file", "cae", Result, ["sta"], "text/plain"),
    cad_format!("abaqus-message", "Abaqus message file", "cae", Result, ["msg"], "text/plain"),
    cad_format!("ansys-result", "ANSYS result file", "cae", Result, ["rst"], OCTET),
    cad_format!("ansys-thermal-result", "ANSYS thermal result file", "cae", Result, ["rth"], OCTET),
    cad_format!("ansys-database", "ANSYS Mechanical APDL database", "cae", Simulation, ["db"], OCTET),
    cad_format!("nastran-bulk", "Nastran input deck (bulk data)", "cae", Simulation, ["bdf", "nas", "dat"], "text/plain"),
    cad_format!("nastran-op2", "Nastran binary results (OP2)", "cae", Result, ["op2"], OCTET),
    cad_format!("nastran-f06", "Nastran print output (F06)", "cae", Result, ["f06"], "text/plain"),
    cad_format!("nastran-f04", "Nastran execution summary (F04)", "cae", Result, ["f04"], "text/plain"),
    cad_format!("nastran-punch", "Nastran punch output (PCH)", "cae", Result, ["pch"], "text/plain"),
    cad_format!("optistruct-deck", "OptiStruct / HyperMesh input deck", "cae", Simulation, ["fem"], "text/plain"),
    cad_format!("calculix-frd", "CalculiX results (FRD)", "cae", Result, ["frd"], "text/plain"),
    cad_format!("lsdyna-keyword", "LS-DYNA keyword input deck", "cae", Simulation, ["k"], "text/plain"),
    cad_format!("lsdyna-d3plot", "LS-DYNA results (d3plot)", "cae", Result, ["d3plot"], OCTET),
    // CFD
    cad_format!("openfoam-case", "OpenFOAM case (controlDict, .foam marker)", "cfd", Simulation, ["foam"], "text/plain"),
    cad_format!("fluent-case", "Fluent case file", "cfd", Simulation, ["cas"], OCTET),
    cad_format!("fluent-data", "Fluent data file (results)", "cfd", Result, ["dat"], OCTET),
    cad_format!("fluent-case-h5", "Fluent case file (HDF5)", "cfd", Simulation, ["cas.h5"], OCTET),
    cad_format!("fluent-data-h5", "Fluent data file (HDF5, results)", "cfd", Result, ["dat.h5"], OCTET),
    cad_format!("fluent-mesh", "Fluent mesh file", "cfd", Mesh, ["msh"], OCTET),
    cad_format!("fluent-mesh-h5", "Fluent mesh file (HDF5)", "cfd", Mesh, ["msh.h5"], OCTET),
    cad_format!("fluent-case-gz", "Fluent case file (gzip)", "cfd", Simulation, ["cas.gz"], "application/gzip"),
    cad_format!("fluent-data-gz", "Fluent data file (gzip, results)", "cfd", Result, ["dat.gz"], "application/gzip"),
    cad_format!("fluent-mesh-gz", "Fluent mesh file (gzip)", "cfd", Mesh, ["msh.gz"], "application/gzip"),
    cad_format!("fluent-project", "Fluent project", "cfd", Simulation, ["flprj"], OCTET),
    cad_format!("fluent-cdat", "Fluent CFD-Post data (results)", "cfd", Result, ["cdat"], OCTET),
    cad_format!("fluent-journal", "Fluent journal (run script)", "cfd", Simulation, ["jou"], "text/plain"),
    cad_format!("fluent-transcript", "Fluent transcript (solver log)", "cfd", Result, ["trn"], "text/plain"),
    cad_format!("fluent-report", "Fluent report file (monitor values)", "cfd", Result, ["out"], "text/plain"),
    cad_format!("starccm-sim", "STAR-CCM+ simulation", "cfd", Simulation, ["sim"], OCTET),
    cad_format!("starccm-history", "STAR-CCM+ solution history (results)", "cfd", Result, ["simh"], OCTET),
    cad_format!("starccm-macro", "STAR-CCM+ macro (Java)", "cfd", Simulation, ["java"], "text/plain"),
    // Open simulation data formats
    cad_format!("vtk-legacy", "VTK legacy data", "cae-open", Result, ["vtk"], OCTET),
    cad_format!("vtk-unstructured", "VTK unstructured grid (XML)", "cae-open", Result, ["vtu"], "application/xml"),
    cad_format!("vtk-polydata", "VTK polygonal data (XML)", "cae-open", Result, ["vtp"], "application/xml"),
    cad_format!("vtk-structured", "VTK structured grid (XML)", "cae-open", Result, ["vts"], "application/xml"),
    cad_format!("vtk-rectilinear", "VTK rectilinear grid (XML)", "cae-open", Result, ["vtr"], "application/xml"),
    cad_format!("vtk-collection", "ParaView data collection (time steps)", "cae-open", Result, ["pvd"], "application/xml"),
    cad_format!("vtk-multiblock", "VTK multiblock data set (XML)", "cae-open", Result, ["vtm"], "application/xml"),
    cad_format!("cgns", "CGNS (CFD General Notation System)", "cae-open", Result, ["cgns"], OCTET),
    cad_format!("exodus", "Exodus II mesh and results", "cae-open", Result, ["exo", "e"], OCTET),
    cad_format!("gmsh", "Gmsh mesh", "cae-open", Mesh, ["msh"], OCTET),
    // CNC / 3D printer programs
    cad_format!("gcode", "G-code program", "cam", Manufacturing, ["gcode", "gco", "ngc", "nc", "bgcode"], "text/plain"),
    // Documents and images
    cad_format!("pdf", "PDF document", "documents", Document, ["pdf"], "application/pdf"),
    cad_format!("word", "Text document", "documents", Document, ["docx", "doc", "odt", "rtf"], ""),
    cad_format!("spreadsheet", "Spreadsheet", "documents", Document, ["xlsx", "xls", "ods"], ""),
    cad_format!("csv", "CSV table", "documents", Document, ["csv"], "text/csv"),
    cad_format!("presentation", "Presentation", "documents", Document, ["pptx", "ppt", "odp"], ""),
    cad_format!("text", "Text file", "documents", Document, ["txt", "md"], "text/plain"),
    cad_format!("png", "PNG image", "documents", Document, ["png"], "image/png"),
    cad_format!("jpeg", "JPEG image", "documents", Document, ["jpg", "jpeg"], "image/jpeg"),
    cad_format!("zip", "ZIP archive", "documents", Other, ["zip"], "application/zip"),
    cad_format!("7z", "7-Zip archive", "documents", Other, ["7z"], "application/x-7z-compressed"),
];

/// Extensions shared by two formats; [`detect`] (and for 3D PDFs
/// [`detect_stored`]) decides by content.
pub const AMBIGUOUS: [&str; 9] = [
    "prt", "asm", "pdf", "sim", "fem", "dat", "msh", "java", "out",
];

/// Creo saves every iteration as `<name>.<ext>.<n>`.
const CREO_ITERATED: [&str; 7] = ["prt", "asm", "drw", "frm", "lay", "sec", "mfg"];

/// Bytes of the file content [`detect`] looks at.
pub const HEAD_LEN: usize = 1024;

const OLE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

pub fn by_id(id: &str) -> Option<&'static CadFormat> {
    FORMATS.iter().find(|f| f.id == id)
}

pub fn system(id: &str) -> Option<&'static CadSystem> {
    SYSTEMS.iter().find(|s| s.id == id)
}

fn first_with_extension(ext: &str) -> Option<&'static CadFormat> {
    FORMATS.iter().find(|f| f.extensions.contains(&ext))
}

/// `bracket.prt.12` → `("bracket.prt", 12)`: the Creo iteration suffix.
pub fn creo_iteration(name: &str) -> Option<(&str, u32)> {
    let (base, number) = name.rsplit_once('.')?;
    if number.is_empty() || number.len() > 6 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (stem, ext) = base.rsplit_once('.')?;
    if stem.is_empty() || !CREO_ITERATED.contains(&ext.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some((base, number.parse().ok()?))
}

/// The name a file is stored under: Creo iteration suffixes are dropped.
pub fn normalize_name(name: &str) -> &str {
    creo_iteration(name).map_or(name, |(base, _)| base)
}

/// Lower-case extension of a (normalised) file name; `None` for names
/// without one or hidden files such as `.stl`.
pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = normalize_name(name).rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

fn is_creo(head: &[u8]) -> bool {
    head.starts_with(b"#UGC:")
}

fn is_ole(head: &[u8]) -> bool {
    head.starts_with(&OLE_MAGIC)
}

/// Mostly printable text without NUL bytes (assembler sources, ASCII files).
fn looks_textual(head: &[u8]) -> bool {
    !head.is_empty()
        && !head.contains(&0)
        && head
            .iter()
            .filter(|b| b.is_ascii_graphic() || b.is_ascii_whitespace() || **b >= 0x80)
            .count()
            * 100
            >= head.len() * 95
}

/// Classifies a file by its name and the first [`HEAD_LEN`] bytes of its
/// content (pass `&[]` if the content is not at hand).
pub fn detect(name: &str, head: &[u8]) -> Option<&'static CadFormat> {
    if let Some(format) = detect_by_name(name) {
        return Some(format);
    }
    let iterated = creo_iteration(name).is_some();
    // `head` is empty when only the name is known: then the name decides.
    let known = !head.is_empty();
    if let Some(compound) = compound_extension(name) {
        return detect_compound(compound, head, known);
    }
    let ext = extension(name)?;
    match ext.as_str() {
        // NX writes its simulation and FE model files in the same SPLMSSTR
        // container as its parts (a positive test). STAR-CCM+ names itself
        // near the start of the file; an unrecognised .sim stays NX (older
        // NX releases have no container signature).
        "sim" if is_nx_container(head) => by_id("nx-simulation"),
        "sim" if contains_ascii_ci(head, b"star-ccm") || contains_ascii_ci(head, b"starccm") => {
            by_id("starccm-sim")
        }
        "sim" => by_id("nx-simulation"),
        // .fem: NX FE model (container) or an OptiStruct/HyperMesh text deck.
        "fem" if !known || is_nx_container(head) => by_id("nx-simulation"),
        "fem" if looks_textual(head) => by_id("optistruct-deck"),
        "fem" => None,
        // .afm: NX assembly FEM; Adobe font metrics (text) use it too.
        "afm" if known && !is_nx_container(head) => None,
        // Abaqus writes .msg and .sta as text; Outlook messages are OLE files.
        "msg" | "sta" if known && !looks_textual(head) => None,
        // Text logs and scripts of Nastran and Fluent.
        "f04" | "pch" | "jou" | "trn" if known && !looks_textual(head) => None,
        // OP2 is binary (Fortran records); a text .op2 is something else.
        "op2" if known && looks_textual(head) => None,
        // ANSYS databases are binary; SQLite files and Windows thumbnail
        // caches use .db too.
        "db" if name.eq_ignore_ascii_case("thumbs.db")
            || head.starts_with(b"SQLite format 3")
            || is_ole(head)
            || looks_textual(head) =>
        {
            None
        }
        // ANSYS results are binary; text .rst files are reStructuredText.
        "rst" if looks_textual(head) => None,
        // Bitmap fonts (BDF) start with STARTFONT, Nastran decks do not.
        "bdf" if head.starts_with(b"STARTFONT") => None,
        // OpenDocument databases (LibreOffice Base) are ZIP files.
        "odb" if head.starts_with(b"PK\x03\x04") => None,
        // .dat is everywhere: only Fluent's "(0 …)" section syntax or the
        // markers of a Nastran deck (Simcenter 3D writes its decks as .dat)
        // count.
        "dat" if is_fluent_sections(head) => by_id("fluent-data"),
        "dat" if looks_textual(head) && is_nastran_deck(head) => by_id("nastran-bulk"),
        "dat" => None,
        // .msh: Fluent sections or Gmsh's "$MeshFormat"; anything else
        // (and a name alone) stays unclassified.
        "msh" if is_fluent_mesh(head) => by_id("fluent-mesh"),
        "msh" if trim_start(head).starts_with(b"$MeshFormat") => by_id("gmsh"),
        "msh" => None,
        // Fluent report files: a quoted title line and a header of quoted
        // column names; by name only "report-file-*.out".
        "out" if is_fluent_report(head) => by_id("fluent-report"),
        "out" if !known && is_report_file_name(name) => by_id("fluent-report"),
        "out" => None,
        // Only Java sources that name STAR-CCM+ are macros.
        "java" if is_starccm_macro(head) => by_id("starccm-macro"),
        "java" => None,
        // Exodus II is netCDF or HDF5; other .e files are not.
        "e" if known && !is_netcdf_or_hdf5(head) => None,
        // LS-DYNA keyword decks are text.
        "k" if known && !looks_textual(head) => None,
        // NX and Creo both use .prt; only Creo writes the "#UGC:" text header
        // and iteration suffixes.
        "prt" if iterated || is_creo(head) => by_id("creo-part"),
        "prt" => by_id("nx-part"),
        // Creo (header) or Solid Edge (OLE compound document); plain text is
        // an assembler source, not a CAD file.
        "asm" if iterated || is_creo(head) => by_id("creo-assembly"),
        "asm" if is_ole(head) => by_id("solidedge-assembly"),
        "asm" if looks_textual(head) => None,
        // KiCad 5 schematics start with "EESchema", Eagle's are XML.
        "sch" if head.starts_with(b"EESchema") => by_id("kicad-schematic"),
        // Compiled object files share the extension with OBJ meshes.
        "obj" if !head.is_empty() && head.contains(&0) => None,
        // 3D PDFs are recognised by detect_stored, which reads the whole file.
        "pdf" => by_id("pdf"),
        // SolidCraft: a ZIP container or a JSON document; other .sc files
        // (scripts) are not CAD files.
        "sc" if head.is_empty() || head.starts_with(b"PK\x03\x04") || is_solidcraft_json(head) => {
            by_id("solidcraft-design")
        }
        "sc" => None,
        other => first_with_extension(other),
    }
}

/// Formats recognised by the whole file name rather than its extension:
/// the OpenFOAM `controlDict` (marks a case folder), LS-DYNA `d3plot`,
/// `d3plot01`, … (two-part extensions such as `.cas.h5` are
/// [`compound_extension`]s).
fn detect_by_name(name: &str) -> Option<&'static CadFormat> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    if base == "controlDict" {
        return by_id("openfoam-case");
    }
    if let Some(rest) = lower.strip_prefix("d3plot") {
        if rest.bytes().all(|b| b.is_ascii_digit()) {
            return by_id("lsdyna-d3plot");
        }
    }
    None
}

/// Two-part extensions of Fluent: HDF5 (Common Fluids Format) and gzip
/// compressed legacy files.
const COMPOUND: [&str; 6] = ["cas.h5", "dat.h5", "msh.h5", "cas.gz", "dat.gz", "msh.gz"];

/// The two-part extension of a file name (`pipe.cas.h5` → `cas.h5`), lower
/// case; `None` without a stem.
fn compound_extension(name: &str) -> Option<&'static str> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    COMPOUND.into_iter().find(|ext| {
        lower.len() > ext.len() + 1
            && lower.ends_with(ext)
            && lower.as_bytes().get(lower.len() - ext.len() - 1) == Some(&b'.')
    })
}

/// A two-part extension: the name decides when only the name is known,
/// otherwise the HDF5 signature or the gzip magic must be there too.
fn detect_compound(ext: &str, head: &[u8], known: bool) -> Option<&'static CadFormat> {
    let id = match ext {
        "cas.h5" => "fluent-case-h5",
        "dat.h5" => "fluent-data-h5",
        "msh.h5" => "fluent-mesh-h5",
        "cas.gz" => "fluent-case-gz",
        "dat.gz" => "fluent-data-gz",
        "msh.gz" => "fluent-mesh-gz",
        _ => return None,
    };
    let content_ok = if ext.ends_with(".h5") {
        is_hdf5(head)
    } else {
        head.starts_with(&GZIP_MAGIC)
    };
    if known && !content_ok {
        return None;
    }
    by_id(id)
}

const HDF5_SIGNATURE: &[u8; 8] = b"\x89HDF\r\n\x1a\n";
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// The HDF5 signature at offset 0 or 512 (HDF5 allows a user block in
/// front of the superblock; later offsets 1024, 2048, … lie beyond
/// [`HEAD_LEN`]).
fn is_hdf5(head: &[u8]) -> bool {
    head.starts_with(HDF5_SIGNATURE) || head.get(512..520) == Some(HDF5_SIGNATURE.as_slice())
}

/// The `SPLMSSTR` container NX writes its parts, FE models and simulations
/// in (NX 1980 and later).
fn is_nx_container(head: &[u8]) -> bool {
    head.starts_with(crate::nxpart::MAGIC)
}

/// `head` without leading white space.
fn trim_start(head: &[u8]) -> &[u8] {
    let start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    head.get(start..).unwrap_or_default()
}

/// Lines of a text head (the last, possibly cut line included).
fn head_lines(head: &[u8]) -> impl Iterator<Item = &[u8]> {
    head.split(|b| *b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
}

/// A Nastran bulk-data deck: a line starting with `SOL `, `CEND`,
/// `BEGIN BULK` or `INIT MASTER`, or a `$` comment that names Nastran or
/// Simcenter (the translator comment of Simcenter 3D).
fn is_nastran_deck(head: &[u8]) -> bool {
    head_lines(head).any(|line| {
        let line = trim_start(line);
        // the keyword as a whole word: "CEND", not "CENDRA"
        let starts = |p: &[u8]| {
            line.get(..p.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(p))
                && (p.ends_with(b" ")
                    || !line
                        .get(p.len())
                        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_'))
        };
        starts(b"SOL ")
            || starts(b"CEND")
            || starts(b"BEGIN BULK")
            || starts(b"INIT MASTER")
            || (line.starts_with(b"$")
                && (contains_ascii_ci(line, b"nastran") || contains_ascii_ci(line, b"simcenter")))
    })
}

/// Fluent mesh files: the sections of case files plus the dimension `(2 …)`
/// and node `(10 …)` sections a mesh may start with.
fn is_fluent_mesh(head: &[u8]) -> bool {
    let rest = trim_start(head);
    is_fluent_sections(head) || rest.starts_with(b"(2 ") || rest.starts_with(b"(10 ")
}

/// The tokens of a line made only of double-quoted strings separated by
/// white space, optionally in parentheses (`"Iteration" "report-def-0"` or
/// `("Iteration" "report-def-0")`); `None` for any other line.
fn quoted_tokens(line: &[u8]) -> Option<usize> {
    let mut rest = trim_start(line);
    if let Some(inner) = rest.strip_prefix(b"(") {
        let inner = inner.trim_ascii_end();
        rest = inner.strip_suffix(b")")?;
    }
    let mut count = 0;
    loop {
        rest = trim_start(rest);
        if rest.is_empty() {
            return (count > 0).then_some(count);
        }
        let after = rest.strip_prefix(b"\"")?;
        let end = after.iter().position(|b| *b == b'"')?;
        rest = after.get(end + 1..)?;
        count += 1;
    }
}

/// A Fluent report file (`report-file-0.out`): the first line a quoted
/// title, the second a header of at least two quoted column names.
fn is_fluent_report(head: &[u8]) -> bool {
    let mut lines = head_lines(head).filter(|l| !trim_start(l).is_empty());
    matches!(lines.next().map(quoted_tokens), Some(Some(1)))
        && matches!(lines.next().map(quoted_tokens), Some(Some(n)) if n >= 2)
}

/// `report-file-*.out`, the name Fluent gives its report files.
fn is_report_file_name(name: &str) -> bool {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.to_ascii_lowercase().starts_with("report-file")
}

/// A Java source that names STAR-CCM+ (the header comment of a recorded
/// macro) or imports its API.
fn is_starccm_macro(head: &[u8]) -> bool {
    looks_textual(head)
        && (contains_ascii_ci(head, b"star-ccm+") || contains_ascii_ci(head, b"import star.common"))
}

/// Case-insensitive search for an ASCII needle.
fn contains_ascii_ci(head: &[u8], needle: &[u8]) -> bool {
    head.windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

/// Fluent case and data files consist of `(<index> …)` sections, starting
/// with a comment section `(0 "…")` or the header `(1 "…")`.
fn is_fluent_sections(head: &[u8]) -> bool {
    let start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    let rest = head.get(start..).unwrap_or_default();
    rest.starts_with(b"(0 ") || rest.starts_with(b"(1 ") || rest.starts_with(b"(4 ")
}

/// netCDF classic/64-bit (`CDF\x01`, `CDF\x02`, `CDF\x05`) or HDF5 signature.
fn is_netcdf_or_hdf5(head: &[u8]) -> bool {
    head.starts_with(b"CDF\x01")
        || head.starts_with(b"CDF\x02")
        || head.starts_with(b"CDF\x05")
        || head.starts_with(b"\x89HDF\r\n\x1a\n")
}

/// JSON design of SolidCraft (`"format": "solidcraft/N"`).
fn is_solidcraft_json(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    text.trim_start().starts_with('{')
        && (text.contains("\"solidcraft/") || text.contains("\"solvecraft/"))
}

/// Whether a PDF embeds a 3D model: the 3D stream dictionaries (`/Subtype
/// /U3D` or `/PRC`) are never compressed, so a byte search finds them.
async fn pdf_has_3d(path: &std::path::Path) -> bool {
    pdf_has_3d_within(path, PDF_3D_SCAN_LIMIT).await
}

/// How much of a PDF [`pdf_has_3d`] reads: 3D PDFs are rarely larger, bigger
/// documents are treated as 2D.
const PDF_3D_SCAN_LIMIT: u64 = 512 * 1024 * 1024;

/// [`pdf_has_3d`] reading at most `limit` bytes (rounded up to a whole read).
async fn pdf_has_3d_within(path: &std::path::Path, limit: u64) -> bool {
    use tokio::io::AsyncReadExt;
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let needles: [&[u8]; 2] = [b"/U3D", b"/PRC"];
    let mut buf = vec![0u8; 256 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    let mut scanned = 0u64;
    while scanned < limit {
        let n = match file.read(&mut buf).await {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        scanned += n as u64;
        carry.extend_from_slice(buf.get(..n).unwrap_or_default());
        if needles
            .iter()
            .any(|needle| carry.windows(needle.len()).any(|w| w == *needle))
        {
            return true;
        }
        let keep = carry.len().saturating_sub(8);
        carry.drain(..keep);
    }
    false
}

/// Content type to store for an upload: the registered type of the format,
/// otherwise the (cleaned) type the client sent.
pub fn content_type(format: Option<&CadFormat>, client: &str) -> String {
    match format {
        Some(f) if !f.mime.is_empty() => f.mime.to_string(),
        _ => client.to_string(),
    }
}

/// Classifies a stored file: reads the first [`HEAD_LEN`] bytes of `path`
/// (a missing or unreadable file is classified by its name only).
pub async fn detect_stored(path: &std::path::Path, name: &str) -> Option<&'static CadFormat> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::with_capacity(HEAD_LEN);
    if let Ok(file) = tokio::fs::File::open(path).await {
        let _ = file.take(HEAD_LEN as u64).read_to_end(&mut head).await;
    }
    let format = detect(name, &head);
    if format.is_some_and(|f| f.id == "pdf") && pdf_has_3d(path).await {
        return by_id("pdf-3d");
    }
    format
}

/// What the UI and the API show about a stored format id.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct FormatInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub system: &'static str,
    pub system_name: &'static str,
    pub system_short: &'static str,
    pub kind: FormatKind,
    pub kind_label: &'static str,
    /// Format of the browser 3D preview (`stl`, `step`, `iges`, `brep`).
    pub preview: Option<&'static str>,
    /// Files of this format carry a preview image (see `thumbnail`).
    pub thumbnail: bool,
}

impl CadFormat {
    pub fn info(&'static self) -> FormatInfo {
        let system = system(self.system);
        FormatInfo {
            id: self.id,
            label: self.label,
            system: self.system,
            system_name: system.map_or("", |s| s.name),
            system_short: system.map_or("", |s| s.short),
            kind: self.kind,
            kind_label: self.kind.label(),
            preview: crate::preview::PreviewFormat::of_format(self.id).map(|p| p.as_str()),
            thumbnail: crate::thumbnail::supports(self.id),
        }
    }
}

/// Info for a stored format id (`''` or unknown ids give `None`).
pub fn info(id: &str) -> Option<FormatInfo> {
    by_id(id).map(CadFormat::info)
}

/// Format ids of a CAD program, for the search filter.
pub fn ids_of_system(system_id: &str) -> Vec<String> {
    FORMATS
        .iter()
        .filter(|f| f.system == system_id)
        .map(|f| f.id.to_string())
        .collect()
}

/// Letters and digits only, lower case: "Solid Edge" → "solidedge".
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The label without a trailing explanation: "CATIA product (assembly)" →
/// "CATIA product".
fn label_core(label: &str) -> &str {
    label.split(" (").next().unwrap_or(label)
}

/// Format ids a free-text search names exactly: a CAD program
/// ("SolidWorks", "NX", "Pro/E"), a format ("CATIA product", "Parasolid") or
/// a format id ("jt", "step"). Case, spaces and punctuation do not matter.
pub fn ids_named_by(query: &str) -> Vec<String> {
    let q = squash(query);
    if q.len() < 2 {
        return Vec::new();
    }
    let systems: Vec<&str> = SYSTEMS
        .iter()
        .filter(|s| {
            squash(s.id) == q
                || squash(s.name) == q
                || squash(s.short) == q
                || s.aliases.iter().any(|a| squash(a) == q)
        })
        .map(|s| s.id)
        .collect();
    FORMATS
        .iter()
        .filter(|f| {
            systems.contains(&f.system)
                || squash(f.id) == q
                || squash(f.label) == q
                || squash(label_core(f.label)) == q
        })
        .map(|f| f.id.to_string())
        .collect()
}

/// Systems with their formats, for the help page and `GET /api/v1/formats`.
pub fn catalogue() -> Vec<serde_json::Value> {
    SYSTEMS
        .iter()
        .map(|s| {
            let formats: Vec<_> = FORMATS
                .iter()
                .filter(|f| f.system == s.id)
                .map(|f| {
                    let info = f.info();
                    serde_json::json!({
                        "id": f.id,
                        "label": f.label,
                        "kind": f.kind,
                        "kind_label": f.kind.label(),
                        "extensions": f.extensions,
                        "preview": info.preview,
                        "searchable_text": f.extensions.first().is_some_and(|e| crate::textindex::indexes(&format!("x.{e}"))),
                        "ambiguous": f.extensions.iter().any(|e| AMBIGUOUS.contains(e)),
                        "creo_iterations": f.extensions.iter().any(|e| CREO_ITERATED.contains(e)) && f.system == "creo",
                    })
                })
                .collect();
            serde_json::json!({
                "id": s.id,
                "name": s.name,
                "short": s.short,
                "vendor": s.vendor,
                "formats": formats,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn id(name: &str, head: &[u8]) -> Option<&'static str> {
        detect(name, head).map(|f| f.id)
    }

    #[test]
    fn native_formats_of_the_common_cad_programs() {
        let cases = [
            ("Bracket.SLDPRT", "solidworks-part"),
            ("Rover.sldasm", "solidworks-assembly"),
            ("Rover.SLDDRW", "solidworks-drawing"),
            ("valve.ipt", "inventor-part"),
            ("valve.IAM", "inventor-assembly"),
            ("valve.idw", "inventor-drawing"),
            ("engine.f3d", "fusion-design"),
            ("engine.f3z", "fusion-distributed"),
            ("Nozzle.CATPart", "catia-part"),
            ("Stage.CATProduct", "catia-product"),
            ("Stage.CATDrawing", "catia-drawing"),
            ("plate.par", "solidedge-part"),
            ("plate.psm", "solidedge-sheetmetal"),
            ("plate.dft", "solidedge-drawing"),
            ("frame.drw", "creo-drawing"),
            ("layout.dwg", "dwg"),
            ("cut.DXF", "dxf"),
            ("model.jt", "jt"),
            ("structure.plmxml", "plmxml"),
            ("rover.FCStd", "freecad"),
            ("hull.scdoc", "spaceclaim"),
            ("tower.rvt", "revit-project"),
            ("door.rfa", "revit-family"),
            ("site.dgn", "dgn"),
            ("coordination.nwd", "navisworks-document"),
            ("coordination.nwc", "navisworks-cache"),
            ("coordination.nwf", "navisworks-file-set"),
            ("house.pln", "archicad-project"),
            ("house.pla", "archicad-archive"),
            ("house.skp", "sketchup"),
            ("body.3dm", "rhino"),
            ("body.wire", "alias-wire"),
            ("scene.blend", "blender"),
            ("scene.ma", "maya-ascii"),
            ("scene.mb", "maya-binary"),
            ("scene.max", "3dsmax-scene"),
            ("scene.c4d", "cinema4d"),
            ("scene.lxo", "modo"),
            ("scene.hipnc", "houdini"),
            ("bracket.fieldcraft", "fieldcraft-model"),
            ("bracket.solidcraft", "solidcraft-design"),
        ];
        for (name, expected) in cases {
            assert_eq!(id(name, &[]), Some(expected), "{name}");
        }
    }

    #[test]
    fn neutral_formats() {
        let cases = [
            ("a.step", "step"),
            ("a.STP", "step"),
            ("a.p21", "step"),
            ("a.igs", "iges"),
            ("a.IGES", "iges"),
            ("a.x_t", "parasolid-text"),
            ("a.X_B", "parasolid-binary"),
            ("a.xmt_txt", "parasolid-text"),
            ("a.sat", "acis-text"),
            ("a.sab", "acis-binary"),
            ("a.stl", "stl"),
            ("a.3mf", "3mf"),
            ("a.brep", "brep"),
            ("board.kicad_pcb", "kicad-pcb"),
            ("flight.ork", "openrocket"),
            ("M2000.eng", "motor-eng"),
            ("building.ifc", "ifc"),
            ("building.IFCZIP", "ifc-zip"),
            ("mesh.off", "off"),
            ("legacy.3ds", "3ds"),
            ("scene.dae", "collada"),
            ("scene.usdz", "usdz"),
            ("scene.usda", "usd"),
            ("anim.abc", "alembic"),
            ("scan.e57", "e57"),
            ("scan.las", "las"),
            ("scan.laz", "laz"),
            ("scan.pts", "pts"),
            ("scan.xyz", "xyz"),
            ("report.pdf", "pdf"),
        ];
        for (name, expected) in cases {
            assert_eq!(id(name, &[]), Some(expected), "{name}");
        }
        assert_eq!(id("README", &[]), None);
        assert_eq!(id(".stl", &[]), None, "hidden file without a name");
        assert_eq!(id("archive.unknownext", &[]), None);
    }

    #[test]
    fn ambiguous_extensions_are_resolved_by_content() {
        // .prt: NX unless the Creo header or an iteration suffix says Creo.
        assert_eq!(id("housing.prt", b"\x00\x01binary"), Some("nx-part"));
        assert_eq!(id("housing.prt", &[]), Some("nx-part"));
        assert_eq!(
            id("housing.prt", b"#UGC:2 PART 1182 500 10 1 1 15 2400"),
            Some("creo-part")
        );
        assert_eq!(id("housing.prt.3", b"\x00binary"), Some("creo-part"));
        // .asm: Creo, Solid Edge (OLE) or an assembler source (unknown).
        assert_eq!(id("rover.asm", b"#UGC:2 ASSEMBLY 1"), Some("creo-assembly"));
        assert_eq!(id("rover.asm.12", &[]), Some("creo-assembly"));
        let mut ole = OLE_MAGIC.to_vec();
        ole.extend_from_slice(&[0; 16]);
        assert_eq!(id("rover.asm", &ole), Some("solidedge-assembly"));
        // the OLE signature decides even when the rest of the head is printable
        // (found by cargo mutants: without NULs it would look like a source)
        let mut printable_ole = OLE_MAGIC.to_vec();
        printable_ole.extend_from_slice(&[b'a'; 60]);
        assert_eq!(id("rover.asm", &printable_ole), Some("solidedge-assembly"));
        assert_eq!(id("startup.asm", b"  .section .text\n  mov r0, #1\n"), None);
        // binary, neither Creo nor OLE: still a CAD assembly by its extension
        assert!(id("rover.asm", b"\x00\x01\x02binary").is_some());
        // .sch: KiCad 5 or Eagle.
        assert_eq!(
            id("power.sch", b"EESchema Schematic File Version 4"),
            Some("kicad-schematic")
        );
        assert_eq!(
            id("power.sch", b"<?xml version=\"1.0\"?><!DOCTYPE eagle"),
            Some("eagle-schematic")
        );
        // .obj: mesh (text) or compiled object file (binary).
        assert_eq!(id("hull.obj", b"v 0 0 0\nv 1 0 0\n"), Some("obj"));
        assert_eq!(id("main.obj", b"\x4c\x01\x00\x00"), None);
        // .sc: SolidCraft container (ZIP) or JSON design – or a script.
        assert_eq!(
            id("bracket.sc", b"PK\x03\x04rest"),
            Some("solidcraft-design")
        );
        assert_eq!(
            id("bracket.sc", b"{\n  \"format\": \"solidcraft/24\",\n"),
            Some("solidcraft-design")
        );
        assert_eq!(
            id("synth.sc", b"SynthDef(\\sine, { Out.ar(0, SinOsc.ar) })"),
            None
        );
        // .pdf by name is a document; 3D content is found in the stored file
        assert_eq!(id("model.pdf", b"%PDF-1.7"), Some("pdf"));
    }

    #[tokio::test]
    async fn three_d_pdfs_are_found_by_content() {
        let dir = std::env::temp_dir().join(format!("teamcraft-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let flat = dir.join("flat.pdf");
        std::fs::write(&flat, b"%PDF-1.7\n1 0 obj << /Type /Page >> endobj\n%%EOF").unwrap();
        let mut model = b"%PDF-1.7\n".to_vec();
        model.extend(std::iter::repeat_n(b'x', 300 * 1024)); // across a read boundary
        model.extend_from_slice(b"\n7 0 obj << /Type /3D /Subtype /U3D /Length 9 >>\nstream");
        let three_d = dir.join("model.pdf");
        std::fs::write(&three_d, &model).unwrap();
        assert_eq!(
            detect_stored(&flat, "flat.pdf").await.map(|f| f.id),
            Some("pdf")
        );
        assert_eq!(
            detect_stored(&three_d, "model.pdf").await.map(|f| f.id),
            Some("pdf-3d")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Found by cargo mutants: the scan limit was not pinned down. Tested
    /// with a 1 MiB limit (four reads of 256 KiB) to stay fast.
    #[tokio::test]
    async fn three_d_markers_are_searched_up_to_the_limit_only() {
        use std::io::{Seek, SeekFrom, Write};
        let dir = std::env::temp_dir().join(format!("teamcraft-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let at = |offset: u64, name: &str| {
            let path = dir.join(name);
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"%PDF-1.7\n").unwrap();
            f.seek(SeekFrom::Start(offset)).unwrap();
            f.write_all(b"/Subtype /U3D").unwrap();
            path
        };
        const MIB: u64 = 1024 * 1024;
        assert_eq!(PDF_3D_SCAN_LIMIT, 512 * MIB);
        let inside = at(MIB - 64, "inside.pdf");
        let past = at(MIB, "past-limit.pdf");
        assert!(pdf_has_3d_within(&inside, MIB).await, "within the limit");
        assert!(
            !pdf_has_3d_within(&past, MIB).await,
            "right after the limit: treated as a 2D document"
        );
        assert!(
            pdf_has_3d_within(&past, 2 * MIB).await,
            "found with a larger limit"
        );
        // the real limit: past the first megabytes is still searched
        assert!(pdf_has_3d(&at(2 * MIB, "two-mib.pdf")).await);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn creo_iterations() {
        assert_eq!(creo_iteration("bracket.prt.7"), Some(("bracket.prt", 7)));
        assert_eq!(creo_iteration("Frame.ASM.12"), Some(("Frame.ASM", 12)));
        assert_eq!(creo_iteration("sheet.drw.1"), Some(("sheet.drw", 1)));
        assert_eq!(creo_iteration("bracket.prt"), None);
        assert_eq!(creo_iteration("report.pdf.2"), None, "only Creo files");
        assert_eq!(creo_iteration("v1.2"), None);
        assert_eq!(creo_iteration(".prt.1"), None);
        assert_eq!(creo_iteration("bracket.prt.x"), None);
        assert_eq!(normalize_name("bracket.prt.7"), "bracket.prt");
        assert_eq!(normalize_name("bracket.stp"), "bracket.stp");
        assert_eq!(extension("Bracket.PRT.3").as_deref(), Some("prt"));
    }

    #[test]
    fn simulation_models_and_results() {
        let by_name = [
            ("mesh.vtk", "vtk-legacy"),
            ("field.VTU", "vtk-unstructured"),
            ("surface.vtp", "vtk-polydata"),
            ("grid.vts", "vtk-structured"),
            ("grid.vtr", "vtk-rectilinear"),
            ("run.pvd", "vtk-collection"),
            ("blocks.vtm", "vtk-multiblock"),
            ("job.frd", "calculix-frd"),
            ("model.op2", "nastran-op2"),
            ("model.f06", "nastran-f06"),
            ("model.bdf", "nastran-bulk"),
            ("model.nas", "nastran-bulk"),
            ("file.rst", "ansys-result"),
            ("file.rth", "ansys-thermal-result"),
            ("file.db", "ansys-database"),
            ("Job-1.odb", "abaqus-odb"),
            ("Job-1.sta", "abaqus-status"),
            ("Job-1.msg", "abaqus-message"),
            ("flow.cgns", "cgns"),
            ("mesh.exo", "exodus"),
            ("mesh.e", "exodus"),
            ("pipe.cas", "fluent-case"),
            ("pipe.cas.h5", "fluent-case-h5"),
            ("pipe-1-00100.DAT.H5", "fluent-data-h5"),
            ("bracket.sim", "nx-simulation"),
            ("bracket_fem1.fem", "nx-simulation"),
            ("crash.k", "lsdyna-keyword"),
            ("crash.d3plot", "lsdyna-d3plot"),
            ("d3plot", "lsdyna-d3plot"),
            ("d3plot07", "lsdyna-d3plot"),
            ("controlDict", "openfoam-case"),
            ("system/controlDict", "openfoam-case"),
            ("pitzDaily.foam", "openfoam-case"),
        ];
        for (name, expected) in by_name {
            assert_eq!(id(name, &[]), Some(expected), "{name}");
        }
        for (expected, kind) in [
            ("vtk-unstructured", FormatKind::Result),
            ("calculix-frd", FormatKind::Result),
            ("nastran-bulk", FormatKind::Simulation),
            ("fluent-data", FormatKind::Result),
            ("openfoam-case", FormatKind::Simulation),
        ] {
            assert_eq!(by_id(expected).unwrap().kind, kind, "{expected}");
        }
        // decided by content
        assert_eq!(
            id("pipe.sim", b"\x00\x01STAR-CCM+ 2402 simulation"),
            Some("starccm-sim")
        );
        assert_eq!(id("pipe.sim", b"\x00\x01\x02NX"), Some("nx-simulation"));
        assert_eq!(id("pipe.dat", &[]), None, ".dat alone is not enough");
        assert_eq!(
            id("pipe.dat", b"(0 \"fluent data\")\n"),
            Some("fluent-data")
        );
        assert_eq!(
            id("pipe.dat", b"  (1 \"ANSYS Fluent 2024\")"),
            Some("fluent-data")
        );
        assert_eq!(id("values.dat", b"1 2 3\n4 5 6\n"), None);
        assert_eq!(id("mesh.e", b"CDF\x02\x00\x00\x00\x01"), Some("exodus"));
        assert_eq!(id("mesh.e", b"\x89HDF\r\n\x1a\n\x00"), Some("exodus"));
        assert_eq!(id("notes.e", b"plain text"), None);
        assert_eq!(id("app.db", b"SQLite format 3\x00"), None);
        assert_eq!(id("Thumbs.db", &[]), None);
        assert_eq!(id("cache.db", &OLE_MAGIC), None);
        assert_eq!(
            id("file.db", b"\x00\x00\x00\x64ANSYS"),
            Some("ansys-database")
        );
        assert_eq!(
            id("README.rst", b"Title\n=====\n\nText"),
            None,
            "reStructuredText"
        );
        assert_eq!(
            id("file.rst", b"\x00\x00\x00\x64\x00\x00"),
            Some("ansys-result")
        );
        assert_eq!(id("mail.msg", &OLE_MAGIC), None, "Outlook message");
        assert_eq!(
            id("Job-1.msg", b" Abaqus 2024\n STEP 1\n"),
            Some("abaqus-message")
        );
        assert_eq!(id("font.bdf", b"STARTFONT 2.1\n"), None);
        assert_eq!(id("model.bdf", b"SOL 101\nCEND\n"), Some("nastran-bulk"));
        assert_eq!(id("base.odb", b"PK\x03\x04"), None, "OpenDocument database");
        assert_eq!(id("crash.k", b"*KEYWORD\n*NODE\n"), Some("lsdyna-keyword"));
        assert_eq!(id("crash.k", b"\x00\x01\x02\x03"), None);
        assert_eq!(id("d3plotter.txt", &[]), Some("text"), "not d3plot");
        assert_eq!(id(".cas.h5", &[]), None, "no stem");
        // the search knows the solvers
        assert!(!ids_named_by("simulation").is_empty());
        assert!(ids_named_by("OpenFOAM").contains(&"openfoam-case".to_string()));
        assert!(ids_named_by("vtk").contains(&"vtk-unstructured".to_string()));
        assert!(ids_named_by("Nastran").contains(&"nastran-op2".to_string()));
    }

    #[test]
    fn registry_is_consistent() {
        let mut ids = HashSet::new();
        let mut owners: HashMap<&str, Vec<&str>> = HashMap::new();
        for f in FORMATS {
            assert!(ids.insert(f.id), "duplicate format id {}", f.id);
            assert!(system(f.system).is_some(), "{}: unknown system", f.id);
            assert!(!f.extensions.is_empty(), "{}", f.id);
            for ext in f.extensions {
                assert_eq!(*ext, ext.to_ascii_lowercase(), "{}: lower case", f.id);
                assert!(!ext.starts_with('.'), "{}: without dot", f.id);
                owners.entry(ext).or_default().push(f.id);
            }
            assert!(
                f.mime.is_empty() || (f.mime.contains('/') && !f.mime.contains(' ')),
                "{}: content type",
                f.id
            );
            assert!(by_id(f.id).is_some_and(|g| g.label == f.label));
        }
        for (ext, formats) in owners {
            if AMBIGUOUS.contains(&ext) {
                // two owners, or one that needs the content (.java, .out)
                assert!((1..=2).contains(&formats.len()), "{ext}: {formats:?}");
            } else {
                assert_eq!(formats.len(), 1, "extension {ext} belongs to {formats:?}");
            }
        }
        for s in SYSTEMS {
            assert!(
                FORMATS.iter().any(|f| f.system == s.id),
                "system {} has no formats",
                s.id
            );
        }
    }

    #[test]
    fn the_programs_the_users_asked_for_are_covered() {
        for system_id in [
            "solidworks",
            "nx",
            "teamcenter",
            "inventor",
            "fusion",
            "creo",
            "catia",
            "solid-edge",
            "autocad",
            "exchange",
        ] {
            assert!(system(system_id).is_some(), "{system_id}");
        }
        for ext in [
            "sldprt",
            "sldasm",
            "slddrw",
            "prt",
            "ipt",
            "iam",
            "idw",
            "dwg",
            "f3d",
            "f3z",
            "asm",
            "drw",
            "catpart",
            "catproduct",
            "catdrawing",
            "par",
            "dft",
            "dxf",
            "stp",
            "step",
            "igs",
            "iges",
            "x_t",
            "x_b",
            "jt",
            "sat",
            "sab",
            "stl",
            "psm",
            "scdoc",
            "rvt",
            "rfa",
            "skp",
            "dgn",
            "ifc",
            "nwd",
            "nwc",
            "nwf",
            "pln",
            "pla",
            "3dm",
            "wire",
            "blend",
            "ma",
            "mb",
            "max",
            "3ds",
            "c4d",
            "lxo",
            "hip",
            "hipnc",
            "pdf",
            "obj",
            "fbx",
            "gltf",
            "glb",
            "3mf",
            "ply",
            "dae",
            "usd",
            "usdz",
            "amf",
            "wrl",
            "x3d",
            "e57",
            "pts",
            "xyz",
            "las",
            "laz",
            "abc",
        ] {
            assert!(
                detect(&format!("file.{ext}"), &[]).is_some()
                    || detect(&format!("file.{ext}"), b"#UGC:2").is_some(),
                ".{ext}"
            );
        }
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type(by_id("step"), OCTET), "model/step");
        assert_eq!(
            content_type(by_id("solidworks-part"), "application/x-whatever"),
            OCTET
        );
        assert_eq!(
            content_type(by_id("word"), "application/msword"),
            "application/msword",
            "documents keep the client's type"
        );
        assert_eq!(content_type(None, "text/plain"), "text/plain");
    }

    #[test]
    fn search_names() {
        let sw = ids_named_by("SolidWorks");
        assert!(sw.contains(&"solidworks-part".to_string()) && sw.len() == 5);
        assert!(ids_named_by("nx").contains(&"nx-part".to_string()));
        assert!(!ids_named_by("nx").contains(&"creo-part".to_string()));
        assert!(ids_named_by("Pro/E").contains(&"creo-part".to_string()));
        assert!(ids_named_by("solid edge").contains(&"solidedge-drawing".to_string()));
        assert!(ids_named_by("Fusion 360").contains(&"fusion-design".to_string()));
        assert_eq!(ids_named_by("CATIA product"), vec!["catia-product"]);
        assert_eq!(ids_named_by("jt"), vec!["jt"]);
        assert!(ids_named_by("step").contains(&"step".to_string()));
        assert!(ids_named_by("bracket").is_empty());
        assert!(ids_named_by("x").is_empty());
        // by the full program name only: neither id, short name nor alias
        // matches "Autodesk Inventor" (found by cargo mutants)
        assert!(ids_named_by("Autodesk Inventor").contains(&"inventor-part".to_string()));
        assert_eq!(ids_of_system("inventor").len(), 5);
    }

    #[test]
    fn extensions_are_case_insensitive() {
        for name in ["Bracket.SLDPRT", "bracket.sldprt", "BRACKET.SldPrt"] {
            assert_eq!(id(name, &[]), Some("solidworks-part"), "{name}");
        }
        assert_eq!(id("Top.CATProduct", &[]), Some("catia-product"));
        assert_eq!(id("TOP.CATPRODUCT", &[]), Some("catia-product"));
        assert_eq!(extension("A.STEP").as_deref(), Some("step"));
        assert_eq!(extension("noext"), None);
        assert_eq!(extension("trailing."), None);
        assert_eq!(extension(".hidden"), None);
    }

    #[test]
    fn name_only_detection_trusts_the_extension() {
        // Without content (`&[]`) the extension decides even for the
        // extensions that need content otherwise.
        assert_eq!(id("Job-1.msg", &[]), Some("abaqus-message"));
        assert_eq!(id("Job-1.sta", &[]), Some("abaqus-status"));
        assert_eq!(id("crash.k", &[]), Some("lsdyna-keyword"));
        assert_eq!(id("mesh.e", &[]), Some("exodus"));
        assert_eq!(id("hull.obj", &[]), Some("obj"));
        assert_eq!(id("design.sc", &[]), Some("solidcraft-design"));
        // ... but .dat never counts without Fluent sections.
        assert_eq!(id("pipe.dat", &[]), None);
    }

    #[test]
    fn whole_file_names_and_paths() {
        assert_eq!(id("case\\system\\controlDict", &[]), Some("openfoam-case"));
        assert_eq!(
            id("controldict", &[]),
            None,
            "OpenFOAM spells it controlDict"
        );
        assert_eq!(id("run/D3PLOT12", &[]), Some("lsdyna-d3plot"));
        assert_eq!(id("d3plot_a", &[]), None);
        assert_eq!(id("d3plot01.bak", &[]), None);
        // The STAR-CCM+ marker is matched case-insensitively.
        assert_eq!(id("x.sim", b"..starccm+.."), Some("starccm-sim"));
        // Fluent sections after leading white space; (4 …) too.
        assert_eq!(id("x.dat", b"\n\t(4 (\"rp\"))"), Some("fluent-data"));
        assert_eq!(id("x.dat", b"(2 3)"), None);
        assert_eq!(id("x.e", b"CDF\x03"), None, "unknown netCDF version");
    }

    /// The research page `docs/src/research/solver-files.md`: synthetic
    /// heads for every rule (no real files of these programs were at hand).
    #[test]
    fn simcenter_3d_files() {
        let mut nx = crate::nxpart::MAGIC.to_vec();
        nx.extend_from_slice(&[0, 0, 0, 1, 0xFF, 0x00]);
        // the SPLMSSTR container is a positive NX test for .sim/.fem/.afm
        for name in ["bracket_sim1.sim", "bracket_fem1.fem", "frame.afm"] {
            assert_eq!(id(name, &nx), Some("nx-simulation"), "{name}");
            assert_eq!(id(name, &[]), Some("nx-simulation"), "{name} by name");
        }
        // even when the container happens to contain the STAR-CCM+ name
        let mut nx_star = nx.clone();
        nx_star.extend_from_slice(b"STAR-CCM+ import");
        assert_eq!(id("a.sim", &nx_star), Some("nx-simulation"));
        // a .sim with neither marker keeps the NX fallback
        assert_eq!(
            id("old.sim", b"\x00\x00\x00\x07binary"),
            Some("nx-simulation")
        );
        // .fem: OptiStruct/HyperMesh text deck, other binary files unknown
        assert_eq!(
            id(
                "bracket.fem",
                b"$$ HyperMesh OptiStruct deck\nBEGIN BULK\nGRID,1,,0.,0.,0.\n"
            ),
            Some("optistruct-deck")
        );
        assert_eq!(id("bracket.fem", b"\x00\x01\x02\x03\x04"), None);
        // .afm: Adobe font metrics are text
        assert_eq!(
            id(
                "Helvetica.afm",
                b"StartFontMetrics 4.1\nFontName Helvetica\n"
            ),
            None
        );
        // Nastran decks written by Simcenter 3D as .dat
        for deck in [
            &b"$ Simcenter Nastran input deck\nSOL 101\nCEND\n"[..],
            b"SOL 101\nCEND\nBEGIN BULK\n",
            b"  sol 103\n",
            b"INIT MASTER(S)\nID bracket\n",
            b"TITLE = x\r\nBEGIN BULK\r\nGRID,1\r\n",
            b"$ NASTRAN deck\nGRID,1,,0.,0.,0.\n",
        ] {
            assert_eq!(
                id("bracket_sim1-solution_1.dat", deck),
                Some("nastran-bulk")
            );
        }
        assert_eq!(id("values.dat", b"SOLAR 1 2 3\nCENDRA\n"), None);
        assert_eq!(id("values.dat", b"$ just a comment\n1 2 3\n"), None);
        assert_eq!(id("blob.dat", b"\x00SOL 101\nCEND\n"), None, "binary");
        assert_eq!(
            id("pipe.dat", b"(0 \"fluent\")\nSOL 101\n"),
            Some("fluent-data")
        );
        // the rest of the Nastran set
        assert_eq!(
            id("run.f04", b"  NASTRAN EXECUTIVE SUMMARY\n"),
            Some("nastran-f04")
        );
        assert_eq!(id("run.f04", &[]), Some("nastran-f04"));
        assert_eq!(id("run.f04", b"\x00\x01\x02\x03"), None);
        assert_eq!(id("run.pch", b"$TITLE   = x\n"), Some("nastran-punch"));
        assert_eq!(id("run.pch", b"\x00\x01\x02\x03"), None);
        assert_eq!(
            id(
                "run.op2",
                b"\x00\x00\x00\x1cNASTRAN FORT TAPE ID CODE - \x00"
            ),
            Some("nastran-op2")
        );
        assert_eq!(id("run.op2", &[]), Some("nastran-op2"));
        assert_eq!(id("notes.op2", b"plain text notes\n"), None);
    }

    #[test]
    fn fluent_files() {
        let mut hdf5 = HDF5_SIGNATURE.to_vec();
        hdf5.extend_from_slice(&[0; 16]);
        let mut user_block = vec![0u8; 512];
        user_block.extend_from_slice(HDF5_SIGNATURE);
        let gz = [0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0];
        for (name, expected) in [
            ("pipe.cas.h5", "fluent-case-h5"),
            ("pipe-1-00100.dat.h5", "fluent-data-h5"),
            ("pipe.MSH.H5", "fluent-mesh-h5"),
        ] {
            assert_eq!(id(name, &hdf5), Some(expected), "{name}");
            assert_eq!(id(name, &user_block), Some(expected), "{name}, user block");
            assert_eq!(id(name, &[]), Some(expected), "{name} by name");
            assert_eq!(
                id(name, b"not hdf5 at all"),
                None,
                "{name} without signature"
            );
            assert_eq!(id(name, &gz), None, "{name} gzip");
        }
        for (name, expected) in [
            ("pipe.cas.gz", "fluent-case-gz"),
            ("pipe.dat.gz", "fluent-data-gz"),
            ("pipe.msh.gz", "fluent-mesh-gz"),
        ] {
            assert_eq!(id(name, &gz), Some(expected), "{name}");
            assert_eq!(id(name, &[]), Some(expected), "{name} by name");
            assert_eq!(id(name, &hdf5), None, "{name} without gzip magic");
        }
        assert_eq!(id(".msh.h5", &[]), None, "no stem");
        assert_eq!(id("pipe.xcas.h5", &[]), None, "not .cas.h5");
        assert_eq!(id("archive.gz", &gz), None);
        // .msh: Fluent sections or Gmsh
        assert_eq!(
            id("pipe.msh", b"(0 \"Grid:\")\n(2 3)\n"),
            Some("fluent-mesh")
        );
        assert_eq!(
            id("pipe.msh", b"(2 3)\n(10 (0 1 2c8 0))\n"),
            Some("fluent-mesh")
        );
        assert_eq!(id("pipe.msh", b"\n(10 (0 1 2c8 0))\n"), Some("fluent-mesh"));
        assert_eq!(
            id("pipe.msh", b"$MeshFormat\n4.1 0 8\n$EndMeshFormat\n"),
            Some("gmsh")
        );
        assert_eq!(id("pipe.msh", b"random words\n"), None);
        assert_eq!(id("pipe.msh", &[]), None, "the name alone is ambiguous");
        // project, CFD-Post data, journal, transcript
        assert_eq!(id("pipe.flprj", &[]), Some("fluent-project"));
        assert_eq!(id("pipe.cdat", &[]), Some("fluent-cdat"));
        assert_eq!(
            id(
                "run.jou",
                b"/file/read-case \"pipe.cas.h5\"\n/solve/iterate 500\n"
            ),
            Some("fluent-journal")
        );
        assert_eq!(id("run.jou", b"\x00\x01\x02\x03"), None);
        assert_eq!(
            id("run.trn", b"Welcome to ANSYS Fluent 2024 R2\n"),
            Some("fluent-transcript")
        );
        assert_eq!(id("run.trn", b"\x00\x01\x02\x03"), None);
        // report files
        let report =
            b"\"report-def-0-rfile\"\n\"Iteration\" \"report-def-0\" \"flow-time\"\n1 0.5 0.001\n";
        assert_eq!(id("report-file-0.out", report), Some("fluent-report"));
        assert_eq!(
            id("monitor.out", report),
            Some("fluent-report"),
            "by content"
        );
        let repeated = b"\"title\"\r\n(\"Time Step\" \"drag\")\r\n1 2\r\n";
        assert_eq!(id("x.out", repeated), Some("fluent-report"));
        assert_eq!(id("report-file-0.out", &[]), Some("fluent-report"));
        assert_eq!(id("a.out", &[]), None);
        assert_eq!(id("a.out", b"\x7fELF\x02\x01\x01"), None, "an executable");
        assert_eq!(id("a.out", b"\"title\"\n1 2 3\n"), None, "no header");
        assert_eq!(id("a.out", b"\"a\" \"b\"\n\"c\" \"d\"\n"), None, "no title");
        assert_eq!(id("a.out", b"\"unclosed\n\"a\" \"b\n"), None);
    }

    #[test]
    fn star_ccm_files() {
        assert_eq!(id("pipe.simh", &[]), Some("starccm-history"));
        let recorded = b"// Simcenter STAR-CCM+ macro: run.java\n// Written by Simcenter STAR-CCM+ 18.06.006\npackage macro;\n";
        assert_eq!(id("run.java", recorded), Some("starccm-macro"));
        assert_eq!(
            id("Export.java", b"package macro;\n\nimport star.common.*;\n"),
            Some("starccm-macro")
        );
        assert_eq!(
            id(
                "Main.java",
                b"package com.example;\n\npublic class Main {}\n"
            ),
            None,
            "ordinary Java"
        );
        assert_eq!(id("Main.java", &[]), None, "the name alone is not enough");
        assert_eq!(
            id("pipe.sim", b"\x00\x01Simcenter STAR-CCM+ 2402"),
            Some("starccm-sim")
        );
    }

    #[test]
    fn solidcraft_json_needs_its_format_marker() {
        assert_eq!(
            id("a.sc", b"  {\"format\": \"solvecraft/2\"}"),
            Some("solidcraft-design")
        );
        assert_eq!(id("a.sc", b"{\"format\": \"other/1\"}"), None);
        assert_eq!(id("a.sc", b"[\"solidcraft/1\"]"), None, "not an object");
    }

    #[test]
    fn creo_iteration_limits() {
        assert_eq!(creo_iteration("a.prt.123456"), Some(("a.prt", 123_456)));
        assert_eq!(creo_iteration("a.prt.1234567"), None, "at most six digits");
        assert_eq!(creo_iteration("a.prt."), None);
        assert_eq!(creo_iteration("a.mfg.3"), Some(("a.mfg", 3)));
        assert_eq!(creo_iteration("a.MFG.3"), Some(("a.MFG", 3)));
        // Iterated names keep their Creo format, even for .prt/.asm.
        assert_eq!(id("plate.prt.12", b"\x00binary"), Some("creo-part"));
        assert_eq!(id("lay.lay.2", &[]), Some("creo-layout"));
    }

    #[test]
    fn kinds_and_labels() {
        let names: HashSet<&str> = FormatKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(names.len(), FormatKind::ALL.len(), "unique ids");
        let labels: HashSet<&str> = FormatKind::ALL.iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), FormatKind::ALL.len(), "unique labels");
        for f in FORMATS {
            assert!(FormatKind::ALL.contains(&f.kind));
        }
        // serde uses the same ids as as_str
        for k in FormatKind::ALL {
            assert_eq!(
                serde_json::to_value(k).unwrap(),
                serde_json::Value::String(k.as_str().into())
            );
        }
    }

    #[test]
    fn content_type_falls_back_to_the_client() {
        // Office documents have no registered type: the client's is kept.
        for id in ["word", "spreadsheet", "presentation"] {
            assert_eq!(content_type(by_id(id), "x/y"), "x/y", "{id}");
        }
        assert_eq!(content_type(by_id("nastran-bulk"), OCTET), "text/plain");
    }

    #[test]
    fn search_ignores_case_spaces_and_punctuation() {
        assert_eq!(ids_named_by("SOLID WORKS"), ids_named_by("solidworks"));
        assert_eq!(ids_named_by("Solid-Edge"), ids_named_by("solid edge"));
        assert!(ids_named_by("  ").is_empty());
        let mut ids = ids_of_system("solidworks");
        ids.sort();
        let mut named = ids_named_by("SolidWorks");
        named.sort();
        assert_eq!(ids, named);
        assert!(ids_of_system("nope").is_empty());
    }

    #[test]
    fn infos() {
        let product = info("catia-product").unwrap();
        assert_eq!(product.system_short, "CATIA");
        assert_eq!(product.kind, FormatKind::Assembly);
        assert_eq!(product.preview, None);
        assert_eq!(ids_named_by("parasolid").len(), 2);
        assert_eq!(ids_named_by("NX part"), vec!["nx-part"]);
        assert_eq!(info("step").unwrap().preview, Some("step"));
        assert_eq!(info("3mf").unwrap().preview, Some("3mf"));
        assert_eq!(info("las").unwrap().preview, Some("las"));
        assert_eq!(
            info("brep").unwrap().preview,
            None,
            "no pure-Rust BREP reader"
        );
        assert_eq!(info("iges").unwrap().preview, None);
        assert_eq!(ids_named_by("revit").len(), 3);
        assert!(ids_named_by("point clouds").contains(&"e57".to_string()));
        assert_eq!(
            ids_named_by("FieldCraft"),
            vec!["solidcraft-design", "fieldcraft-model"]
        );
        assert!(info("").is_none() && info("nope").is_none());
        let catalogue = catalogue();
        assert_eq!(catalogue.len(), SYSTEMS.len());
        assert!(catalogue
            .iter()
            .all(|s| !s["formats"].as_array().unwrap().is_empty()));
        // each system lists exactly its own formats (found by cargo mutants)
        let listed: usize = catalogue
            .iter()
            .map(|s| s["formats"].as_array().unwrap().len())
            .sum();
        assert_eq!(listed, FORMATS.len(), "every format once");
        for s in &catalogue {
            for f in s["formats"].as_array().unwrap() {
                let id = f["id"].as_str().unwrap();
                assert_eq!(by_id(id).unwrap().system, s["id"], "{id}");
            }
        }
    }

    /// Found by cargo mutants: a line without tokens is not a token line.
    #[test]
    fn quoted_tokens_counts_quoted_strings_only() {
        assert_eq!(quoted_tokens(br#""Iteration" "report-def-0""#), Some(2));
        assert_eq!(quoted_tokens(br#"  ("Flow Time")  "#), Some(1));
        assert_eq!(quoted_tokens(b"()"), None, "nothing inside the parentheses");
        assert_eq!(quoted_tokens(b"   "), None);
        assert_eq!(quoted_tokens(br#""open"#), None, "unterminated");
        assert_eq!(quoted_tokens(br#""a" b"#), None, "unquoted token");
        assert_eq!(quoted_tokens(br#"("a""#), None, "unclosed parenthesis");
    }

    /// Found by cargo mutants: the 95 % threshold was not pinned down.
    #[test]
    fn text_needs_95_percent_printable_bytes_and_no_nul() {
        let with_controls = |n: usize| {
            let mut head = vec![b'a'; 20 - n];
            head.extend(std::iter::repeat_n(0x01, n));
            head
        };
        assert!(looks_textual(b"GRID    1       0.0     1.0\n"));
        assert!(
            looks_textual("Ümlaut-Kommentar\n".as_bytes()),
            "non-ASCII counts as text"
        );
        assert!(looks_textual(&with_controls(1)), "19 of 20 = 95 %");
        assert!(!looks_textual(&with_controls(2)), "18 of 20 = 90 %");
        assert!(!looks_textual(&[0x01; 20]));
        assert!(!looks_textual(b"text\0with a NUL"));
        assert!(!looks_textual(b""));
    }
}
