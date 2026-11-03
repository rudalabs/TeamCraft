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
//!
//! Native formats are proprietary; TeamCraft identifies and versions them,
//! scans the ones with readable references (see `cadrefs`) and previews the
//! open formats in 3D (see `preview`). Onshape keeps its documents in the
//! cloud – store their STEP/Parasolid exports.
//!
//! Extensions are matched case-insensitively (`.CATPart`, `.SLDPRT`).

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
    /// Simulation and analysis models.
    Simulation,
    /// Manufacturing data (CAM, G-code).
    Manufacturing,
    /// Documents and images.
    Document,
    /// Templates, libraries, project files.
    Other,
}

impl FormatKind {
    pub const ALL: [FormatKind; 12] = [
        FormatKind::Part,
        FormatKind::Assembly,
        FormatKind::Drawing,
        FormatKind::Model,
        FormatKind::Exchange,
        FormatKind::Mesh,
        FormatKind::Visualization,
        FormatKind::Electronics,
        FormatKind::Simulation,
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
    system!("cae", "Simulation (ANSYS, Abaqus, COMSOL)", "Simulation", "various", "ansys", "abaqus", "comsol"),
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
pub const AMBIGUOUS: [&str; 3] = ["prt", "asm", "pdf"];

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
    let iterated = creo_iteration(name).is_some();
    let ext = extension(name)?;
    match ext.as_str() {
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

/// JSON design of SolidCraft (`"format": "solidcraft/N"`).
fn is_solidcraft_json(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    text.trim_start().starts_with('{')
        && (text.contains("\"solidcraft/") || text.contains("\"solvecraft/"))
}

/// Whether a PDF embeds a 3D model: the 3D stream dictionaries (`/Subtype
/// /U3D` or `/PRC`) are never compressed, so a byte search finds them.
async fn pdf_has_3d(path: &std::path::Path) -> bool {
    use tokio::io::AsyncReadExt;
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let needles: [&[u8]; 2] = [b"/U3D", b"/PRC"];
    let mut buf = vec![0u8; 256 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    let mut scanned = 0u64;
    // 3D PDFs are rarely larger; bigger documents are treated as 2D
    while scanned < 512 * 1024 * 1024 {
        let n = match file.read(&mut buf).await {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        scanned += n as u64;
        carry.extend_from_slice(&buf[..n]);
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
                        "searchable_text": crate::textindex::indexes(&format!("x.{}", f.extensions[0])),
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
        assert_eq!(id("startup.asm", b"  .section .text\n  mov r0, #1\n"), None);
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
                assert_eq!(formats.len(), 2, "{ext}");
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
        assert_eq!(ids_of_system("inventor").len(), 5);
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
    }
}
