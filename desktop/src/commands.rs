//! Commands the user interface calls through `window.__TAURI__.core.invoke`.
//! Every network access and every file system access of the app is here.

use std::{path::PathBuf, sync::Arc};

use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::Notify;

use crate::{
    api::{self, Api, ApiError},
    cadexport, folderimport,
    settings::{self, Settings},
    signin, workspace, AppState,
};

type CmdResult<T> = Result<T, ApiError>;

fn snapshot(state: &State<'_, AppState>) -> Settings {
    state.settings.lock().expect("settings lock").clone()
}

/// The API client for the configured server (and token, if signed in).
fn api_of(state: &State<'_, AppState>) -> CmdResult<Api> {
    let s = snapshot(state);
    let base = s
        .server_url
        .ok_or_else(|| ApiError::local("No TeamCraft server configured."))?;
    Ok(Api {
        client: state.client.clone(),
        base,
        token: s.token,
    })
}

fn signed_in_api(state: &State<'_, AppState>) -> CmdResult<Api> {
    let api = api_of(state)?;
    if api.token.is_none() {
        return Err(ApiError {
            status: 401,
            message: "Please sign in.".into(),
        });
    }
    Ok(api)
}

fn workspace_root(app: &AppHandle, state: &State<'_, AppState>) -> PathBuf {
    snapshot(state).workspace_dir.unwrap_or_else(|| {
        app.path()
            .document_dir()
            .or_else(|_| app.path().home_dir())
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("TeamCraft")
    })
}

fn update_settings(
    state: &State<'_, AppState>,
    change: impl FnOnce(&mut Settings),
) -> CmdResult<Settings> {
    let mut s = state.settings.lock().expect("settings lock");
    change(&mut s);
    s.save(&state.config_dir)
        .map_err(|e| ApiError::local(format!("Settings could not be saved: {e}")))?;
    Ok(s.clone())
}

#[derive(Serialize)]
pub struct Session {
    pub server_url: Option<String>,
    pub signed_in: bool,
    pub skin: String,
    pub workspace_dir: PathBuf,
    pub version: &'static str,
    pub device: String,
    /// Zoom factor of the user interface.
    pub zoom: f64,
}

#[tauri::command]
pub fn session(app: AppHandle, state: State<'_, AppState>) -> Session {
    let s = snapshot(&state);
    Session {
        server_url: s.server_url.clone(),
        signed_in: s.token.is_some(),
        skin: s.skin.clone().unwrap_or_else(|| "teamcraft".into()),
        workspace_dir: workspace_root(&app, &state),
        version: env!("CARGO_PKG_VERSION"),
        device: crate::device_name(),
        zoom: s.zoom.unwrap_or(1.0),
    }
}

/// Checks that the address is a TeamCraft server and stores it.
#[tauri::command]
pub async fn set_server(state: State<'_, AppState>, url: String) -> CmdResult<String> {
    let base = settings::normalize_server_url(&url).map_err(ApiError::local)?;
    let api = Api {
        client: state.client.clone(),
        base: base.clone(),
        token: None,
    };
    let index = api
        .json(Method::GET, "", None)
        .await
        .map_err(|e| ApiError {
            status: e.status,
            message: format!("{base} does not answer like a TeamCraft server: {e}"),
        })?;
    if index["name"] != "TeamCraft REST API" {
        return Err(ApiError::local(format!(
            "{base} is not a TeamCraft server."
        )));
    }
    let changed = snapshot(&state).server_url.as_deref() != Some(base.as_str());
    let mut s = update_settings(&state, |s| s.server_url = Some(base.clone()))?;
    if changed {
        // a token belongs to its server
        s.save_token(&state.config_dir, None)
            .map_err(|e| ApiError::local(e.to_string()))?;
        state.settings.lock().expect("settings lock").token = None;
    }
    Ok(base)
}

fn store_token(state: &State<'_, AppState>, token: String) -> CmdResult<()> {
    let mut s = state.settings.lock().expect("settings lock");
    s.save_token(&state.config_dir, Some(token))
        .map_err(|e| ApiError::local(format!("The sign-in could not be stored: {e}")))
}

