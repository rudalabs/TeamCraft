//! STEP export of native CAD files through the CAD program installed on this
//! computer – the way the Inventor add-in of Autodesk Vault or the Solid Edge
//! integration of Teamcenter create viewables at check-in (findings in
//! `docs/src/research/cad-previews.md`). The server then shows the STEP in 3D.
//!
//! Windows only: Autodesk Inventor and Siemens Solid Edge are driven through
//! their documented COM interfaces by a short Windows PowerShell script
//! (`Documents.Open`, `SaveAs` with a `.stp` name). TeamCraft contains no
//! Autodesk or Siemens component – the user's own licence (student licences
//! included, within their terms) does the export. A document the user already
//! has open is exported as it is and left open. Not tested against real
//! Inventor or Solid Edge installations in the development environment.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Serialize;

/// Longest time an export may take (large assemblies).
#[cfg_attr(not(windows), allow(dead_code))]
pub const TIME_LIMIT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CadProgram {
    Inventor,
    SolidEdge,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl CadProgram {
    pub const ALL: [CadProgram; 2] = [CadProgram::Inventor, CadProgram::SolidEdge];

    /// COM class of the application.
    pub fn prog_id(self) -> &'static str {
        match self {
            CadProgram::Inventor => "Inventor.Application",
            CadProgram::SolidEdge => "SolidEdge.Application",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CadProgram::Inventor => "Autodesk Inventor",
            CadProgram::SolidEdge => "Siemens Solid Edge",
        }
    }

    fn script(self) -> &'static str {
        match self {
            CadProgram::Inventor => INVENTOR_SCRIPT,
            CadProgram::SolidEdge => SOLID_EDGE_SCRIPT,
        }
    }
}

const OLE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

fn is_ole(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head == OLE_MAGIC
}

/// The program that exports a file: Inventor parts and assemblies, Solid
/// Edge parts, sheet metal, weldments and assemblies (`.asm` only when it is
/// an OLE file – Creo assemblies use the same extension).
pub fn program_for(path: &Path) -> Option<CadProgram> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "ipt" | "iam" => Some(CadProgram::Inventor),
        "par" | "psm" | "pwd" => Some(CadProgram::SolidEdge),
        "asm" if is_ole(path) => Some(CadProgram::SolidEdge),
        _ => None,
    }
}

/// `bracket.ipt` → `bracket.stp` in the same folder.
pub fn step_path(input: &Path) -> PathBuf {
    input.with_extension("stp")
}

/// Opens the document invisibly (or uses it when it is already open), saves
/// a copy as STEP – Inventor picks the STEP translator by the extension.
const INVENTOR_SCRIPT: &str = r#"param([Parameter(Mandatory = $true)][string]$In, [Parameter(Mandatory = $true)][string]$Out)
$ErrorActionPreference = 'Stop'
$started = $false
try { $app = [Runtime.InteropServices.Marshal]::GetActiveObject('Inventor.Application') }
catch { $app = New-Object -ComObject 'Inventor.Application'; $started = $true }
$silent = $app.SilentOperation
$app.SilentOperation = $true
try {
    $doc = $null; $opened = $false
    foreach ($d in $app.Documents) { if ($d.FullFileName -ieq $In) { $doc = $d } }
    if ($null -eq $doc) { $doc = $app.Documents.Open($In, $false); $opened = $true }
    try { $doc.SaveAs($Out, $true) } finally { if ($opened) { $doc.Close($true) } }
} finally {
    $app.SilentOperation = $silent
    if ($started) { $app.Quit() }
}
"#;

