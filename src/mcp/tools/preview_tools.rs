//! `open_3d_preview`: finds the item the user means and the file of its
//! revision that the browser's 3D viewer can show, and answers with the
//! viewer page and an action hint (`{"action":"open","kind":"preview3d",
//! "url":…}`). MCP is stateless, so the tool opens nothing itself: an MCP
//! client shows the link, the built-in assistant a button (see
//! `crate::assistant::open_links`). Only reads, like the other read tools
//! without an audit record.

use uuid::Uuid;

use super::*;
use crate::preview::PreviewFormat;

/// Most candidates listed when the search text matches several items.
pub const MAX_PREVIEW_CANDIDATES: usize = 8;
/// Other 3D files of the revision named next to the chosen one.
const MAX_OTHER_FILES: usize = 20;
/// Files named when the revision has no 3D file.
const MAX_LISTED_FILES: usize = 20;

/// What the arguments ask for.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// An item number, with the revision code of `PRT-000012/B` or `revision`.
    Item {
        number: String,
        revision: Option<String>,
    },
    /// Free text for the item search.
    Search {
        query: String,
        revision: Option<String>,
    },
}

/// Reads `item`, `q` and `revision`. `item` wins over `q`; a revision code
/// in `item` and a different `revision` contradict each other.
pub(super) fn target(a: &Args<'_>) -> AppResult<Target> {
    let revision = a.opt_str("revision")?;
    if let Some(reference) = a.opt_str("item")? {
        let (number, code) = parse_item_ref(&reference);
        if number.is_empty() {
            return Err(bad("`item` must be an item number, e.g. PRT-000012."));
        }
        let revision = match (code, revision) {
            (Some(c), Some(r)) if !c.eq_ignore_ascii_case(&r) => {
                return Err(bad(format!(
                    "`item` names revision {c} but `revision` is {r} – give one."
                )))
            }
            (Some(c), _) => Some(c),
            (None, r) => r,
        };
        return Ok(Target::Item { number, revision });
    }
    match a.opt_str("q")? {
        Some(query) => Ok(Target::Search { query, revision }),
        None => Err(bad(
            "Give `item` (an item number) or `q` (search text, e.g. a name like `bracket`).",
        )),
    }
}

/// The 3D preview of a stored file: by its stored format, or by its name
/// while the format is not classified yet (as the revision page decides
/// whether to show "3D view").
pub(super) fn preview_format(format_id: &str, filename: &str) -> Option<PreviewFormat> {
    PreviewFormat::of_format(format_id).or_else(|| PreviewFormat::of(filename))
}

/// Which 3D file is opened first: models (STEP first, as the shape
/// comparison does), then simulation results, then point clouds.
pub(super) fn preview_rank(format: PreviewFormat) -> u8 {
    if format.is_point_cloud() {
        3
    } else if format.is_result() {
        2
    } else if format.is_brep() {
        0
    } else {
        1
    }
}

/// The 3D files of a revision, the one to open first at the front (ties by
/// file name).
fn ranked(files: &[FileRecord]) -> Vec<(&FileRecord, PreviewFormat)> {
    let mut list: Vec<(&FileRecord, PreviewFormat)> = files
        .iter()
        .filter_map(|f| preview_format(&f.format, &f.filename).map(|p| (f, p)))
        .collect();
    list.sort_by(|(fa, pa), (fb, pb)| {
        preview_rank(*pa)
            .cmp(&preview_rank(*pb))
            .then_with(|| fa.filename.to_lowercase().cmp(&fb.filename.to_lowercase()))
    });
    list
}

/// The path of the 3D viewer page of a file (the current version).
pub fn viewer_path(file_id: Uuid) -> String {
    format!("/files/{file_id}/preview")
}

/// The action hint of a viewer URL.
pub fn preview_action(url: &str) -> Value {
    json!({ "action": "open", "kind": "preview3d", "url": url })
}

/// Checks a viewer URL from a tool result before anything shows it: it must
/// be this server's 3D viewer page of a file – a relative `/files/<uuid>/preview`
/// or the same under `public_url` (same scheme, host and port). Returns the
/// relative path in canonical form; anything else (other hosts, `//host`,
/// queries, fragments, encodings, other pages) is `None`.
pub fn checked_viewer_path(url: &str, public_url: &str) -> Option<String> {
    let url = url.trim();
    let public_url = public_url.trim().trim_end_matches('/');
    let path = if url.starts_with('/') {
        url
    } else if !public_url.is_empty() {
        url.strip_prefix(public_url)
            .filter(|rest| rest.starts_with('/'))?
    } else {
        return None;
    };
    let id = path.strip_prefix("/files/")?.strip_suffix("/preview")?;
    // exactly a hyphenated UUID: no slashes, dots, `%`, `?` or `#`
    if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return None;
    }
    let id = Uuid::parse_str(id).ok()?;
    Some(viewer_path(id))
}