/// Sign-in through the browser (single sign-on).
#[tauri::command]
pub async fn sign_in_browser(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Value> {
    let api = api_of(&state)?;
    // A fresh cancel signal per attempt: `notify_one` keeps a permit, so a
    // cancel between two steps (or before the wait started) is not lost, and
    // an old cancel never affects the next attempt.
    let cancel = Arc::new(Notify::new());
    *state.sign_in_cancel.lock().expect("sign-in lock") = Some(cancel.clone());
    let flow = async {
        let attempt = signin::prepare(&api.base, &crate::device_name()).await?;
        app.opener()
            .open_url(attempt.url.clone(), None::<&str>)
            .map_err(|e| ApiError::local(format!("The browser could not be opened: {e}")))?;
        let code = match signin::wait_for_callback(&attempt, &cancel).await? {
            signin::Callback::Code(code) => code,
            signin::Callback::Denied => {
                return Err(ApiError::local("The sign-in was cancelled in the browser."))
            }
        };
        let result = signin::exchange(&api, &code, &attempt.verifier).await?;
        let token = result["token"]
            .as_str()
            .ok_or_else(|| ApiError::local("The server sent no token."))?
            .to_string();
        Ok::<_, ApiError>((token, result["user"].clone()))
    };
    // cancelling drops the flow at whatever step it is in
    let outcome = tokio::select! {
        _ = cancel.notified() => Err(ApiError::local("Sign-in cancelled.")),
        result = flow => result,
    };
    state.sign_in_cancel.lock().expect("sign-in lock").take();
    let (token, user) = outcome?;
    store_token(&state, token)?;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_focus();
    }
    Ok(user)
}

/// Stops a browser sign-in immediately, whatever step it is in.
#[tauri::command]
pub fn cancel_sign_in(state: State<'_, AppState>) {
    if let Some(cancel) = state.sign_in_cancel.lock().expect("sign-in lock").take() {
        cancel.notify_one();
        // the waiting callback listener holds a second waiter
        cancel.notify_waiters();
    }
}

/// Sign-in with a personal access token from the account page.
#[tauri::command]
pub async fn sign_in_token(state: State<'_, AppState>, token: String) -> CmdResult<Value> {
    let token = token.trim().to_string();
    if !token.starts_with("tc_pat_") {
        return Err(ApiError::local(
            "Personal access tokens start with tc_pat_ – create one on your account page.",
        ));
    }
    let mut api = api_of(&state)?;
    api.token = Some(token.clone());
    let me = api.json(Method::GET, "/me", None).await?;
    store_token(&state, token)?;
    Ok(me)
}

#[tauri::command]
pub fn sign_out(state: State<'_, AppState>) -> CmdResult<()> {
    let mut s = state.settings.lock().expect("settings lock");
    s.save_token(&state.config_dir, None)
        .map_err(|e| ApiError::local(e.to_string()))
}

#[tauri::command]
pub fn set_skin(state: State<'_, AppState>, skin: String) -> CmdResult<()> {
    let skin: String = skin
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(32)
        .collect();
    update_settings(&state, |s| s.skin = Some(skin))?;
    Ok(())
}

#[tauri::command]
pub fn set_workspace_dir(state: State<'_, AppState>, dir: PathBuf) -> CmdResult<()> {
    if !dir.is_absolute() {
        return Err(ApiError::local("Choose an absolute folder."));
    }
    std::fs::create_dir_all(&dir)?;
    update_settings(&state, |s| s.workspace_dir = Some(dir))?;
    Ok(())
}

/// Generic JSON call: `api("GET", "/items?q=x", null)`.
#[tauri::command]
pub async fn api(
    state: State<'_, AppState>,
    method: String,
    path: String,
    body: Option<Value>,
) -> CmdResult<Value> {
    let method = match method.to_ascii_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        other => return Err(ApiError::local(format!("Unsupported method {other}."))),
    };
    signed_in_api(&state)?
        .json(method, &path, body.as_ref())
        .await
}

