//! Commands the user interface calls through `window.__TAURI__.core.invoke`.
//! Every network access and every file system access of the app is here.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::Notify;

use crate::{
    acp,
    api::{self, Api, ApiError, Transfer},
    bomimport, cadexport, folderimport, layout,
    settings::{self, Settings, TokenStorage},
    signin, update, workspace, AppState,
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
    /// Where the token is kept: `file` makes the UI warn (no OS keychain).
    pub token_storage: Option<TokenStorage>,
    /// The daily check for a newer release is switched on.
    pub update_check: bool,
}

#[tauri::command]
pub fn session(app: AppHandle, state: State<'_, AppState>) -> Session {
    let s = snapshot(&state);
    Session {
        token_storage: s.token.as_ref().and(s.token_storage),
        update_check: s.update_check != Some(false),
        server_url: s.server_url.clone(),
        signed_in: s.token.is_some(),
        skin: s.skin.clone().unwrap_or_else(|| "teamcraft".into()),
        workspace_dir: workspace_root(&app, &state),
        version: env!("CARGO_PKG_VERSION"),
        device: crate::device_name(),
        zoom: s.zoom.unwrap_or(1.0),
    }
}

/// Would this address (as typed) reach another computer over plain
/// `http://`? The connect dialog warns while the user types.
#[tauri::command]
pub fn server_url_insecure(url: String) -> bool {
    settings::normalize_server_url(&url).is_ok_and(|base| settings::insecure_http(&base))
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
    // the agent works with the token that is about to go
    stop_agent(&state);
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

/// A question in the assistant sidebar (`POST /assistant`, in the user's
/// conversation). Unlike [`api`] it can be cancelled with
/// [`assistant_cancel`]: the request is dropped, so the server stops the run
/// and stores nothing.
#[tauri::command]
pub async fn assistant_send(state: State<'_, AppState>, body: Value) -> CmdResult<Value> {
    let api = signed_in_api(&state)?;
    let cancel = Arc::new(Notify::new());
    *state.assistant_cancel.lock().expect("assistant lock") = Some(cancel.clone());
    let result = tokio::select! {
        answer = api.json(Method::POST, "/assistant", Some(&body)) => answer,
        _ = cancel.notified() => Err(crate::abortable::cancelled("Cancelled.")),
    };
    state
        .assistant_cancel
        .lock()
        .expect("assistant lock")
        .take();
    result
}

/// Stops the running assistant question, if any.
#[tauri::command]
pub fn assistant_cancel(state: State<'_, AppState>) {
    if let Some(cancel) = state
        .assistant_cancel
        .lock()
        .expect("assistant lock")
        .take()
    {
        cancel.notify_one();
    }
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
    // a fresh cancel switch for this download (`cancel_package` flips it);
    // `package-progress` events follow it: {received, total?}
    let cancel = Arc::new(AtomicBool::new(false));
    *state.package_cancel.lock().expect("package lock") = cancel.clone();
    let events = app.clone();
    let on_bytes = move |received: u64, total: Option<u64>| {
        let _ = events.emit(
            "package-progress",
            json!({ "received": received, "total": total }),
        );
    };
    api.download_to(&path, &target, &cancel, &on_bytes).await?;
    Ok(Some(target))
}

/// Stops the running Pack and Go download (also while the server is still
/// packing); the partial ZIP is removed and nothing is written.
#[tauri::command]
pub fn cancel_package(state: State<'_, AppState>) {
    state
        .package_cancel
        .lock()
        .expect("package lock")
        .store(true, Ordering::Relaxed);
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

/// A progress reporter for Gets: `workspace-progress` events to the user
/// interface. Every Get starts with a fresh cancel flag.
fn progress_events(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> (
    Arc<AtomicBool>,
    impl FnMut(&workspace::Progress) + Send + 'static,
) {
    state.get_cancel.store(false, Ordering::Relaxed);
    let app = app.clone();
    (state.get_cancel.clone(), move |p: &workspace::Progress| {
        let _ = app.emit("workspace-progress", p);
    })
}

/// Check out: takes the lock on the server and copies the files into the
/// working folder; the files of the revision become writable.
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
    let (cancel, mut progress) = progress_events(&app, &state);
    workspace::download_revision(
        &api,
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
        &cancel,
        &mut progress,
    )
    .await
}

/// Copies the current files of a revision into the working folder without
/// taking the lock (read-only on disk).
#[tauri::command]
pub async fn workspace_download(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: String,
    code: String,
) -> CmdResult<workspace::Downloaded> {
    let api = signed_in_api(&state)?;
    let (cancel, mut progress) = progress_events(&app, &state);
    workspace::download_revision(
        &api,
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
        &cancel,
        &mut progress,
    )
    .await
}

/// "Get": a revision with its structure (BOM under the revision rule
/// `latest` or `released`) and the CAD files it references – only missing
/// and outdated files are downloaded, with progress events and cancel.
#[tauri::command]
pub async fn workspace_get(
    app: AppHandle,
    state: State<'_, AppState>,
    item: String,
    code: String,
    rule: Option<String>,
) -> CmdResult<workspace::Downloaded> {
    let api = signed_in_api(&state)?;
    let (cancel, mut progress) = progress_events(&app, &state);
    workspace::get_structure(
        &api,
        &workspace_root(&app, &state),
        &item,
        &code,
        rule.as_deref().unwrap_or("latest"),
        &cancel,
        &mut progress,
    )
    .await
}

/// Stops the running Get after the current chunk; the files copied so far
/// stay (the next Get continues with the rest).
#[tauri::command]
pub fn workspace_cancel(state: State<'_, AppState>) {
    state.get_cancel.store(true, Ordering::Relaxed);
}

/// The files of a revision in the working folder compared with the server.
#[tauri::command]
pub async fn workspace_status(
    app: AppHandle,
    state: State<'_, AppState>,
    project: Option<String>,
    item: String,
    code: String,
) -> CmdResult<workspace::Status> {
    let api = signed_in_api(&state)?;
    workspace::status(
        &api,
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
    )
    .await
}

/// Check in the chosen files (new versions) and release the lock.
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
    let root = workspace_root(&app, &state);
    let cancel = new_upload(&state);
    let progress = upload_progress_events(app);
    let transfer = Transfer {
        cancel: &cancel,
        progress: &progress,
    };
    workspace::check_in(
        &api,
        &root,
        project.as_deref(),
        &item,
        &code,
        &files,
        &comment,
        &transfer,
    )
    .await
}

/// A fresh cancel switch for the upload that starts now (`cancel_upload`
/// flips the switch of the running one).
fn new_upload(state: &State<'_, AppState>) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    *state.upload_cancel.lock().expect("upload lock") = cancel.clone();
    cancel
}

/// Reports the progress of large uploads to the user interface
/// (event `upload-progress`: `{file, sent, total}`; Gets use
/// [`progress_events`]).
fn upload_progress_events(app: AppHandle) -> impl Fn(&str, u64, u64) + Send + Sync {
    move |file: &str, sent: u64, total: u64| {
        let _ = app.emit(
            "upload-progress",
            json!({ "file": file, "sent": sent, "total": total }),
        );
    }
}

/// Stops the running upload before its next chunk; chunks already on the
/// server are discarded and nothing is stored.
#[tauri::command]
pub fn cancel_upload(state: State<'_, AppState>) {
    state
        .upload_cancel
        .lock()
        .expect("upload lock")
        .store(true, Ordering::Relaxed);
}

/// Opens the folder of a revision (or a file of it) with the system's
/// default application; without item and revision the workspace root.
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
    let target = match (item, code) {
        (Some(item), Some(code)) => {
            let api = signed_in_api(&state)?;
            match file {
                // Creo: the newest iteration (bracket.prt.7) of bracket.prt.
                Some(name) => {
                    let (dir, found) =
                        workspace::locate(&api, &root, project.as_deref(), &item, &code, &name)
                            .await?;
                    found.unwrap_or(dir)
                }
                None => {
                    let rev =
                        workspace::revision_files(&api, project.as_deref(), &item, &code).await?;
                    workspace::main_dir(&root, project.as_deref(), &rev.files)
                }
            }
        }
        _ => root,
    };
    if !target.exists() {
        std::fs::create_dir_all(&target)?;
    }
    app.opener()
        .open_path(target.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| ApiError::local(format!("Could not open {}: {e}", target.display())))
}

