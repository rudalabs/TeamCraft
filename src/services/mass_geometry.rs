//! Which geometry file a revision's mass is computed from (pure rules).
//!
//! The 3D files of a revision are grouped by *model*: files whose
//! normalised stem is the same (`box.prt`, `Box.SLDPRT`, `box_rev-B.stp`)
//! are one model in several formats. One model is picked automatically
//! (format rank, the user's preferred CAD program breaks ties between native
//! formats); files of several models are *ambiguous* and add nothing until a
//! file is chosen, because the mass of one arbitrary part is worse than none.
//! A candidate that is probably the whole assembly (the root of the file
//! references of the revision, or named like the item) is suggested.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use uuid::Uuid;

use super::mass_properties::{GeometryFile, GEOMETRY_FORMATS};
use crate::cadformat;

/// The shortest item number or name that counts as a name match.
const MIN_NAME_LEN: usize = 3;

/// File name without extension, lower case, `_`, `-` and spaces folded to
/// single spaces and a trailing revision (`rev-D`, `_rev_B`, `revB`)
/// removed. Creo version extensions (`.prt.5`) are dropped as well.
pub fn normalise_stem(filename: &str) -> String {
    let mut stem = filename;
    if let Some((rest, ext)) = stem.rsplit_once('.') {
        stem = rest;
        if !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_digit()) {
            if let Some((again, _)) = stem.rsplit_once('.') {
                stem = again;
            }
        }
    }
    let lower = stem.to_lowercase();
    let mut words: Vec<&str> = lower
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect();
    let n = words.len();
    if n >= 3 && words[n - 2] == "rev" && words[n - 1].len() <= 2 {
        words.truncate(n - 2);
    } else if n >= 2 && words[n - 1].starts_with("rev") && words[n - 1].len() <= 5 {
        words.truncate(n - 1);
    }
    words.join(" ")
}

/// Folds an item number or name like a stem (no extension handling).
fn fold(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The files of a revision that look like the root of an assembly: they
/// reference other files of the revision (by name) and no other file of the
/// revision references them. `files` are `(id, file name)`, `references`
/// are `(referencing file id, referenced name)`.
pub fn reference_roots(files: &[(Uuid, String)], references: &[(Uuid, String)]) -> HashSet<Uuid> {
    let by_name: BTreeMap<String, Uuid> = files
        .iter()
        .map(|(id, name)| (name.to_lowercase(), *id))
        .collect();
    let mut referencing: HashSet<Uuid> = HashSet::new();
    let mut referenced: HashSet<Uuid> = HashSet::new();
    for (from, name) in references {
        if let Some(to) = by_name.get(&name.to_lowercase()) {
            if to != from {
                referencing.insert(*from);
                referenced.insert(*to);
            }
        }
    }
    referencing.difference(&referenced).copied().collect()
}

/// What the choice of a revision's geometry file knows besides the files.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hints<'a> {
    pub item_number: &'a str,
    pub item_name: &'a str,
    /// Files that are the root of the file references of the revision.
    pub roots: Option<&'a HashSet<Uuid>>,
}

/// Several files of one model, best first.
#[derive(Clone, Debug)]
pub struct Group<'a> {
    pub stem: String,
    pub files: Vec<&'a GeometryFile>,
}

/// The result of the choice.
#[derive(Clone, Debug)]
pub enum Pick<'a> {
    /// No 3D file.
    Nothing,
    /// Chosen by hand.
    Chosen(&'a GeometryFile),
    /// One model: its best file; `other_formats` files of the same model.
    Automatic {
        file: &'a GeometryFile,
        other_formats: usize,
    },
    /// Files of several models, none chosen.
    Ambiguous { models: usize, files: usize },
}

#[derive(Clone, Debug)]
pub struct Resolution<'a> {
    pub pick: Pick<'a>,
    /// Models, the likely whole assembly first, then by name.
    pub groups: Vec<Group<'a>>,
    /// The file suggested as the whole assembly (only when ambiguous).
    pub likely: Option<&'a GeometryFile>,
}

