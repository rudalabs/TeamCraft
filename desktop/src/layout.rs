//! The layout of the working folder – pure rules, no network, no Tauri.
//!
//! One folder per project group that mirrors the folder structure the files
//! have on the server (`files.folder`), one copy of each file; the revision
//! is metadata, not a folder level:
//!
//! ```text
//! <workspace>/ROVER/chassis/chassis.asm
//! <workspace>/ROVER/chassis/brackets/bracket.prt
//! <workspace>/.teamcraft/state.json     what every local file is (item, revision, version, sha256, …)
//! <workspace>/.teamcraft/migration.log  what the move from the old layout did
//! ```
//!
//! CAD programs find the parts of an assembly by relative path or by name in
//! the folders next to it, so the tree has to look like the one the
//! assembly was built in. The older layout `<group>/<item>/<revision>/` (and
//! before it `<item>/<revision>/`) is detected and moved over by
//! [`plan_migration`], never overwriting anything.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

/// Hidden folder of the workspace for the app's own files.
pub const STATE_DIR: &str = ".teamcraft";
pub const STATE_FILE: &str = "state.json";
pub const MIGRATION_LOG: &str = "migration.log";
/// Hidden file of a revision folder of the old layout (see `workspace`).
pub const OLD_BASE_FILE: &str = ".teamcraft-base.json";

// ---------------------------------------------------------------------------
// Names and folders
// ---------------------------------------------------------------------------

/// A file or folder name that is valid on Windows, macOS and Linux.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Temporary, lock and backup files of CAD and office programs are never
/// uploaded: SolidWorks/Office `~$…` locks, AutoCAD `.dwl`/`.sv$`, KiCad
/// `-bak`/`_autosave-`/`.lck`, Creo `trail.txt`/`std.out`, FreeCAD `.FCBak`,
/// Eagle `.b#1`, Blender `.blend1`, `Thumbs.db`, …
pub fn is_ignored(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    name.starts_with('.')
        || name.starts_with("~$")
        || lower.ends_with('~')
        || lower.ends_with(".teamcraft-download")
        || lower.ends_with("-bak")
        || lower.starts_with("_autosave-")
        || lower.starts_with("trail.txt")
        || matches!(
            lower.as_str(),
            "std.out" | "current_session.pro" | "thumbs.db" | "desktop.ini" | "fp-info-cache"
        )
        || matches!(
            ext,
            "tmp" | "bak" | "lck" | "lock" | "dwl" | "dwl2" | "sv$" | "ac$" | "fcbak" | "fcstd1"
                | "blend1" | "blend2" | "kicad_prl"
        )
        // Eagle backups: board.b#1, schematic.s#2, library.l#3
        || (ext.len() == 3
            && matches!(ext.as_bytes()[0], b'b' | b's' | b'l')
            && ext.as_bytes()[1] == b'#'
            && ext.as_bytes()[2].is_ascii_digit())
}

/// Creo writes every save as `<name>.<ext>.<n>` (`bracket.prt.7`).
const CREO_ITERATED: [&str; 7] = ["prt", "asm", "drw", "frm", "lay", "sec", "mfg"];

/// The name a workspace file has on the server and its Creo iteration:
/// `bracket.prt.7` → `("bracket.prt", 7)`, `bracket.prt` → `("bracket.prt", 0)`.
/// Same rule as `cadformat::normalize_name` on the server.
pub fn logical_name(name: &str) -> (&str, u32) {
    if let Some((base, number)) = name.rsplit_once('.') {
        if !number.is_empty() && number.len() <= 6 && number.bytes().all(|b| b.is_ascii_digit()) {
            if let Some((stem, ext)) = base.rsplit_once('.') {
                if !stem.is_empty() && CREO_ITERATED.contains(&ext.to_ascii_lowercase().as_str()) {
                    return (base, number.parse().unwrap_or(0));
                }
            }
        }
    }
    (name, 0)
}

/// The name without its extension, lower case (`Bracket.SLDPRT` → `bracket`);
/// a STEP export `bracket.step` belongs to `bracket.prt`.
pub fn stem_of(name: &str) -> String {
    let (logical, _) = logical_name(name);
    logical
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|s| !s.is_empty())
        .unwrap_or(logical)
        .to_lowercase()
}

/// Normalises a folder inside a project group: the same rule as
/// `domain::normalize_folder` on the server (`\` → `/`, no `..`, no
/// leading or trailing `/`, names valid on Windows, macOS and Linux).
pub fn normalize_folder(raw: &str) -> Result<String, String> {
    let mut segments = Vec::new();
    for segment in raw.trim().split(['/', '\\']) {
        let segment = segment.trim_start();
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(format!("Folder '{raw}': '..' is not allowed."));
        }
        if segment
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
            || segment.ends_with(['.', ' '])
            || is_reserved(segment)
        {
            return Err(format!(
                "Folder '{raw}': '{segment}' is not a valid folder name."
            ));
        }
        if segment.chars().count() > 120 {
            return Err(format!(
                "Folder '{raw}': folder names must be at most 120 characters."
            ));
        }
        segments.push(segment);
    }
    let folder = segments.join("/");
    if segments.len() > 32 || folder.chars().count() > 400 {
        return Err(format!("Folder '{raw}' is too long or too deep."));
    }
    Ok(folder)
}