/// Content of a file (`kind` = `file` for the current version or `version`)
/// as raw bytes – for the 3D viewer.
#[tauri::command]
pub async fn file_bytes(
    state: State<'_, AppState>,
    kind: String,
    id: String,
) -> CmdResult<tauri::ipc::Response> {
    let id: String = id
        .chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect();
    let path = match kind.as_str() {
        "file" => format!("/files/{id}/content"),
        "version" => format!("/file-versions/{id}/content"),
        // 3D preview meshes, converted by the server
        "mesh" => format!("/files/{id}/mesh"),
        "version-mesh" => format!("/file-versions/{id}/mesh"),
        // preview image stored in Inventor / Solid Edge / SolidWorks files
        "thumbnail" => format!("/files/{id}/thumbnail"),
        // screenshots and PDFs attached to a change request
        "request-attachment" => format!("/change-request-attachments/{id}/content"),
        _ => return Err(ApiError::local("Unknown file kind.")),
    };
    let bytes = signed_in_api(&state)?.bytes(&path).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Asks for a target file with the native save dialog (from Rust, so the web
/// view never chooses paths itself). `None` = cancelled.
async fn ask_save_path(
    app: &AppHandle,
    suggested: &str,
    filter: (&str, &[&str]),
) -> CmdResult<Option<PathBuf>> {
    use tauri_plugin_dialog::DialogExt;
    let app = app.clone();
    let name = workspace::safe_name(suggested);
    let (label, extensions) = (
        filter.0.to_string(),
        filter.1.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
    );
    tokio::task::spawn_blocking(move || {
        let exts: Vec<&str> = extensions.iter().map(String::as_str).collect();
        app.dialog()
            .file()
            .set_file_name(&name)
            .add_filter(&label, &exts)
            .blocking_save_file()
            .and_then(|p| p.into_path().ok())
    })
    .await
    .map_err(|e| ApiError::local(format!("The save dialog failed: {e}")))
}

/// "Pack and Go": downloads the ZIP of a revision and its structure to a
/// place the user chooses. Returns the path, or `null` when cancelled.
#[tauri::command]
pub async fn save_package(
    app: AppHandle,
    state: State<'_, AppState>,
    item: String,
    code: String,
    rule: Option<String>,
) -> CmdResult<Option<PathBuf>> {
    let api = signed_in_api(&state)?;
    let rule = rule.filter(|r| matches!(r.as_str(), "latest" | "released" | "as_released"));
    let suggested = format!(
        "{}_{}{}.zip",
        item,
        code,
        rule.as_deref().map(|r| format!("_{r}")).unwrap_or_default()
    );
    let Some(target) = ask_save_path(&app, &suggested, ("ZIP archive", &["zip"])).await? else {
        return Ok(None);
    };
    let mut path = format!(
        "/items/{}/revisions/{}/package",
        workspace::enc(&item),
        workspace::enc(&code)
    );
    if let Some(r) = rule {
        path.push_str(&format!("?rule={r}"));
    }
    api.download_to(&path, &target).await?;
    Ok(Some(target))
}

/// Saves bytes from the web view (the STL export of the 3D viewer) to a file
/// the user chooses. The raw request body is the content; the header
/// `x-file-name` the suggested name. Returns the path, or `null`.
#[tauri::command]
pub async fn save_stl(
    app: AppHandle,
    request: tauri::ipc::Request<'_>,
) -> CmdResult<Option<PathBuf>> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err(ApiError::local("Expected the STL data as raw bytes."));
    };
    let name = request
        .headers()
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        // sent percent-encoded (encodeURIComponent)
        .map(|v| {
            url::form_urlencoded::parse(format!("n={v}").as_bytes())
                .map(|(_, v)| v.into_owned())
                .next()
                .unwrap_or_default()
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "model.stl".into());
    let bytes = bytes.clone();
    let Some(target) = ask_save_path(&app, &name, ("STL mesh", &["stl"])).await? else {
        return Ok(None);
    };
    tokio::fs::write(&target, bytes).await?;
    Ok(Some(target))
}

/// Check out: takes the lock on the server and fills the workspace folder.
#[tauri::command]
pub async fn workspace_checkout(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: String,
    code: String,
) -> CmdResult<workspace::Downloaded> {
    let api = signed_in_api(&state)?;
    api.json(
        Method::POST,
        &format!(
            "/items/{}/revisions/{}/checkout",
            workspace::enc(&item),
            workspace::enc(&code)
        ),
        None,
    )
    .await?;
    workspace::download_all(
        &api,
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
    )
    .await
}

/// Copies the files of any revision (e.g. released ones) into the workspace
/// without taking the lock.
#[tauri::command]
pub async fn workspace_download(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: String,
    code: String,
) -> CmdResult<workspace::Downloaded> {
    let api = signed_in_api(&state)?;
    workspace::download_all(
        &api,
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
    )
    .await
}

/// Local files of a revision compared with the server.
#[tauri::command]
pub async fn workspace_status(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: String,
    code: String,
) -> CmdResult<Value> {
    let api = signed_in_api(&state)?;
    let files = api
        .json(
            Method::GET,
            &format!(
                "/items/{}/revisions/{}/files",
                workspace::enc(&item),
                workspace::enc(&code)
            ),
            None,
        )
        .await?;
    let dir = workspace::revision_dir(
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
    );
    let files = workspace::status(&dir, &files).await?;
    Ok(json!({ "dir": dir, "exists": dir.is_dir(), "files": files }))
}

/// Check in the chosen workspace files (new versions) and release the lock.
#[tauri::command]
pub async fn workspace_checkin(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: String,
    code: String,
    files: Vec<String>,
    comment: String,
) -> CmdResult<Value> {
    let api = signed_in_api(&state)?;
    workspace::check_in(
        &api,
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
        &files,
        &comment,
    )
    .await
}

/// Opens a revision folder (or a file in it) with the system's default
/// application; without item and revision the workspace root.
#[tauri::command]
pub async fn workspace_open(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: Option<String>,
    code: Option<String>,
    file: Option<String>,
) -> CmdResult<()> {
    let root = workspace_root(&app, &state);
    let dir = match (item, code) {
        (Some(item), Some(code)) => {
            workspace::revision_dir(&root, project.as_deref(), &item, &code)
        }
        _ => root,
    };
    std::fs::create_dir_all(&dir)?;
    let target = match file {
        // Creo: open the newest iteration (bracket.prt.7) of bracket.prt.
        Some(name) if !name.contains(['/', '\\']) && name != ".." => {
            workspace::newest_local(&dir, workspace::logical_name(&name).0)
                .await?
                .unwrap_or_else(|| dir.join(&name))
        }
        Some(_) => return Err(ApiError::local("Invalid file name.")),
        None => dir,
    };
    app.opener()
        .open_path(target.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| ApiError::local(format!("Could not open {}: {e}", target.display())))
}

/// Opens a page of the web UI in the default browser (e.g. `/revisions/<id>`).
#[tauri::command]
pub fn open_web(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<()> {
    api::check_path(&path)?;
    let base = api_of(&state)?.base;
    app.opener()
        .open_url(format!("{base}{path}"), None::<&str>)
        .map_err(|e| ApiError::local(format!("The browser could not be opened: {e}")))
}

/// Smallest and largest zoom factor of the user interface.
pub const ZOOM_RANGE: (f64, f64) = (0.5, 2.5);

pub fn clamp_zoom(level: f64) -> f64 {
    if level.is_finite() {
        level.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1)
    } else {
        1.0
    }
}

/// Zooms the user interface (Ctrl + mouse wheel, Ctrl + +/-/0) and remembers
/// the factor for the next start.
#[tauri::command]
pub fn set_zoom(app: AppHandle, state: State<'_, AppState>, level: f64) -> CmdResult<f64> {
    let level = clamp_zoom(level);
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_zoom(level)
            .map_err(|e| ApiError::local(format!("Zoom failed: {e}")))?;
    }
    update_settings(&state, |s| s.zoom = Some(level))?;
    Ok(level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_is_clamped() {
        assert_eq!(clamp_zoom(1.2), 1.2);
        assert_eq!(clamp_zoom(0.1), ZOOM_RANGE.0);
        assert_eq!(clamp_zoom(9.0), ZOOM_RANGE.1);
        assert_eq!(clamp_zoom(f64::NAN), 1.0);
    }
}

/// Uploads local files into a revision the user has checked out: the files
/// of a new item, or of an item created by the folder import. Creo
/// iterations are uploaded under their logical name (`bracket.prt.7` →
/// `bracket.prt`).
#[tauri::command]
pub async fn upload_files(
    state: State<'_, AppState>,
    item: String,
    code: String,
    paths: Vec<PathBuf>,
    comment: Option<String>,
) -> CmdResult<Value> {
    let api = signed_in_api(&state)?;
    let files = upload_names(&paths)?;
    if files.is_empty() {
        return Ok(Value::Null);
    }
    api.multipart(
        &format!(
            "/items/{}/revisions/{}/files",
            workspace::enc(&item),
            workspace::enc(&code)
        ),
        &files,
        &[("comment", comment.unwrap_or_default())],
    )
    .await
}

/// Largest attachment of a change request (the server enforces the same).
const MAX_REQUEST_ATTACHMENT: u64 = 10 * 1024 * 1024;

/// Attaches a screenshot, mark-up or PDF (chosen in the native file dialog)
/// to a change request. The server accepts images and PDF only, decided by
/// the content.
#[tauri::command]
pub async fn upload_request_attachment(
    state: State<'_, AppState>,
    number: String,
    path: PathBuf,
) -> CmdResult<Value> {
    let api = signed_in_api(&state)?;
    let size = tokio::fs::metadata(&path).await?.len();
    if size > MAX_REQUEST_ATTACHMENT {
        return Err(ApiError::local(format!(
            "{} is larger than 10 MiB.",
            path.display()
        )));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ApiError::local(format!("{} has no usable file name.", path.display())))?
        .to_string();
    api.multipart(
        &format!("/change-requests/{}/attachments", workspace::enc(&number)),
        &[(name, path.clone())],
        &[],
    )
    .await
}

/// `(name on the server, local path)` for every path.
fn upload_names(paths: &[PathBuf]) -> CmdResult<Vec<(String, PathBuf)>> {
    paths
        .iter()
        .map(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
                ApiError::local(format!("{} has no usable file name.", path.display()))
            })?;
            let (logical, _) = workspace::logical_name(name);
            Ok((logical.to_string(), path.clone()))
        })
        .collect()
}