/// The same with Solid Edge: `SaveAs` with a `.stp` name translates, like
/// *Save As → STEP* in the program.
const SOLID_EDGE_SCRIPT: &str = r#"param([Parameter(Mandatory = $true)][string]$In, [Parameter(Mandatory = $true)][string]$Out)
$ErrorActionPreference = 'Stop'
$started = $false
try { $app = [Runtime.InteropServices.Marshal]::GetActiveObject('SolidEdge.Application') }
catch { $app = New-Object -ComObject 'SolidEdge.Application'; $started = $true }
$alerts = $app.DisplayAlerts
$app.DisplayAlerts = $false
try {
    $doc = $null; $opened = $false
    foreach ($d in $app.Documents) { if ($d.FullName -ieq $In) { $doc = $d } }
    if ($null -eq $doc) { $doc = $app.Documents.Open($In); $opened = $true }
    try { $doc.SaveAs($Out) } finally { if ($opened) { $doc.Close($false) } }
} finally {
    $app.DisplayAlerts = $alerts
    if ($started) { $app.Quit() }
}
"#;

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{os::windows::process::CommandExt, process::Command};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Whether the COM class of a program is registered (it is installed).
    pub fn registered(program: CadProgram) -> bool {
        Command::new("reg")
            .args(["query", &format!("HKCR\\{}\\CLSID", program.prog_id())])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    pub fn run(program: CadProgram, input: &Path, output: &Path) -> Result<(), String> {
        let script = std::env::temp_dir().join(format!(
            "teamcraft-step-export-{}-{}.ps1",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::write(&script, program.script())
            .map_err(|e| format!("Cannot write the export script: {e}"))?;
        let result = wait(program, &script, input, output);
        let _ = std::fs::remove_file(&script);
        result
    }

    fn wait(program: CadProgram, script: &Path, input: &Path, output: &Path) -> Result<(), String> {
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script)
            .arg("-In")
            .arg(input)
            .arg("-Out")
            .arg(output)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Cannot start Windows PowerShell: {e}"))?;
        // read the error output on the side, so a full pipe never blocks
        let reader = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                text
            })
        });
        let started = std::time::Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > TIME_LIMIT => {
                    let _ = child.kill();
                    return Err(format!(
                        "{} did not finish the export within {} minutes.",
                        program.label(),
                        TIME_LIMIT.as_secs() / 60
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(250)),
                Err(e) => return Err(format!("Export failed: {e}")),
            }
        };
        if status.success() {
            return Ok(());
        }
        let stderr = reader.and_then(|r| r.join().ok()).unwrap_or_default();
        let reason = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("no details");
        Err(format!(
            "{} could not export the file: {reason}",
            program.label()
        ))
    }
}

/// The CAD programs installed on this computer that can export STEP.
pub fn available() -> Vec<CadProgram> {
    #[cfg(windows)]
    {
        CadProgram::ALL
            .into_iter()
            .filter(|p| platform::registered(*p))
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Exports `input` to a STEP file next to it with the installed CAD program;
/// returns the path of the STEP file.
pub fn export_step(input: &Path) -> Result<PathBuf, String> {
    let program = program_for(input).ok_or_else(|| {
        format!(
            "{} is not an Inventor or Solid Edge part or assembly.",
            input.display()
        )
    })?;
    let output = step_path(input);
    #[cfg(windows)]
    {
        platform::run(program, input, &output)?;
        if output.is_file() {
            Ok(output)
        } else {
            Err(format!(
                "{} finished, but {} was not written.",
                program.label(),
                output.display()
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = output;
        Err(format!(
            "The STEP export through {} works on Windows only.",
            program.label()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_and_names() {
        let dir = std::env::temp_dir().join(format!("teamcraft-cadexport-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let se_asm = dir.join("frame.asm");
        let mut ole = OLE_MAGIC.to_vec();
        ole.extend_from_slice(&[0; 504]);
        std::fs::write(&se_asm, &ole).unwrap();
        let creo_asm = dir.join("gearbox.asm");
        std::fs::write(&creo_asm, b"#UGC:2 ASSEMBLY").unwrap();
        assert_eq!(
            program_for(Path::new("/w/Bracket.IPT")),
            Some(CadProgram::Inventor)
        );
        assert_eq!(
            program_for(Path::new("/w/rover.iam")),
            Some(CadProgram::Inventor)
        );
        assert_eq!(
            program_for(Path::new("/w/plate.psm")),
            Some(CadProgram::SolidEdge)
        );
        assert_eq!(program_for(&se_asm), Some(CadProgram::SolidEdge));
        assert_eq!(program_for(&creo_asm), None, "Creo assembly");
        assert_eq!(program_for(Path::new("/w/hub.step")), None);
        assert_eq!(
            step_path(Path::new("/w/Bracket.ipt")),
            PathBuf::from("/w/Bracket.stp")
        );
        let _ = std::fs::remove_dir_all(&dir);
        // the scripts never close a document the user had open
        for script in [INVENTOR_SCRIPT, SOLID_EDGE_SCRIPT] {
            assert!(script.contains("if ($opened)") && script.contains("if ($started)"));
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_explain_the_limit() {
        assert!(available().is_empty());
        let err = export_step(Path::new("/w/Bracket.ipt")).unwrap_err();
        assert!(err.contains("Windows only"), "{err}");
    }
}