fn is_reserved(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

/// The folder sent with an upload for a local relative folder: normalised,
/// or – if a name is not valid everywhere – with every name made safe.
pub fn folder_for_upload(raw: &str) -> String {
    normalize_folder(raw).unwrap_or_else(|_| {
        let safe: Vec<String> = raw
            .split(['/', '\\'])
            .filter(|s| !s.trim().is_empty() && *s != "." && *s != "..")
            .map(|s| {
                let name = safe_name(s);
                if is_reserved(&name) {
                    format!("{name}_")
                } else {
                    name
                }
            })
            .collect();
        normalize_folder(&safe.join("/")).unwrap_or_default()
    })
}

/// The relative path (`/` separated) of a server file in the workspace:
/// `<group>/<folder>/<file>`; without a project group directly below the
/// workspace. Every name is made safe for the local file system.
pub fn local_rel(group: Option<&str>, folder: &str, filename: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(group) = group.map(str::trim).filter(|g| !g.is_empty()) {
        parts.push(safe_name(group));
    }
    let folder = normalize_folder(folder).unwrap_or_else(|_| folder_for_upload(folder));
    parts.extend(folder.split('/').filter(|s| !s.is_empty()).map(safe_name));
    parts.push(safe_name(filename));
    parts.join("/")
}

/// The local relative folder of a server folder (`<group>/<folder>`).
pub fn local_dir_rel(group: Option<&str>, folder: &str) -> String {
    let rel = local_rel(group, folder, "x");
    rel.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

/// The key of a relative path in the state: file systems on Windows and
/// macOS ignore case, so `Bracket.PRT` and `bracket.prt` are one file.
pub fn key_of(rel: &str) -> String {
    rel.replace('\\', "/").to_lowercase()
}

/// A relative path (`/` separated) below the workspace root.
pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        path.push(part);
    }
    path
}

/// The relative path of `path` below `root` with `/`, if it is below it.
pub fn rel_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Revision codes in their order: `B` < `Y` < `AA` (same alphabet as the server).
pub fn revision_order(code: &str) -> (usize, String) {
    (code.len(), code.to_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// State: what every local file is
// ---------------------------------------------------------------------------

/// A file of the workspace that came from the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative path with `/` (as on disk).
    pub path: String,
    pub project: Option<String>,
    pub item: String,
    pub revision: String,
    pub file_id: String,
    /// Name on the server (Creo iterations without `.<n>`).
    pub filename: String,
    /// The server version the local content was copied from (the *base*).
    pub version: i64,
    pub sha256: String,
    /// The user holds the check-out of the revision (the file is writable).
    #[serde(default)]
    pub checked_out: bool,
    /// Size and modification time when `sha256` was taken – an unchanged
    /// file is not hashed again.
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified_ms: u64,
}

/// `<workspace>/.teamcraft/state.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub version: u32,
    /// [`key_of`] the relative path → entry.
    #[serde(default)]
    pub files: BTreeMap<String, Entry>,
}

impl State {
    pub fn path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join(STATE_FILE)
    }

    /// Reads the state (empty if there is none; a damaged file is kept as
    /// `state.json.damaged` and the workspace starts over – the files on
    /// disk are never touched because of it).
    pub fn load(root: &Path) -> State {
        let path = State::path(root);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                let _ = std::fs::rename(&path, path.with_extension("json.damaged"));
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    /// Writes the state atomically (temporary file, then rename).
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let path = State::path(root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(&State {
            version: 1,
            files: self.files.clone(),
        })
        .map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    /// Writes `entries` into the state on disk. The state is read again
    /// under a lock of the process first, so two commands that run at the
    /// same time (a Get and a status, two Gets) never drop each other's
    /// entries – a lost entry would turn a file into a clash with "a local
    /// file that did not come from TeamCraft" that no Get repairs.
    pub fn store<I: IntoIterator<Item = Entry>>(root: &Path, entries: I) -> std::io::Result<()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = State::load(root);
        for entry in entries {
            state.insert(entry);
        }
        state.save(root)
    }

    /// The entries that are new or different compared with `before` (what a
    /// command changed in its copy of the state, for [`State::store`]).
    pub fn changes_since(&self, before: &State) -> Vec<Entry> {
        self.files
            .iter()
            .filter(|(key, entry)| before.files.get(*key) != Some(*entry))
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    pub fn get(&self, rel: &str) -> Option<&Entry> {
        self.files.get(&key_of(rel))
    }

    pub fn insert(&mut self, entry: Entry) {
        self.files.insert(key_of(&entry.path), entry);
    }

    /// The entries of one revision of an item.
    pub fn of_revision<'a>(
        &'a self,
        item: &'a str,
        revision: &'a str,
    ) -> impl Iterator<Item = &'a Entry> + 'a {
        self.files.values().filter(move |e| {
            e.item.eq_ignore_ascii_case(item) && e.revision.eq_ignore_ascii_case(revision)
        })
    }
}

// ---------------------------------------------------------------------------
// Get: what to download
// ---------------------------------------------------------------------------

/// A file on the server as the workspace needs it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerFile {
    pub project: Option<String>,
    pub item: String,
    pub revision: String,
    pub file_id: String,
    pub filename: String,
    pub folder: String,
    pub version: i64,
    pub sha256: String,
    pub size: u64,
    /// The user holds the check-out of the revision.
    pub locked_by_me: bool,
}

impl ServerFile {
    pub fn rel(&self) -> String {
        local_rel(self.project.as_deref(), &self.folder, &self.filename)
    }