#[cfg(test)]
mod upload_tests {
    use super::*;

    #[test]
    fn upload_names_drop_creo_iterations() {
        let names = upload_names(&[
            PathBuf::from("/cad/frame.asm.12"),
            PathBuf::from("/cad/Bracket.ipt"),
        ])
        .unwrap();
        assert_eq!(names[0].0, "frame.asm");
        assert_eq!(names[1].0, "Bracket.ipt");
    }
}

/// The CAD programs on this computer that can export STEP for the 3D
/// preview (Windows: Inventor, Solid Edge; elsewhere none).
#[tauri::command]
pub async fn cad_programs() -> Vec<cadexport::CadProgram> {
    tokio::task::spawn_blocking(cadexport::available)
        .await
        .unwrap_or_default()
}

/// Exports a workspace file of a revision to STEP with the local Inventor or
/// Solid Edge; the STEP file lands next to it (and is checked in with it).
/// Returns the name of the STEP file.
#[tauri::command]
pub async fn cad_export_step(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: String,
    code: String,
    file: String,
) -> CmdResult<String> {
    if file.contains(['/', '\\']) || file == ".." || file.is_empty() {
        return Err(ApiError::local(format!("'{file}' is not a file name.")));
    }
    let dir = workspace::revision_dir(
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
    );
    let input = dir.join(&file);
    if !input.is_file() {
        return Err(ApiError::local(format!(
            "{file} is not in the workspace – check out or use “Get files” first."
        )));
    }
    let output = tokio::task::spawn_blocking(move || cadexport::export_step(&input))
        .await
        .map_err(|e| ApiError::local(format!("The export stopped: {e}")))?
        .map_err(ApiError::local)?;
    Ok(output
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string())
}

/// Scans a CAD folder for the import wizard (nothing is uploaded).
#[tauri::command]
pub async fn scan_folder(path: PathBuf) -> CmdResult<folderimport::Scan> {
    tokio::task::spawn_blocking(move || folderimport::scan(&path))
        .await
        .map_err(|e| ApiError::local(format!("The scan stopped: {e}")))?
        .map_err(ApiError::local)
}