/// `open_3d_preview`.
pub(super) async fn open_3d_preview(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let file_name = a.opt_str("file")?;
    let rev = match target(a)? {
        Target::Item { number, revision } => {
            revision_at(st, me, &number, revision.as_deref()).await?
        }
        Target::Search { query, revision } => match search_target(st, me, a, &query).await? {
            Found::One(number) => revision_at(st, me, &number, revision.as_deref()).await?,
            Found::Choose(answer) => return Ok(answer),
        },
    };
    let file_list = files::list(&st.db, me, rev.id).await?;
    let usable = ranked(&file_list);
    let chosen = match &file_name {
        Some(name) => {
            let file = file_list
                .iter()
                .find(|f| f.filename.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    AppError::not_found(format!(
                        "{}/{} has no file '{name}'.",
                        rev.item_number, rev.code
                    ))
                })?;
            let format = preview_format(&file.format, &file.filename).ok_or_else(|| {
                bad(format!(
                    "'{}' has no 3D preview. Viewable are STEP, STL, OBJ, PLY, 3MF, glTF, JT, NX and \
                     SOLIDWORKS parts, Rhino, point clouds and VTK/FRD results.",
                    file.filename
                ))
            })?;
            Some((file, format))
        }
        None => usable.first().copied(),
    };
    let Some((file, format)) = chosen else {
        let (listed, cut) = capped(file_list.iter().collect::<Vec<_>>(), MAX_LISTED_FILES);
        return Ok(json!({
            "status": "no_3d_file",
            "item": rev.item_number,
            "revision": rev.code,
            "name": rev.name,
            "message": format!(
                "{}/{} has no file the 3D viewer can show. Native CAD files without a stored \
                 lightweight model need a neutral export (STEP, STL, 3MF …) in the revision. Tell \
                 the user; do not open anything.",
                rev.item_number, rev.code
            ),
            "files": listed.iter().map(|f| json!({
                "filename": f.filename,
                "format": f.format_info.as_ref().map(|i| i.label),
            })).collect::<Vec<_>>(),
            "truncated": cut,
            "revision_url": web_url(st, &format!("/revisions/{}", rev.id)),
        }));
    };
    let url = web_url(st, &viewer_path(file.id));
    let (others, others_cut) = capped(
        usable
            .iter()
            .filter(|(f, _)| f.id != file.id)
            .map(|(f, p)| json!({ "filename": f.filename, "format": p.as_str() }))
            .collect::<Vec<_>>(),
        MAX_OTHER_FILES,
    );
    Ok(json!({
        "status": "ok",
        "item": rev.item_number,
        "revision": rev.code,
        "name": rev.name,
        "state": rev.state,
        "file": {
            "filename": file.filename,
            "format": format.as_str(),
            "version": file.version,
        },
        "url": url,
        "action": preview_action(&url),
        "other_3d_files": others,
        "truncated": others_cut,
        "revision_url": web_url(st, &format!("/revisions/{}", rev.id)),
        "message": "Give the user this link to the 3D viewer (TeamCraft's own assistant shows it as a \
            button). Another file: call again with `file`.",
    }))
}

enum Found {
    One(String),
    Choose(Value),
}

/// The item a search text means: the only hit, or the hit whose number is
/// the text; several hits are returned as candidates to choose from.
async fn search_target(st: &AppState, me: &Actor, a: &Args<'_>, query: &str) -> AppResult<Found> {
    let project_id = match a.opt_str("project")? {
        Some(code) => Some(projects::get_by_code(&st.db, me, &code).await?.id),
        None => None,
    };
    let filter = items::ItemFilter {
        query: Some(query.to_string()),
        project_id,
        ..Default::default()
    };
    let (rows, total) = items::search(
        &st.db,
        me,
        &filter,
        Page::new(Some(MAX_PREVIEW_CANDIDATES as i64), None),
    )
    .await?;
    if let Some(exact) = rows
        .iter()
        .find(|r| r.number.eq_ignore_ascii_case(query.trim()))
    {
        return Ok(Found::One(exact.number.clone()));
    }
    match rows.as_slice() {
        [] => Err(AppError::not_found(format!(
            "No item matches '{query}'. Try a shorter search text (a key word of the name, e.g. \
             `bracket`) or search_items."
        ))),
        [only] if total == 1 => Ok(Found::One(only.number.clone())),
        _ => {
            let mut candidates = Vec::with_capacity(rows.len());
            for r in &rows {
                let file_list = files::list(&st.db, me, r.latest_revision_id).await?;
                let first = ranked(&file_list).first().map(|(f, _)| f.filename.clone());
                candidates.push(json!({
                    "item": r.number,
                    "name": r.name,
                    "type": r.item_type,
                    "project": r.project_code,
                    "latest_revision": r.latest_code,
                    "latest_state": r.latest_state,
                    "has_3d": first.is_some(),
                    "3d_file": first,
                }));
            }
            Ok(Found::Choose(json!({
                "status": "choose",
                "query": query,
                "message": format!(
                    "{total} items match '{query}'. Do not guess: ask the user which one they mean \
                     (or narrow the search), then call again with `item`."
                ),
                "total": total,
                "truncated": total > rows.len() as i64,
                "candidates": candidates,
            })))
        }
    }
}