    pub fn entry(&self, sha256: &str, size: u64, modified_ms: u64) -> Entry {
        Entry {
            path: self.rel(),
            project: self.project.clone(),
            item: self.item.clone(),
            revision: self.revision.clone(),
            file_id: self.file_id.clone(),
            filename: self.filename.clone(),
            version: self.version,
            sha256: sha256.to_string(),
            checked_out: self.locked_by_me,
            size,
            modified_ms,
        }
    }
}

/// What a Get does with one server file.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum GetAction {
    /// The local copy is the current version.
    UpToDate,
    /// Not on disk yet, or an unchanged copy of an older version (or of
    /// another revision of the same item) that is replaced.
    Download { replace: bool },
    /// Changed here: kept untouched, never overwritten.
    KeepModified,
    /// Another file sits at this place (another item with the same folder
    /// and name, or a file that did not come from TeamCraft): reported,
    /// never overwritten.
    Clash { with: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GetStep {
    pub rel: String,
    pub file: ServerFile,
    #[serde(flatten)]
    pub action: GetAction,
}

/// Plans a Get. `local` holds the SHA-256 of every file that exists at the
/// path of a server file ([`key_of`] → sha256).
pub fn plan_get(
    files: &[ServerFile],
    state: &State,
    local: &HashMap<String, String>,
) -> Vec<GetStep> {
    let mut claimed: HashMap<String, String> = HashMap::new();
    let mut steps = Vec::with_capacity(files.len());
    for file in files {
        let rel = file.rel();
        let key = key_of(&rel);
        let label = format!("{}/{} {}", file.item, file.revision, file.filename);
        if let Some(other) = claimed.get(&key) {
            steps.push(GetStep {
                rel,
                file: file.clone(),
                action: GetAction::Clash {
                    with: other.clone(),
                },
            });
            continue;
        }
        claimed.insert(key.clone(), label);
        let entry = state.get(&rel);
        let action = match local.get(&key) {
            None => GetAction::Download { replace: false },
            Some(sha) if sha.eq_ignore_ascii_case(&file.sha256) => GetAction::UpToDate,
            Some(sha) => match entry {
                Some(e) if e.item.eq_ignore_ascii_case(&file.item) => {
                    if sha.eq_ignore_ascii_case(&e.sha256) {
                        GetAction::Download { replace: true }
                    } else {
                        GetAction::KeepModified
                    }
                }
                Some(e) => GetAction::Clash {
                    with: format!("{}/{} {}", e.item, e.revision, e.filename),
                },
                None => GetAction::Clash {
                    with: "a local file that did not come from TeamCraft".into(),
                },
            },
        };
        steps.push(GetStep {
            rel,
            file: file.clone(),
            action,
        });
    }
    steps
}

/// Files the user has not checked out are read-only on disk, so nobody
/// edits a file that cannot be checked in; local changes are never touched.
pub fn read_only_wanted(checked_out_by_me: bool) -> bool {
    !checked_out_by_me
}

// ---------------------------------------------------------------------------
// Migration from the old layout <group>/<item>/<revision>/
// ---------------------------------------------------------------------------

/// A file of a revision folder of the old layout.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OldFile {
    /// Relative path in the workspace (`ROVER/PRT-000001/A/bracket.prt.3`).
    pub rel: String,
    pub group: Option<String>,
    pub item: String,
    pub revision: String,
    /// Name on disk (a Creo iteration keeps its number).
    pub disk_name: String,
    pub sha256: String,
    /// The server version the file was copied from (old base file), if known.
    pub base_version: Option<i64>,
    pub base_sha256: Option<String>,
    /// The server file of the same (logical) name, if there is one.
    pub server: Option<ServerFile>,
    /// Folder of a server file of the same revision to put a local-only file
    /// next to (same name stem first).
    pub sibling_folder: Option<String>,
}

impl OldFile {
    /// Changed since it was copied (or not known to be unchanged).
    pub fn modified(&self) -> bool {
        match (&self.base_sha256, &self.server) {
            (Some(base), _) => !base.eq_ignore_ascii_case(&self.sha256),
            (None, Some(s)) => !s.sha256.eq_ignore_ascii_case(&self.sha256),
            (None, None) => true,
        }
    }

    fn target(&self) -> Option<String> {
        let folder = match &self.server {
            Some(s) => s.folder.clone(),
            None => self.sibling_folder.clone()?,
        };
        Some(local_rel(self.group.as_deref(), &folder, &self.disk_name))
    }
}