/// Folders of the old layout `<group>/<item>/<revision>/` and what moving
/// them into the working-folder layout would do (nothing is changed).
#[tauri::command]
pub async fn workspace_migration_plan(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<workspace::MigrationPlan> {
    let api = signed_in_api(&state)?;
    workspace::migration_plan(&api, &workspace_root(&app, &state)).await
}

/// Moves the old layout into the working-folder layout (never overwriting
/// anything; every step in `.teamcraft/migration.log`).
#[tauri::command]
pub async fn workspace_migrate(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<workspace::MigrationPlan> {
    let api = signed_in_api(&state)?;
    workspace::migrate(&api, &workspace_root(&app, &state)).await
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

/// A newer release than this app (the update notice).
#[derive(Serialize)]
pub struct UpdateNotice {
    pub version: String,
    pub current: &'static str,
}

/// Looks for a newer release – on GitHub at most once a day, otherwise from
/// the last answer – unless the user switched the check off. Failures (no
/// network, rate limit) show no notice; the next try is a day later.
#[tauri::command]
pub async fn check_update(state: State<'_, AppState>) -> CmdResult<Option<UpdateNotice>> {
    let s = snapshot(&state);
    if s.update_check == Some(false) {
        return Ok(None);
    }
    let mut latest = s.update_latest.clone();
    let now = update::now_secs();
    if update::due(s.update_checked_at, now) {
        // stamped first: a window reload meanwhile does not ask again
        update_settings(&state, |s| s.update_checked_at = Some(now))?;
        if let Ok(found) = update::latest_release().await {
            latest = found.clone();
            update_settings(&state, |s| s.update_latest = found)?;
        }
    }
    let current = env!("CARGO_PKG_VERSION");
    Ok(latest
        .filter(|v| update::is_newer(v, current) && s.update_dismissed.as_ref() != Some(v))
        .map(|version| UpdateNotice { version, current }))
}

/// Hides the notice of this release version (a newer one shows again).
#[tauri::command]
pub fn dismiss_update(state: State<'_, AppState>, version: String) -> CmdResult<()> {
    let version = update::tag_version(&version)
        .ok_or_else(|| ApiError::local(format!("'{version}' is not a version.")))?;
    update_settings(&state, |s| s.update_dismissed = Some(version))?;
    Ok(())
}

/// Switches the daily release check on or off.
#[tauri::command]
pub fn set_update_check(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    update_settings(&state, |s| s.update_check = Some(enabled))?;
    Ok(())
}

/// Opens the release page in the default browser (a fixed address – the app
/// itself never downloads or installs anything).
#[tauri::command]
pub fn open_release_page(app: AppHandle) -> CmdResult<()> {
    app.opener()
        .open_url(update::RELEASES_PAGE, None::<&str>)
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
/// `bracket.prt`). `folders` (file name → relative folder, from the folder
/// import) gives every file its place in the project group, so the working
/// folder later mirrors the original folder structure.
#[tauri::command]
pub async fn upload_files(
    app: AppHandle,
    state: State<'_, AppState>,
    item: String,
    code: String,
    paths: Vec<PathBuf>,
    comment: Option<String>,
    folders: Option<HashMap<String, String>>,
) -> CmdResult<Value> {
    let api = signed_in_api(&state)?;
    let files = upload_names(&paths)?;
    if files.is_empty() {
        return Ok(Value::Null);
    }
    let folders: serde_json::Map<String, Value> = folders
        .unwrap_or_default()
        .into_iter()
        .map(|(name, folder)| (name, Value::String(layout::folder_for_upload(&folder))))
        .collect();
    let cancel = new_upload(&state);
    let progress = upload_progress_events(app);
    let transfer = Transfer {
        cancel: &cancel,
        progress: &progress,
    };
    api.send_files(
        &format!(
            "/items/{}/revisions/{}/files",
            workspace::enc(&item),
            workspace::enc(&code)
        ),
        &item,
        &code,
        &files,
        &[
            ("comment", comment.unwrap_or_default()),
            ("folders", Value::Object(folders).to_string()),
        ],
        &transfer,
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

/// *Import BOM…*: asks for the BOM table or STEP assembly with the native
/// open dialog (from Rust, so the web view never chooses paths itself).
/// `None` = cancelled.
#[tauri::command]
pub async fn pick_bom_file(app: AppHandle) -> CmdResult<Option<PathBuf>> {
    use tauri_plugin_dialog::DialogExt;
    tokio::task::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("BOM to import")
            .add_filter("BOM tables and STEP assemblies", bomimport::EXTENSIONS)
            .blocking_pick_file()
            .and_then(|p| p.into_path().ok())
    })
    .await
    .map_err(|e| ApiError::local(format!("The file dialog failed: {e}")))
}

/// *Import BOM…*: sends the chosen file to the server's BOM import – with
/// `dry_run` for the preview (the plan), then for real.
#[tauri::command]
pub async fn import_bom(
    state: State<'_, AppState>,
    path: PathBuf,
    request: bomimport::Request,
    dry_run: bool,
) -> CmdResult<Value> {
    let api = signed_in_api(&state)?;
    bomimport::send(&api, &path, &request, dry_run).await
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
    let api = signed_in_api(&state)?;
    let (_, found) = workspace::locate(
        &api,
        &workspace_root(&app, &state),
        project.as_deref(),
        &item,
        &code,
        &file,
    )
    .await?;
    let Some(input) = found.filter(|p| p.is_file()) else {
        return Err(ApiError::local(format!(
            "{file} is not in the working folder – check out or use “Get files” first."
        )));
    };
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

// ---------------------------------------------------------------- ACP agents

/// The agent session the desktop runs (at most one).
pub struct RunningAgent {
    /// Number of the session; its events carry it.
    pub session: u64,
    /// Name of the agent in the settings.
    pub name: String,
    pub agent: Arc<acp::Session>,
}

/// Events of an agent session go to the window as `agent` events.
struct AgentSink {
    app: AppHandle,
    session: u64,
}

impl acp::Sink for AgentSink {
    fn emit(&self, event: acp::Event) {
        let _ = self.app.emit(
            "agent",
            acp::Envelope {
                session: self.session,
                event,
            },
        );
    }
}

/// An agent as the settings dialog edits it: the arguments as one line.
#[derive(Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: String,
}

#[derive(Serialize)]
pub struct AgentProfiles {
    pub agents: Vec<AgentEntry>,
    pub selected: Option<String>,
}

/// Arguments as one line again (quoted where they contain spaces).
fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() || a.chars().any(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn profiles_of(s: &Settings) -> AgentProfiles {
    AgentProfiles {
        agents: s
            .agents
            .iter()
            .map(|a| AgentEntry {
                name: a.name.clone(),
                command: a.command.clone(),
                args: join_args(&a.args),
            })
            .collect(),
        selected: s.agent.clone(),
    }
}

/// The configured agents (Settings → Agents).
#[tauri::command]
pub fn agent_profiles(state: State<'_, AppState>) -> AgentProfiles {
    profiles_of(&snapshot(&state))
}

/// Most agents the settings hold.
const MAX_AGENTS: usize = 20;

fn parse_profiles(agents: Vec<AgentEntry>) -> Result<Vec<acp::AgentProfile>, String> {
    if agents.len() > MAX_AGENTS {
        return Err(format!("At most {MAX_AGENTS} agents."));
    }
    let mut profiles: Vec<acp::AgentProfile> = Vec::new();
    for a in agents {
        let profile = acp::AgentProfile {
            name: a.name.trim().to_string(),
            command: a.command.trim().to_string(),
            args: acp::split_args(&a.args),
        };
        profile.validate()?;
        if profiles.iter().any(|p| p.name == profile.name) {
            return Err(format!("Two agents are called “{}”.", profile.name));
        }
        profiles.push(profile);
    }
    Ok(profiles)
}

#[tauri::command]
pub fn set_agent_profiles(
    state: State<'_, AppState>,
    agents: Vec<AgentEntry>,
) -> CmdResult<AgentProfiles> {
    let profiles = parse_profiles(agents).map_err(ApiError::local)?;
    let s = update_settings(&state, |s| {
        if !s
            .agent
            .as_ref()
            .is_some_and(|name| profiles.iter().any(|p| &p.name == name))
        {
            s.agent = profiles.first().map(|p| p.name.clone());
        }
        s.agents = profiles;
    })?;
    Ok(profiles_of(&s))
}

#[derive(Serialize)]
pub struct AgentState {
    pub session: u64,
    pub name: String,
    pub info: acp::AgentInfo,
    /// A prompt turn is running.
    pub busy: bool,
    /// The models the agent offers, if it offers a choice.
    pub models: Option<acp::Models>,
}

fn describe(running: &RunningAgent) -> AgentState {
    AgentState {
        session: running.session,
        name: running.name.clone(),
        info: running.agent.info.clone(),
        busy: running.agent.busy(),
        models: running.agent.models(),
    }
}

fn current_agent(state: &State<'_, AppState>) -> CmdResult<Arc<acp::Session>> {
    state
        .agent
        .lock()
        .expect("agent lock")
        .as_ref()
        .map(|r| r.agent.clone())
        .ok_or_else(|| ApiError::local("No agent is running – start one first."))
}

fn stop_agent(state: &State<'_, AppState>) {
    let running = state.agent.lock().expect("agent lock").take();
    if let Some(running) = running {
        running.agent.stop();
    }
}

/// The running agent session, if any (after a reload of the window).
#[tauri::command]
pub fn agent_status(state: State<'_, AppState>) -> Option<AgentState> {
    state
        .agent
        .lock()
        .expect("agent lock")
        .as_ref()
        .map(describe)
}

/// Starts a configured agent in the workspace folder and opens a session
/// with TeamCraft's MCP server (the desktop's token, so the user's rights).
#[tauri::command]
pub async fn agent_start(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> CmdResult<AgentState> {
    let api = signed_in_api(&state)?;
    let profile = snapshot(&state)
        .agents
        .into_iter()
        .find(|a| a.name == name)
        .ok_or_else(|| ApiError::local(format!("There is no agent “{name}” in the settings.")))?;
    stop_agent(&state);
    let root = workspace_root(&app, &state);
    std::fs::create_dir_all(&root)?;
    let session = state.agent_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let sink = Arc::new(AgentSink {
        app: app.clone(),
        session,
    });
    let opts = acp::Options {
        cwd: root,
        mcp: api.token.clone().map(|token| acp::McpServer {
            url: format!("{}/api/v1/mcp", api.base),
            token,
        }),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        stderr: acp::Tail::default(),
    };
    let agent = Arc::new(
        acp::spawn(&profile, sink, opts)
            .await
            .map_err(ApiError::local)?,
    );
    if state.agent_seq.load(Ordering::SeqCst) != session {
        agent.stop();
        return Err(ApiError::local("Another agent was started meanwhile."));
    }
    // the model picked last for this agent, if it still offers it
    let remembered = snapshot(&state).agent_models.get(&name).cloned();
    if let (Some(model), Some(models)) = (remembered, agent.models()) {
        let offered = models.options.iter().any(|o| o.id == model);
        if offered && models.current.as_deref() != Some(model.as_str()) {
            // a failure keeps the agent's default; the picker shows which
            let _ = agent.set_model(&model).await;
        }
    }
    let _ = update_settings(&state, |s| s.agent = Some(name.clone()));
    let running = RunningAgent {
        session,
        name,
        agent,
    };
    let described = describe(&running);
    let old = state.agent.lock().expect("agent lock").replace(running);
    if let Some(old) = old {
        old.agent.stop();
    }
    Ok(described)
}

/// Sends a message (with what the user is looking at) and waits for the end
/// of the turn; the answer streams as `agent` events. Returns the stop reason.
#[tauri::command]
pub async fn agent_prompt(
    state: State<'_, AppState>,
    text: String,
    context: Option<acp::PromptContext>,
) -> CmdResult<String> {
    let agent = current_agent(&state)?;
    agent
        .prompt(&text, &context.unwrap_or_default())
        .await
        .map_err(ApiError::local)
}

/// The user's answer to a permission request: allow once or reject. The
/// request is named by its session and key – keys restart with every
/// session, so a card of an ended session must not answer a new request.
#[tauri::command]
pub async fn agent_permission(
    state: State<'_, AppState>,
    session: u64,
    key: u64,
    allow: bool,
) -> CmdResult<()> {
    let agent = state
        .agent
        .lock()
        .expect("agent lock")
        .as_ref()
        .filter(|r| r.session == session)
        .map(|r| r.agent.clone())
        .ok_or_else(|| ApiError::local("This request is no longer open."))?;
    agent
        .answer_permission(key, allow)
        .await
        .map_err(ApiError::local)
}

/// Switches the agent to one of the models it offers and remembers the pick
/// for its next start. Named by session, like permission answers.
#[tauri::command]
pub async fn agent_set_model(
    state: State<'_, AppState>,
    session: u64,
    model: String,
) -> CmdResult<acp::Models> {
    let (name, agent) = state
        .agent
        .lock()
        .expect("agent lock")
        .as_ref()
        .filter(|r| r.session == session)
        .map(|r| (r.name.clone(), r.agent.clone()))
        .ok_or_else(|| ApiError::local("This agent is no longer running."))?;
    let models = agent.set_model(&model).await.map_err(ApiError::local)?;
    let _ = update_settings(&state, |s| {
        s.agent_models.insert(name, model);
    });
    Ok(models)
}

/// Stops the running turn of the agent.
#[tauri::command]
pub async fn agent_cancel(state: State<'_, AppState>) -> CmdResult<()> {
    let agent = current_agent(&state)?;
    agent.cancel().await.map_err(ApiError::local)
}

/// Ends the agent process.
#[tauri::command]
pub fn agent_stop(state: State<'_, AppState>) {
    stop_agent(&state);
}

#[cfg(test)]
mod agent_tests {
    use super::*;

    #[test]
    fn agent_entries_round_trip() {
        let profiles = parse_profiles(vec![
            AgentEntry {
                name: " Claude Code ".into(),
                command: "npx".into(),
                args: "-y @zed-industries/claude-code-acp".into(),
            },
            AgentEntry {
                name: "Gemini".into(),
                command: "gemini".into(),
                args: "--experimental-acp --x \"a b\"".into(),
            },
        ])
        .unwrap();
        assert_eq!(profiles[0].name, "Claude Code");
        assert_eq!(
            profiles[0].args,
            vec!["-y", "@zed-industries/claude-code-acp"]
        );
        assert_eq!(
            join_args(&profiles[1].args),
            "--experimental-acp --x \"a b\""
        );
        let twice = vec![
            AgentEntry {
                name: "A".into(),
                command: "a".into(),
                args: String::new(),
            },
            AgentEntry {
                name: "A".into(),
                command: "b".into(),
                args: String::new(),
            },
        ];
        assert!(parse_profiles(twice).is_err());
    }
}