impl Resolution<'_> {
    /// The file the mass is computed from.
    pub fn file(&self) -> Option<&GeometryFile> {
        match &self.pick {
            Pick::Chosen(f) | Pick::Automatic { file: f, .. } => Some(f),
            Pick::Nothing | Pick::Ambiguous { .. } => None,
        }
    }

    pub fn is_ambiguous(&self) -> bool {
        matches!(self.pick, Pick::Ambiguous { .. })
    }
}

fn is_native(format: &str) -> bool {
    cadformat::family(format).is_some()
}

/// Sort key inside a model: mesh and neutral STEP before native formats, a
/// native format of the preferred CAD program before the other native ones,
/// then the format rank, then the name.
fn order(f: &GeometryFile, preferred: &str) -> (u8, usize, String) {
    let preferred_native =
        !preferred.is_empty() && cadformat::family(&f.format).is_some_and(|s| s.id == preferred);
    let class = u8::from(is_native(&f.format) && !preferred_native);
    let rank = GEOMETRY_FORMATS
        .iter()
        .position(|g| *g == f.format)
        .unwrap_or(GEOMETRY_FORMATS.len());
    (class, rank, f.filename.clone())
}

/// Groups the candidates by model, each group best file first.
pub fn groups<'a>(files: &'a [GeometryFile], preferred: &str) -> Vec<Group<'a>> {
    let mut map: BTreeMap<String, Vec<&GeometryFile>> = BTreeMap::new();
    for f in files {
        map.entry(normalise_stem(&f.filename)).or_default().push(f);
    }
    map.into_iter()
        .map(|(stem, mut files)| {
            files.sort_by_cached_key(|f| order(f, preferred));
            Group { stem, files }
        })
        .collect()
}

/// The group that is probably the whole assembly: the only one holding the
/// root of the file references, else the only one named like the item.
fn likely_group(groups: &[Group<'_>], hints: &Hints<'_>) -> Option<usize> {
    let number = fold(hints.item_number);
    let name = fold(hints.item_name);
    let named = |stem: &str| {
        [&number, &name]
            .iter()
            .any(|n| n.len() >= MIN_NAME_LEN && stem.contains(n.as_str()))
    };
    let rooted = |g: &Group<'_>| {
        hints
            .roots
            .is_some_and(|roots| g.files.iter().any(|f| roots.contains(&f.id)))
    };
    let only = |hits: Vec<usize>| (hits.len() == 1).then(|| hits[0]);
    only((0..groups.len()).filter(|i| rooted(&groups[*i])).collect()).or_else(|| {
        only(
            (0..groups.len())
                .filter(|i| named(&groups[*i].stem))
                .collect(),
        )
    })
}

/// The file that stands for a whole group: a neutral exchange file (STEP,
/// JT, STL …) if there is one, else the best native file.
fn representative<'a>(group: &Group<'a>) -> &'a GeometryFile {
    group
        .files
        .iter()
        .find(|f| !is_native(&f.format))
        .copied()
        .unwrap_or(group.files[0])
}

/// Chooses the geometry file of a revision from its candidates.
pub fn resolve<'a>(
    files: &'a [GeometryFile],
    chosen: Option<Uuid>,
    preferred: &str,
    hints: &Hints<'_>,
) -> Resolution<'a> {
    let mut groups = groups(files, preferred);
    let nothing = |pick| Resolution {
        pick,
        groups: Vec::new(),
        likely: None,
    };
    if files.is_empty() {
        return nothing(Pick::Nothing);
    }
    let chosen_file = chosen.and_then(|id| files.iter().find(|f| f.id == id));
    let likely_idx = likely_group(&groups, hints);
    if groups.len() > 1 {
        if let Some(i) = likely_idx {
            let g = groups.remove(i);
            groups.insert(0, g);
        }
    }
    if let Some(file) = chosen_file {
        return Resolution {
            pick: Pick::Chosen(file),
            groups,
            likely: None,
        };
    }
    if groups.len() == 1 {
        let g = &groups[0];
        let file = g.files[0];
        return Resolution {
            pick: Pick::Automatic {
                file,
                other_formats: g.files.len() - 1,
            },
            groups,
            likely: None,
        };
    }
    let likely = likely_idx.map(|_| representative(&groups[0]));
    Resolution {
        pick: Pick::Ambiguous {
            models: groups.len(),
            files: files.len(),
        },
        groups,
        likely,
    }
}