/// Where an old file goes in the new layout (`None`: nothing shows where).
pub fn migration_target(f: &OldFile) -> Option<String> {
    f.target()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MigrationAction {
    /// Moved to its place in the new layout.
    Move { to: String },
    /// The same content is already there: the old copy is removed.
    AlreadyThere { to: String },
    /// Left where it is, with the reason.
    Keep { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MigrationStep {
    pub from: String,
    #[serde(flatten)]
    pub action: MigrationAction,
    /// Changed locally since it was copied from the server.
    pub modified: bool,
}

/// Plans the move into the new layout. `existing` holds the SHA-256 of
/// every file that already exists at a target path ([`key_of`] → sha256).
///
/// Nothing is ever overwritten: a file whose place is taken (by a different
/// file on disk, or by a file of another revision moved in this run) is kept
/// where it is and reported. Modified files are placed first, then newer
/// revisions, so local work wins over unchanged server copies.
pub fn plan_migration(files: &[OldFile], existing: &HashMap<String, String>) -> Vec<MigrationStep> {
    let mut order: Vec<&OldFile> = files.iter().collect();
    order.sort_by(|a, b| {
        b.modified()
            .cmp(&a.modified())
            .then_with(|| revision_order(&b.revision).cmp(&revision_order(&a.revision)))
            .then_with(|| a.rel.cmp(&b.rel))
    });
    let mut claimed: HashMap<String, String> = HashMap::new();
    let mut steps = Vec::with_capacity(files.len());
    for f in order {
        let modified = f.modified();
        let action = match f.target() {
            None => MigrationAction::Keep {
                reason:
                    "not on the server and no other file of the revision shows where it belongs"
                        .into(),
            },
            Some(to) => {
                let key = key_of(&to);
                if let Some(other) = claimed.get(&key) {
                    MigrationAction::Keep {
                        reason: format!("{to} is taken by {other}"),
                    }
                } else if key_of(&f.rel) == key {
                    // The file already is at its place (a server folder that
                    // looks like <item>/<revision>): it must stay – removing
                    // the "old copy" would delete the only one.
                    claimed.insert(key, f.rel.clone());
                    MigrationAction::Keep {
                        reason: "already at its place in the working folder".into(),
                    }
                } else {
                    match existing.get(&key) {
                        Some(sha) if sha.eq_ignore_ascii_case(&f.sha256) => {
                            claimed.insert(key, f.rel.clone());
                            MigrationAction::AlreadyThere { to }
                        }
                        Some(_) => MigrationAction::Keep {
                            reason: format!("a different file is already at {to}"),
                        },
                        None => {
                            claimed.insert(key, f.rel.clone());
                            MigrationAction::Move { to }
                        }
                    }
                }
            }
        };
        steps.push(MigrationStep {
            from: f.rel.clone(),
            action,
            modified,
        });
    }
    steps.sort_by(|a, b| a.from.cmp(&b.from));
    steps
}

/// A revision folder of the old layout found in the workspace.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OldDir {
    /// Relative path (`ROVER/PRT-000001/A`, or `PRT-000001/A`).
    pub rel: String,
    pub group: Option<String>,
    pub item: String,
    pub revision: String,
    /// It holds the base file of the old layout (certainly an old folder);
    /// without it the server has to confirm the item and revision.
    pub marked: bool,
}

/// Looks like a revision code (`A`, `B`, `AA`, `01`).
pub fn looks_like_revision(name: &str) -> bool {
    (1..=3).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Looks like an item number (`PRT-000001`, `ROVER-PRT-2026-0001`, `NX-ENG-001`).
pub fn looks_like_item(name: &str) -> bool {
    name.len() >= 3
        && name.len() <= 64
        && name.bytes().any(|b| b.is_ascii_digit())
        && name.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
}

/// Candidates for revision folders of the old layout: `rel` paths of
/// folders that hold only files, with whether they hold the old base file.
/// `<group>/<item>/<rev>` and `<item>/<rev>` are recognised; folders with
/// files the new layout already tracks are not old folders.
pub fn old_dir_candidates(dirs: &[(String, bool)], state: &State) -> Vec<OldDir> {
    let tracked: HashSet<String> = state
        .files
        .keys()
        .filter_map(|k| k.rsplit_once('/').map(|(dir, _)| dir.to_string()))
        .collect();
    let mut out = Vec::new();
    for (rel, marked) in dirs {
        if tracked.contains(&key_of(rel)) {
            continue;
        }
        let parts: Vec<&str> = rel.split('/').collect();
        let (group, item, revision) = match parts.as_slice() {
            [group, item, rev] => (Some(group.to_string()), *item, *rev),
            [item, rev] => (None, *item, *rev),
            _ => continue,
        };
        if !looks_like_revision(revision) || (!*marked && !looks_like_item(item)) {
            continue;
        }
        out.push(OldDir {
            rel: rel.clone(),
            group,
            item: item.to_string(),
            revision: revision.to_string(),
            marked: *marked,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Local changes (before "Check in all" and sign-out) and parking
// ---------------------------------------------------------------------------

/// Whether a file state of the Workspace tab is local work the server does
/// not have yet: `modified` and `new` can be checked in, a `conflict` has to
/// be merged first. Everything else (`up_to_date`, `outdated`, `missing`,
/// `clash`, `inaccessible`) is not work of the user's.
pub fn is_local_change(state: &str) -> bool {
    matches!(state, "modified" | "new" | "conflict")
}

/// A check-out of the user, as the worklist lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutRef {
    /// The revision id on the server (for `POST /worklist/checkin-all`).
    pub id: String,
    pub project: Option<String>,
    pub item: String,
    pub code: String,
}

/// A check-out with local changes in the working folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChangedRevision {
    #[serde(flatten)]
    pub revision: CheckoutRef,
    /// Files that can be checked in (or parked) as they are: `modified`, `new`.
    pub upload: Vec<String>,
    /// Files changed locally *and* on the server: they have to be merged in
    /// the Workspace tab first, a check-in would be refused (409).
    pub conflicts: Vec<String>,
}

/// The check-outs of the user split by their local changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LocalChanges {
    /// Revisions with local changes – to be checked in with their files (or
    /// parked) before a bulk check-in or sign-out, never released silently.
    pub changed: Vec<ChangedRevision>,
    /// Revisions without local changes: a plain lock release loses nothing.
    pub clean: Vec<CheckoutRef>,
    /// Revisions whose working folder could not be compared, with the error:
    /// treated as changed by the callers (nothing is released blindly).
    pub unreadable: Vec<(CheckoutRef, String)>,
}

impl LocalChanges {
    /// How many revisions hold work that exists only on this computer (or
    /// might: the unreadable ones).
    pub fn at_risk(&self) -> usize {
        self.changed.len() + self.unreadable.len()
    }
}

/// Sorts check-outs by the states of their local files (`(name, state)` per
/// file, from the Workspace status) into changed, clean and unreadable,
/// keeping the order of the worklist.
pub fn local_changes<I>(results: I) -> LocalChanges
where
    I: IntoIterator<Item = (CheckoutRef, Result<Vec<(String, String)>, String>)>,
{
    let mut out = LocalChanges::default();
    for (revision, files) in results {
        let files = match files {
            Ok(files) => files,
            Err(e) => {
                out.unreadable.push((revision, e));
                continue;
            }
        };
        let mut upload = Vec::new();
        let mut conflicts = Vec::new();
        for (name, state) in files {
            match state.as_str() {
                "conflict" => conflicts.push(name),
                s if is_local_change(s) => upload.push(name),
                _ => {}
            }
        }
        if upload.is_empty() && conflicts.is_empty() {
            out.clean.push(revision);
        } else {
            out.changed.push(ChangedRevision {
                revision,
                upload,
                conflicts,
            });
        }
    }
    out
}

/// The comment of a parked upload when the user gives none (file history,
/// audit log).
pub const PARK_COMMENT: &str = "Work in progress (parked, still checked out)";

/// The comment a park stores: the user's, or [`PARK_COMMENT`].
pub fn park_comment(comment: &str) -> String {
    let comment = comment.trim();
    if comment.is_empty() {
        PARK_COMMENT.to_string()
    } else {
        comment.to_string()
    }
}

// ---------------------------------------------------------------------------
// Log
// ---------------------------------------------------------------------------

/// `2026-11-04 21:37:16 UTC` for seconds since 1970 (no time zone database
/// needed; days → civil date after Howard Hinnant's algorithm).
pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(item: &str, rev: &str, name: &str, folder: &str, sha: &str) -> ServerFile {
        ServerFile {
            project: Some("ROVER".into()),
            item: item.into(),
            revision: rev.into(),
            file_id: format!("id-{item}-{name}"),
            filename: name.into(),
            folder: folder.into(),
            version: 2,
            sha256: sha.into(),
            size: 10,
            locked_by_me: false,
        }
    }

    #[test]
    fn names() {
        assert_eq!(safe_name("PRT-000001"), "PRT-000001");
        assert_eq!(safe_name("a/b\\c:d"), "a_b_c_d");
        assert_eq!(safe_name(" .. "), "_");
        assert_eq!(safe_name("CON?"), "CON_");
        assert!(is_ignored("~$drawing.docx"));
        assert!(is_ignored("~$Bracket.SLDPRT"), "SolidWorks lock file");
        assert!(is_ignored(".DS_Store"));
        assert!(is_ignored("part.step.teamcraft-download"));
        for name in [
            "layout.dwl",
            "layout.dwl2",
            "layout.sv$",
            "board.kicad_pcb-bak",
            "_autosave-board.kicad_pcb",
            "~board.kicad_pcb.lck",
            "board.kicad_prl",
            "trail.txt.12",
            "std.out",
            "Thumbs.db",
            "rover.FCBak",
            "scene.blend1",
            "board.b#1",
            "power.s#3",
        ] {
            assert!(is_ignored(name), "{name}");
        }
        for name in [
            "nozzle.step",
            "Bracket.SLDPRT",
            "housing.prt.3",
            "board.kicad_pcb",
            "power.sch",
            "rover.FCStd",
            "notes.txt",
        ] {
            assert!(!is_ignored(name), "{name}");
        }
        assert_eq!(logical_name("bracket.prt.7"), ("bracket.prt", 7));
        assert_eq!(logical_name("Frame.ASM.12"), ("Frame.ASM", 12));
        assert_eq!(logical_name("bracket.prt"), ("bracket.prt", 0));
        assert_eq!(logical_name("report.pdf.2"), ("report.pdf.2", 0));
        assert_eq!(logical_name("v1.2"), ("v1.2", 0));
        assert_eq!(stem_of("Bracket.SLDPRT"), "bracket");
        assert_eq!(stem_of("bracket.prt.7"), "bracket");
        assert_eq!(stem_of("README"), "readme");
    }

    #[test]
    fn folders_like_the_server() {
        assert_eq!(
            normalize_folder("\\chassis\\brackets\\").unwrap(),
            "chassis/brackets"
        );
        assert_eq!(normalize_folder("/a//./b/").unwrap(), "a/b");
        assert_eq!(normalize_folder("").unwrap(), "");
        for bad in [
            "..",
            "a/../b",
            "C:/x",
            "a|b",
            "trailing.",
            "CON",
            "com1/x",
            "x?",
        ] {
            assert!(normalize_folder(bad).is_err(), "{bad}");
        }
        assert_eq!(folder_for_upload("Rover/Parts"), "Rover/Parts");
        assert_eq!(folder_for_upload("Rover: old/Parts?"), "Rover_ old/Parts_");
        assert_eq!(folder_for_upload("../x/CON"), "x/CON_");
    }

    #[test]
    fn layout_mapping() {
        assert_eq!(
            local_rel(Some("ROVER"), "chassis/brackets", "bracket.prt"),
            "ROVER/chassis/brackets/bracket.prt"
        );
        assert_eq!(local_rel(Some("ROVER"), "", "rover.asm"), "ROVER/rover.asm");
        assert_eq!(local_rel(None, "chassis", "a.step"), "chassis/a.step");
        assert_eq!(local_rel(Some(" "), "", "a.step"), "a.step");
        assert_eq!(
            local_rel(Some("ROVER"), "chassis\\..\\x", "a.step"),
            "ROVER/chassis/x/a.step",
            "a bad folder from an old server never leaves the group"
        );
        assert_eq!(local_rel(Some("R&D: 1"), "a", "b/c"), "R&D_ 1/a/b_c");
        assert_eq!(local_dir_rel(Some("ROVER"), "chassis"), "ROVER/chassis");
        assert_eq!(local_dir_rel(None, ""), "");
        assert_eq!(key_of("ROVER\\Chassis/A.STEP"), "rover/chassis/a.step");
        let root = Path::new("/ws");
        let path = join_rel(root, "ROVER/chassis/a.step");
        assert_eq!(path, Path::new("/ws/ROVER/chassis/a.step"));
        assert_eq!(rel_of(root, &path).as_deref(), Some("ROVER/chassis/a.step"));
        assert_eq!(rel_of(root, Path::new("/elsewhere/a")), None);
        assert!(revision_order("B") < revision_order("Y"));
        assert!(revision_order("Y") < revision_order("AA"));
    }

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::load(dir.path());
        assert_eq!(state, State::default());
        let f = server("PRT-000001", "A", "Bracket.prt", "chassis", "s1");
        state.insert(f.entry("s1", 10, 99));
        state.save(dir.path()).unwrap();
        let loaded = State::load(dir.path());
        assert_eq!(loaded.version, 1);
        assert_eq!(
            loaded
                .get("rover/CHASSIS/bracket.PRT")
                .map(|e| e.item.as_str()),
            Some("PRT-000001")
        );
        assert_eq!(loaded.of_revision("prt-000001", "a").count(), 1);
        std::fs::write(State::path(dir.path()), b"{ damaged").unwrap();
        assert_eq!(State::load(dir.path()), State::default());
        assert!(dir.path().join(".teamcraft/state.json.damaged").is_file());
    }

    #[test]
    fn concurrent_commands_keep_each_others_entries() {
        let dir = tempfile::tempdir().unwrap();
        let a = server("PRT-000001", "A", "a.prt", "chassis", "sa");
        let b = server("PRT-000002", "A", "b.prt", "chassis", "sb");
        // Both commands loaded the empty state before either one saved.
        let before = State::load(dir.path());
        let (mut first, mut second) = (before.clone(), before.clone());
        first.insert(a.entry("sa", 1, 1));
        second.insert(b.entry("sb", 1, 1));
        assert_eq!(first.changes_since(&before), vec![a.entry("sa", 1, 1)]);
        assert!(first.changes_since(&first).is_empty());
        State::store(dir.path(), first.changes_since(&before)).unwrap();
        State::store(dir.path(), second.changes_since(&before)).unwrap();
        let state = State::load(dir.path());
        assert!(state.get("ROVER/chassis/a.prt").is_some());
        assert!(state.get("ROVER/chassis/b.prt").is_some());
        // A newer entry for the same place replaces the older one.
        let mut a2 = a.entry("sa2", 2, 2);
        a2.version = 3;
        State::store(dir.path(), [a2]).unwrap();
        assert_eq!(
            State::load(dir.path())
                .get("rover/chassis/A.PRT")
                .unwrap()
                .version,
            3
        );
    }

    #[test]
    fn get_downloads_only_what_is_missing_or_outdated() {
        let files = vec![
            server("ASM-000001", "A", "chassis.asm", "chassis", "asm2"),
            server("PRT-000001", "B", "bracket.prt", "chassis/brackets", "prt2"),
            server("PRT-000002", "A", "plate.prt", "chassis", "plate2"),
            server("PRT-000003", "A", "mine.prt", "chassis", "mine2"),
            server(
                "PRT-000004",
                "A",
                "bracket.prt",
                "chassis/brackets",
                "other",
            ),
            server("PRT-000005", "A", "taken.prt", "chassis", "t2"),
            server("PRT-000006", "A", "stray.prt", "chassis", "s2"),
        ];
        let mut state = State::default();
        // bracket.prt is an unchanged copy of revision A: replaced by B
        let mut old = files[1].entry("prt1", 1, 1);
        old.revision = "A".into();
        state.insert(old);
        // mine.prt was edited here
        state.insert(files[3].entry("mine1", 1, 1));
        // taken.prt holds another item's file
        let mut foreign = files[5].entry("t1", 1, 1);
        foreign.item = "PRT-000099".into();
        state.insert(foreign);
        let local: HashMap<String, String> = [
            ("rover/chassis/chassis.asm", "asm2"),
            ("rover/chassis/brackets/bracket.prt", "prt1"),
            ("rover/chassis/mine.prt", "edited"),
            ("rover/chassis/taken.prt", "t1"),
            ("rover/chassis/stray.prt", "whatever"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let steps = plan_get(&files, &state, &local);
        let actions: Vec<(&str, &GetAction)> = steps
            .iter()
            .map(|s| (s.file.filename.as_str(), &s.action))
            .collect();
        assert_eq!(actions[0], ("chassis.asm", &GetAction::UpToDate));
        assert_eq!(
            actions[1],
            ("bracket.prt", &GetAction::Download { replace: true })
        );
        assert_eq!(
            actions[2],
            ("plate.prt", &GetAction::Download { replace: false })
        );
        assert_eq!(actions[3], ("mine.prt", &GetAction::KeepModified));
        assert_eq!(
            actions[4],
            (
                "bracket.prt",
                &GetAction::Clash {
                    with: "PRT-000001/B bracket.prt".into()
                }
            ),
            "two items with the same folder and name"
        );
        assert_eq!(
            actions[5],
            (
                "taken.prt",
                &GetAction::Clash {
                    with: "PRT-000099/A taken.prt".into()
                }
            )
        );
        assert!(
            matches!(actions[6].1, GetAction::Clash { .. }),
            "untracked file"
        );
        assert!(read_only_wanted(false));
        assert!(!read_only_wanted(true));
    }

    fn old(
        rel: &str,
        rev: &str,
        sha: &str,
        base: Option<&str>,
        server_file: Option<ServerFile>,
    ) -> OldFile {
        let parts: Vec<&str> = rel.split('/').collect();
        OldFile {
            rel: rel.into(),
            group: Some(parts[0].into()),
            item: parts[1].into(),
            revision: rev.into(),
            disk_name: parts[3].into(),
            sha256: sha.into(),
            base_version: base.map(|_| 1),
            base_sha256: base.map(str::to_string),
            server: server_file,
            sibling_folder: Some("chassis".into()),
        }
    }

    #[test]
    fn migration_never_overwrites() {
        let s = |item: &str, rev: &str, name: &str, sha: &str| {
            Some(server(item, rev, name, "chassis", sha))
        };
        let files = vec![
            // two revisions of one part: the edited older one wins the place
            old(
                "ROVER/PRT-000001/A/bracket.prt",
                "A",
                "edited",
                Some("a1"),
                s("PRT-000001", "A", "bracket.prt", "a1"),
            ),
            old(
                "ROVER/PRT-000001/B/bracket.prt",
                "B",
                "b1",
                Some("b1"),
                s("PRT-000001", "B", "bracket.prt", "b1"),
            ),
            // unchanged copy, nothing at the target
            old(
                "ROVER/PRT-000002/A/plate.prt",
                "A",
                "p1",
                None,
                s("PRT-000002", "A", "plate.prt", "p1"),
            ),
            // a different file is already at the target
            old(
                "ROVER/PRT-000003/A/rib.prt",
                "A",
                "r1",
                Some("r1"),
                s("PRT-000003", "A", "rib.prt", "r1"),
            ),
            // the same content is already there
            old(
                "ROVER/PRT-000004/A/nut.prt",
                "A",
                "n1",
                Some("n1"),
                s("PRT-000004", "A", "nut.prt", "n1"),
            ),
            // a local-only STEP export goes next to its siblings
            old("ROVER/PRT-000002/A/plate.step", "A", "x", None, None),
        ];
        let mut no_sibling = old("ROVER/PRT-000009/A/lonely.txt", "A", "l", None, None);
        no_sibling.sibling_folder = None;
        let mut files = files;
        files.push(no_sibling);
        let existing: HashMap<String, String> = [
            ("rover/chassis/rib.prt", "something else"),
            ("rover/chassis/nut.prt", "n1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let steps = plan_migration(&files, &existing);
        let by_from: HashMap<&str, &MigrationStep> =
            steps.iter().map(|s| (s.from.as_str(), s)).collect();
        assert_eq!(
            by_from["ROVER/PRT-000001/A/bracket.prt"].action,
            MigrationAction::Move {
                to: "ROVER/chassis/bracket.prt".into()
            }
        );
        assert!(by_from["ROVER/PRT-000001/A/bracket.prt"].modified);
        assert_eq!(
            by_from["ROVER/PRT-000001/B/bracket.prt"].action,
            MigrationAction::Keep {
                reason: "ROVER/chassis/bracket.prt is taken by ROVER/PRT-000001/A/bracket.prt"
                    .into()
            }
        );
        assert_eq!(
            by_from["ROVER/PRT-000002/A/plate.prt"].action,
            MigrationAction::Move {
                to: "ROVER/chassis/plate.prt".into()
            }
        );
        assert!(!by_from["ROVER/PRT-000002/A/plate.prt"].modified);
        assert_eq!(
            by_from["ROVER/PRT-000003/A/rib.prt"].action,
            MigrationAction::Keep {
                reason: "a different file is already at ROVER/chassis/rib.prt".into()
            }
        );
        assert_eq!(
            by_from["ROVER/PRT-000004/A/nut.prt"].action,
            MigrationAction::AlreadyThere {
                to: "ROVER/chassis/nut.prt".into()
            }
        );
        assert_eq!(
            by_from["ROVER/PRT-000002/A/plate.step"].action,
            MigrationAction::Move {
                to: "ROVER/chassis/plate.step".into()
            }
        );
        assert!(matches!(
            by_from["ROVER/PRT-000009/A/lonely.txt"].action,
            MigrationAction::Keep { .. }
        ));
        // without a newer edit, the newer revision wins
        let files = vec![
            old(
                "ROVER/PRT-000001/A/bracket.prt",
                "A",
                "a1",
                Some("a1"),
                s("PRT-000001", "A", "bracket.prt", "a1"),
            ),
            old(
                "ROVER/PRT-000001/B/bracket.prt",
                "B",
                "b1",
                Some("b1"),
                s("PRT-000001", "B", "bracket.prt", "b1"),
            ),
        ];
        let steps = plan_migration(&files, &HashMap::new());
        assert!(
            matches!(steps[0].action, MigrationAction::Keep { .. }),
            "{steps:?}"
        );
        assert!(matches!(steps[1].action, MigrationAction::Move { .. }));
    }

    #[test]
    fn a_file_already_in_place_is_never_removed() {
        // The server folder of the file is PRT-000005/A, so the "old"
        // folder is its place in the new layout (e.g. after the state was
        // lost): its target is the file itself.
        let axle = old(
            "ROVER/PRT-000005/A/axle.prt",
            "A",
            "x1",
            Some("x1"),
            Some(server("PRT-000005", "A", "axle.prt", "PRT-000005/A", "x1")),
        );
        let existing: HashMap<String, String> =
            [("rover/prt-000005/a/axle.prt".to_string(), "x1".to_string())].into();
        let steps = plan_migration(&[axle], &existing);
        assert_eq!(
            steps[0].action,
            MigrationAction::Keep {
                reason: "already at its place in the working folder".into()
            }
        );
    }

    #[test]
    fn old_folders_are_recognised() {
        let mut state = State::default();
        state.insert(server("PRT-000007", "A", "x.prt", "PRT-000007/A", "s").entry("s", 1, 1));
        let dirs: Vec<(String, bool)> = [
            ("ROVER/PRT-000001/A", false),
            ("ROVER/Weird Name/B", true),
            ("ROVER/chassis/brackets", false),
            ("PRT-000002/AA", false),
            ("ROVER/PRT-000003/A", true),
            ("ROVER/PRT-000007/A", false),
            ("ROVER/a/b/c", true),
        ]
        .into_iter()
        .map(|(r, m)| (r.to_string(), m))
        .collect();
        let found: Vec<(Option<String>, String, String)> = old_dir_candidates(&dirs, &state)
            .into_iter()
            .map(|d| (d.group, d.item, d.revision))
            .collect();
        assert_eq!(
            found,
            vec![
                (Some("ROVER".into()), "PRT-000001".into(), "A".into()),
                (Some("ROVER".into()), "Weird Name".into(), "B".into()),
                (None, "PRT-000002".into(), "AA".into()),
                (Some("ROVER".into()), "PRT-000003".into(), "A".into()),
            ],
            "chassis/brackets is no revision; ROVER/PRT-000007/A is tracked by the new layout"
        );
    }

    #[test]
    fn utc_dates() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(1_793_828_236), "2026-11-04 21:37:16 UTC");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00 UTC");
    }

    fn checkout(item: &str) -> CheckoutRef {
        CheckoutRef {
            id: format!("id-{item}"),
            project: Some("ROVER".into()),
            item: item.into(),
            code: "A".into(),
        }
    }

    fn files(list: &[(&str, &str)]) -> Result<Vec<(String, String)>, String> {
        Ok(list
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect())
    }

    #[test]
    fn local_change_states() {
        for s in ["modified", "new", "conflict"] {
            assert!(is_local_change(s), "{s}");
        }
        for s in [
            "up_to_date",
            "outdated",
            "missing",
            "clash",
            "inaccessible",
            "",
        ] {
            assert!(!is_local_change(s), "{s}");
        }
    }

    #[test]
    fn check_outs_with_local_changes_are_listed() {
        let report = local_changes(vec![
            (
                checkout("PRT-1"),
                files(&[("a.prt", "up_to_date"), ("a.step", "outdated")]),
            ),
            (
                checkout("PRT-2"),
                files(&[
                    ("b.prt", "modified"),
                    ("b.step", "new"),
                    ("b.drw", "conflict"),
                    ("b.pdf", "missing"),
                ]),
            ),
            (checkout("PRT-3"), Err("server not reachable".into())),
            (checkout("PRT-4"), files(&[])),
            (checkout("PRT-5"), files(&[("e.prt", "conflict")])),
            (checkout("PRT-6"), files(&[("f.prt", "clash")])),
        ]);
        assert_eq!(
            report.changed,
            vec![
                ChangedRevision {
                    revision: checkout("PRT-2"),
                    upload: vec!["b.prt".into(), "b.step".into()],
                    conflicts: vec!["b.drw".into()],
                },
                ChangedRevision {
                    revision: checkout("PRT-5"),
                    upload: vec![],
                    conflicts: vec!["e.prt".into()],
                },
            ]
        );
        assert_eq!(
            report.clean,
            vec![checkout("PRT-1"), checkout("PRT-4"), checkout("PRT-6")],
            "no local work: a plain release loses nothing (a clash is another item's file)"
        );
        assert_eq!(
            report.unreadable,
            vec![(checkout("PRT-3"), "server not reachable".to_string())]
        );
        assert_eq!(report.at_risk(), 3, "unreadable counts as at risk");
        assert_eq!(local_changes(Vec::new()), LocalChanges::default());
        assert_eq!(LocalChanges::default().at_risk(), 0);
    }

    #[test]
    fn changed_revisions_serialize_flat() {
        let report = local_changes(vec![(checkout("PRT-2"), files(&[("b.prt", "modified")]))]);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["changed"][0]["item"], "PRT-2");
        assert_eq!(json["changed"][0]["id"], "id-PRT-2");
        assert_eq!(json["changed"][0]["upload"][0], "b.prt");
        assert_eq!(json["clean"], serde_json::json!([]));
    }

    #[test]
    fn park_comments() {
        assert_eq!(park_comment(""), PARK_COMMENT);
        assert_eq!(park_comment("  \n"), PARK_COMMENT);
        assert_eq!(park_comment(" fillet fixed "), "fillet fixed");
    }
}