// ---------------------------------------------------------------------------
// What is shown and exported
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct CandidateOut {
    pub file_id: Uuid,
    pub filename: String,
    pub format: String,
    /// The suggested whole-assembly file.
    pub likely: bool,
    /// The file the mass is computed from.
    pub used: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupOut {
    /// The normalised name shared by the files.
    pub model: String,
    pub files: Vec<CandidateOut>,
}

/// How the geometry file of a revision was decided, for the page, the
/// exports, REST and MCP.
#[derive(Clone, Debug, Serialize)]
pub struct GeometryInfo {
    /// `none`, `chosen` (by hand), `automatic` or `ambiguous`.
    pub choice: &'static str,
    pub file: Option<String>,
    pub file_id: Option<Uuid>,
    /// Further formats of the same model (automatic choice).
    pub other_formats: usize,
    /// Different models among the candidates.
    pub models: usize,
    /// Candidate files.
    pub files: usize,
    pub likely_file: Option<String>,
    pub likely_file_id: Option<Uuid>,
    /// The candidates by model (likely whole assembly first); empty for a
    /// single model unless `with_groups` was asked for.
    pub candidates: Vec<GroupOut>,
}

impl GeometryInfo {
    pub fn is_ambiguous(&self) -> bool {
        self.choice == "ambiguous"
    }

    /// The warning of an ambiguous choice.
    pub fn ambiguity_warning(&self) -> String {
        let mut text = format!(
            "{} files of {} different parts – choose the geometry file in Mass settings, or split the parts into BOM lines (one item per part)",
            self.files, self.models
        );
        if let Some(f) = &self.likely_file {
            text.push_str(&format!("; likely the whole assembly: {f}"));
        }
        text
    }

    /// A short note for the detail column.
    pub fn note(&self) -> String {
        match self.choice {
            "automatic" => {
                let n = self.other_formats;
                if n == 0 {
                    "chosen automatically".to_string()
                } else {
                    format!(
                        "chosen automatically; {n} other format{} of the same model",
                        if n == 1 { "" } else { "s" }
                    )
                }
            }
            "ambiguous" => {
                let names: Vec<&str> = self
                    .candidates
                    .iter()
                    .flat_map(|g| &g.files)
                    .map(|c| c.filename.as_str())
                    .collect();
                format!("candidates: {}", names.join("; "))
            }
            _ => String::new(),
        }
    }
}

/// Describes a resolution; the candidates are listed when ambiguous or when
/// `with_groups` is set.
pub fn info(r: &Resolution<'_>, with_groups: bool) -> GeometryInfo {
    let used = r.file().map(|f| f.id);
    let (choice, other_formats, models, files) = match &r.pick {
        Pick::Nothing => ("none", 0, 0, 0),
        Pick::Chosen(_) => (
            "chosen",
            0,
            r.groups.len(),
            r.groups.iter().map(|g| g.files.len()).sum(),
        ),
        Pick::Automatic { other_formats, .. } => {
            ("automatic", *other_formats, 1, other_formats + 1)
        }
        Pick::Ambiguous { models, files } => ("ambiguous", 0, *models, *files),
    };
    let candidates = if with_groups || r.is_ambiguous() {
        r.groups
            .iter()
            .map(|g| GroupOut {
                model: g.stem.clone(),
                files: g
                    .files
                    .iter()
                    .map(|f| CandidateOut {
                        file_id: f.id,
                        filename: f.filename.clone(),
                        format: f.format.clone(),
                        likely: r.likely.is_some_and(|l| l.id == f.id),
                        used: used == Some(f.id),
                    })
                    .collect(),
            })
            .collect()
    } else {
        Vec::new()
    };
    GeometryInfo {
        choice,
        file: r.file().map(|f| f.filename.clone()),
        file_id: used,
        other_formats,
        models,
        files,
        likely_file: r.likely.map(|f| f.filename.clone()),
        likely_file_id: r.likely.map(|f| f.id),
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, format: &str) -> GeometryFile {
        GeometryFile {
            id: Uuid::new_v4(),
            revision_id: Uuid::nil(),
            filename: name.into(),
            sha256: name.into(),
            format: format.into(),
            length_unit: None,
            instances: 1,
        }
    }

    fn picked(files: &[GeometryFile], chosen: Option<Uuid>, hints: &Hints<'_>) -> String {
        let r = resolve(files, chosen, "", hints);
        match &r.pick {
            Pick::Nothing => "nothing".into(),
            Pick::Chosen(f) => format!("chosen {}", f.filename),
            Pick::Automatic {
                file,
                other_formats,
            } => format!("auto {} +{other_formats}", file.filename),
            Pick::Ambiguous { models, files } => format!("ambiguous {models}/{files}"),
        }
    }

    #[test]
    fn stems_fold_case_separators_extensions_and_revisions() {
        for (name, stem) in [
            ("box.prt", "box"),
            ("Box.SLDPRT", "box"),
            ("box.stp", "box"),
            ("nist_mtc_crada_box_rev-D.SLDPRT", "nist mtc crada box"),
            ("NIST mtc crada box.prt", "nist mtc crada box"),
            ("cover_rev_B.SAT", "cover"),
            ("cover rev-A.stl", "cover"),
            ("cover-revB.stl", "cover"),
            ("part.prt.5", "part"),
            ("rev.stl", "rev"),
            ("Review Rig.stl", "review rig"),
            ("noextension", "noextension"),
        ] {
            assert_eq!(normalise_stem(name), stem, "{name}");
        }
    }

    #[test]
    fn one_model_in_several_formats_is_picked_by_rank() {
        let files = vec![
            file("part.SLDPRT", "solidworks-part"),
            file("part.prt", "nx-part"),
            file("part.stl", "stl"),
        ];
        let hints = Hints::default();
        assert_eq!(picked(&files, None, &hints), "auto part.stl +2");
        let r = resolve(&files, None, "", &hints);
        let i = info(&r, false);
        assert_eq!(i.choice, "automatic");
        assert_eq!(i.other_formats, 2);
        assert!(i.note().contains("2 other formats of the same model"));
        // a single file: no other formats
        assert_eq!(picked(&files[..1], None, &hints), "auto part.SLDPRT +0");
    }

    #[test]
    fn the_preferred_cad_program_breaks_ties_between_native_formats() {
        let files = vec![
            file("part.prt", "nx-part"),
            file("part.SLDPRT", "solidworks-part"),
        ];
        let hints = Hints::default();
        let first = |preferred: &str| {
            resolve(&files, None, preferred, &hints)
                .file()
                .map(|f| f.filename.clone())
        };
        assert_eq!(first("").as_deref(), Some("part.prt"));
        assert_eq!(first("solidworks").as_deref(), Some("part.SLDPRT"));
        // a mesh still wins over a preferred native file
        let mut with_mesh = files.clone();
        with_mesh.push(file("part.stl", "stl"));
        assert_eq!(
            resolve(&with_mesh, None, "solidworks", &hints)
                .file()
                .map(|f| f.filename.clone())
                .as_deref(),
            Some("part.stl")
        );
    }

    #[test]
    fn files_of_several_parts_are_ambiguous_until_one_is_chosen() {
        let files = vec![
            file("fin.stl", "stl"),
            file("body.stl", "stl"),
            file("body.prt", "nx-part"),
        ];
        let hints = Hints::default();
        assert_eq!(picked(&files, None, &hints), "ambiguous 2/3");
        let chosen = files[0].id;
        assert_eq!(picked(&files, Some(chosen), &hints), "chosen fin.stl");
        // a stale choice (file gone) is ignored
        assert_eq!(
            picked(&files, Some(Uuid::new_v4()), &hints),
            "ambiguous 2/3"
        );
        assert_eq!(picked(&[], None, &hints), "nothing");
        let r = resolve(&files, None, "", &hints);
        let i = info(&r, false);
        assert!(i.is_ambiguous() && i.file.is_none() && i.likely_file.is_none());
        assert_eq!(i.candidates.len(), 2);
        assert!(i
            .ambiguity_warning()
            .starts_with("3 files of 2 different parts"));
    }

    #[test]
    fn the_root_of_the_file_references_is_the_likely_assembly() {
        let files = vec![
            file("asm.prt", "nx-part"),
            file("asm.jt", "jt"),
            file("a.prt", "nx-part"),
            file("b.prt", "nx-part"),
        ];
        let ids: Vec<(Uuid, String)> = files.iter().map(|f| (f.id, f.filename.clone())).collect();
        let refs = vec![
            (files[0].id, "A.PRT".to_string()),
            (files[0].id, "b.prt".to_string()),
            (files[0].id, "elsewhere.prt".to_string()),
            (files[2].id, "a.prt".to_string()), // itself
        ];
        let roots = reference_roots(&ids, &refs);
        assert_eq!(roots, HashSet::from([files[0].id]));
        let hints = Hints {
            roots: Some(&roots),
            ..Hints::default()
        };
        let r = resolve(&files, None, "", &hints);
        // the neutral file of the assembly's model stands for it
        assert_eq!(r.likely.map(|f| f.filename.as_str()), Some("asm.jt"));
        assert_eq!(r.groups[0].stem, "asm");
        let i = info(&r, false);
        assert!(i.candidates[0].files.iter().any(|c| c.likely));
        assert!(i
            .ambiguity_warning()
            .contains("likely the whole assembly: asm.jt"));
        // two roots: no suggestion
        let two = HashSet::from([files[0].id, files[2].id]);
        let hints = Hints {
            roots: Some(&two),
            ..Hints::default()
        };
        assert!(resolve(&files, None, "", &hints).likely.is_none());
    }

    #[test]
    fn a_file_named_like_the_item_is_the_likely_assembly() {
        let files = vec![file("Rover Frame.step", "step"), file("wheel.stl", "stl")];
        let by_name = Hints {
            item_number: "ASM-1",
            item_name: "Rover frame",
            roots: None,
        };
        assert_eq!(
            resolve(&files, None, "", &by_name)
                .likely
                .map(|f| f.filename.as_str()),
            Some("Rover Frame.step")
        );
        // too short to count
        let short = Hints {
            item_number: "A1",
            item_name: "",
            roots: None,
        };
        assert!(resolve(&files, None, "", &short).likely.is_none());
    }

    /// The file names of the NIST MTC assembly fixture (`tests/fixtures/cad`)
    /// attached to one revision, as the issue describes it.
    fn nist() -> Vec<GeometryFile> {
        let nx = "nx-part";
        let sw = "solidworks-part";
        vec![
            file("90591A141 HEX NUT.prt", nx),
            file("91274A118 SHCS.prt", nx),
            file("91274A141 SHCS.prt", nx),
            file("91304A112 FHSCS.prt", nx),
            file("NIST mtc crada assembly.jt", "jt"),
            file("NIST mtc crada assembly.prt", nx),
            file("NIST mtc crada box.prt", nx),
            file("NIST mtc crada cover.prt", nx),
            file("NIST mtc crada plate.prt", nx),
            file("NIST mtc crada polycarb.prt", nx),
            file("90591A141_ZINC PLATED STEEL HEX NUT.SLDPRT", sw),
            file("nist_mtc_crada_box_rev-D.SLDPRT", sw),
            file("nist_mtc_crada_cover_rev-B.SLDPRT", sw),
            file("nist_mtc_crada_plate_rev-A.SLDPRT", sw),
            file("nist_mtc_crada_polycarb-window.SLDPRT", sw),
        ]
    }

    #[test]
    fn the_nist_assembly_in_one_revision_is_ambiguous_with_the_jt_suggested() {
        let files = nist();
        let hints = Hints {
            item_number: "PRT-000001",
            item_name: "NIST mtc crada assembly",
            roots: None,
        };
        let r = resolve(&files, None, "", &hints);
        assert!(r.is_ambiguous());
        assert!(r.file().is_none());
        assert_eq!(
            r.likely.map(|f| f.filename.as_str()),
            Some("NIST mtc crada assembly.jt")
        );
        // the NX and SOLIDWORKS parts of one part are one model
        let box_group = r
            .groups
            .iter()
            .find(|g| g.stem == "nist mtc crada box")
            .unwrap();
        assert_eq!(box_group.files.len(), 2);
        assert_eq!(box_group.files[0].format, "nx-part");
        assert_eq!(r.groups[0].stem, "nist mtc crada assembly");
        // the hex nut is no longer picked behind the user's back
        assert_ne!(
            resolve(&files, None, "", &Hints::default())
                .file()
                .map(|f| f.filename.as_str()),
            Some("90591A141 HEX NUT.prt")
        );
    }
}
