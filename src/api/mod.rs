//! JSON REST API (`/api/v1`). Authentication: `Authorization: Bearer <token>`
//! with a personal access token created on the account page.
//!
//! Items are addressed by their number and revisions by their code, e.g.
//! `GET /api/v1/items/PRT-000042/revisions/B`. The code `latest` selects the
//! newest revision.

mod change_requests;
mod interchange_api;
mod personal;
mod simulations;
pub(crate) mod uploads_api;
mod webhooks;

use axum::{
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{Actor, ApiUser},
    domain::{
        self, ChangeStatus, Decision, ItemType, LifecycleState, Priority, ProjectVisibility,
        ReferenceKind, RevisionRule,
    },
    error::{AppError, AppResult},
    models::Revision,
    services::{
        attributes, audit, bom, changes, checkouts, desktop, files, impacts, imports, items,
        notifications, package, projects, references, requests, reviewers, revisions, similar,
        stats, uploads, users, workset, Page,
    },
    web, AppState,
};

pub fn router(state: &AppState) -> Router<AppState> {
    let upload_limit = usize::try_from(state.config.max_upload_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(1024 * 1024);
    let import_limit = imports::MAX_STEP_BYTES + 1024 * 1024;
    let attachment_limit = requests::MAX_ATTACHMENT_BYTES + 1024 * 1024;
    Router::new()
        .route("/", get(index))
        .route("/me", get(me))
        .route("/desktop/token", post(desktop_token))
        .route("/worklist", get(worklist))
        .route("/worklist/checkin-all", post(checkin_all))
        .route("/mcp", post(crate::mcp::http).get(crate::mcp::http_get))
        .route("/assistant", get(assistant_info).post(assistant_ask))
        .route("/formats", get(formats))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{code}/visibility", put(set_project_visibility))
        .route(
            "/projects/{code}/members",
            get(project_members).post(add_project_member),
        )
        .route(
            "/projects/{code}/numbering",
            get(project_numbering).put(set_project_numbering),
        )
        .route("/items", get(list_items).post(create_item))
        .route("/items/{number}", get(get_item).patch(update_item))
        .route("/attributes", get(list_attributes).post(create_attribute))
        .route("/attributes/{key}", patch(update_attribute))
        .route("/items/{number}/where-used", get(where_used))
        .route("/items/{number}/interchange", get(interchange_api::show))
        .route(
            "/items/{number}/alternates",
            post(interchange_api::add_alternate),
        )
        .route(
            "/items/{number}/alternates/{alternate}",
            delete(interchange_api::delete_alternate),
        )
        .route(
            "/items/{number}/supersession",
            put(interchange_api::supersede).delete(interchange_api::clear_supersession),
        )
        .route(
            "/items/{number}/replace",
            get(interchange_api::plan_replace).post(interchange_api::replace),
        )
        .route("/items/{number}/history", get(item_history))
        .route("/items/{number}/impacts", get(item_impacts))
        .route(
            "/items/{number}/revisions/{code}/references",
            get(get_references).post(add_reference),
        )
        .route("/references/{id}", axum::routing::delete(delete_reference))
        // simulations
        .route(
            "/items/{number}/revisions/{code}/simulation",
            get(simulations::get_simulation),
        )
        .route(
            "/items/{number}/revisions/{code}/simulation/bases",
            get(simulations::list_bases).post(simulations::add_basis),
        )
        .route(
            "/simulation-bases/{id}",
            axum::routing::delete(simulations::delete_basis),
        )
        .route(
            "/items/{number}/revisions/{code}/simulation/results",
            get(simulations::list_results)
                .post(simulations::add_result)
                .put(simulations::put_results),
        )
        .route(
            "/items/{number}/revisions/{code}/simulation/results/import",
            post(simulations::import_results).layer(DefaultBodyLimit::max(import_limit)),
        )
        .route(
            "/simulation-results/{id}",
            patch(simulations::patch_result).delete(simulations::delete_result),
        )
        .route(
            "/items/{number}/revisions/{code}/simulation/compare",
            get(simulations::compare),
        )
        .route(
            "/items/{number}/simulations",
            get(simulations::item_simulations),
        )
        .route("/impacts", get(list_impacts))
        .route("/notifications", get(list_notifications))
        .route("/notifications/read", post(read_notifications))
        .route(
            "/notifications/settings",
            get(get_notification_settings).put(put_notification_settings),
        )
        .route("/notifications/test", post(test_notification))
        .route("/watches", get(list_watches))
        .route("/favorites", get(personal::list_favorites))
        .route(
            "/items/{number}/favorite",
            post(personal::set_favorite).delete(personal::set_favorite),
        )
        .route(
            "/searches",
            get(personal::list_searches).post(personal::create_search),
        )
        .route(
            "/searches/{id}",
            axum::routing::delete(personal::delete_search),
        )
        .route(
            "/items/{number}/revisions/{code}/save-as",
            post(personal::save_as),
        )
        .route(
            "/items/{number}/revisions/{code}/bom/compare",
            get(personal::bom_compare),
        )
        .route(
            "/items/{number}/revisions/{code}/bom/compare.xlsx",
            get(personal::bom_compare_xlsx),
        )
        .route("/items/{number}/watch", post(watch_item).delete(watch_item))
        .route(
            "/changes/{number}/watch",
            post(watch_change).delete(watch_change),
        )
        .route("/impacts/{id}/acknowledge", post(acknowledge_impact))
        .route(
            "/items/{number}/revisions/{code}/impacts/acknowledge",
            post(acknowledge_impacts),
        )
        .route("/changes/{number}/impact", get(change_impact))
        .route(
            "/items/{number}/revisions/{code}",
            get(get_revision).patch(update_revision),
        )
        .route("/items/{number}/revisions/{code}/checkout", post(checkout))
        .route(
            "/items/{number}/revisions/{code}/file-status",
            get(file_status).post(file_status),
        )
        .route(
            "/items/{number}/revisions/{code}/checkin",
            post(checkin).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/items/{number}/revisions/{code}/cancel-checkout",
            post(cancel_checkout),
        )
        .route("/items/{number}/revisions/{code}/revise", post(revise))
        .route("/items/{number}/revisions/{code}/package", get(get_package))
        .route("/items/{number}/revisions/{code}/workset", get(get_workset))
        .route("/items/{number}/revisions/{code}/similar", get(get_similar))
        .route(
            "/bom-imports",
            post(import_bom).layer(DefaultBodyLimit::max(import_limit)),
        )
        .route(
            "/items/{number}/revisions/{code}/bom/import",
            post(import_bom_into).layer(DefaultBodyLimit::max(import_limit)),
        )
        .route("/items/{number}/revisions/{code}/obsolete", post(obsolete))
        .route(
            "/items/{number}/revisions/{code}/bom",
            get(get_bom).post(add_bom_line),
        )
        .route(
            "/items/{number}/revisions/{code}/bom.xlsx",
            get(get_bom_xlsx),
        )
        .route(
            "/items/{number}/revisions/{code}/files",
            get(list_files)
                .post(upload_files)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/bom-lines/{id}",
            patch(update_bom_line).delete(delete_bom_line),
        )
        .route("/uploads", post(uploads_api::create))
        .route(
            "/uploads/{id}",
            get(uploads_api::status)
                .patch(uploads_api::append)
                .delete(uploads_api::abort),
        )
        .route("/uploads/{id}/finish", post(uploads_api::finish))
        .route(
            "/files/{id}",
            get(file_meta).patch(update_file).delete(delete_file),
        )
        .route("/files/{id}/content", get(download))
        .route("/files/{id}/thumbnail", get(file_thumbnail))
        .route("/files/{id}/versions", get(file_versions))
        .route("/files/{id}/restore", post(restore_file))
        .route("/file-versions/{id}/content", get(download_version))
        .route("/files/{id}/mesh", get(file_mesh))
        .route("/file-versions/{id}/mesh", get(version_mesh))
        .route("/changes", get(list_changes).post(create_change))
        .route("/changes/{number}", get(get_change))
        .route("/changes/{number}/items", post(add_change_item))
        .route("/changes/{number}/reviewers", post(add_change_reviewer))
        .route("/changes/{number}/readiness", get(change_readiness))
        .route(
            "/changes/{number}/suggested-reviewers",
            get(change_suggested_reviewers),
        )
        .route(
            "/changes/{number}/dependencies",
            post(add_change_dependencies),
        )
        .route("/changes/{number}/submit", post(submit_change))
        .route("/changes/{number}/review", post(review_change))
        .route("/changes/{number}/withdraw", post(withdraw_change))
        .route(
            "/change-requests",
            get(change_requests::list).post(change_requests::create),
        )
        .route(
            "/change-requests/{number}",
            get(change_requests::get).patch(change_requests::update),
        )
        .route(
            "/change-requests/{number}/comments",
            post(change_requests::comment),
        )
        .route(
            "/change-requests/{number}/items",
            post(change_requests::add_item),
        )
        .route(
            "/change-requests/{number}/triage",
            post(change_requests::triage),
        )
        .route(
            "/change-requests/{number}/assign",
            post(change_requests::assign),
        )
        .route(
            "/change-requests/{number}/decide",
            post(change_requests::decide),
        )
        .route(
            "/change-requests/{number}/reopen",
            post(change_requests::reopen),
        )
        .route(
            "/change-requests/{number}/withdraw",
            post(change_requests::withdraw),
        )
        .route(
            "/change-requests/{number}/create-change",
            post(change_requests::create_change),
        )
        .route(
            "/change-requests/{number}/link-change",
            post(change_requests::link_change),
        )
        .route(
            "/change-requests/{number}/attachments",
            post(change_requests::attach).layer(DefaultBodyLimit::max(attachment_limit)),
        )
        .route(
            "/change-requests/{number}/watch",
            post(change_requests::watch).delete(change_requests::watch),
        )
        .route(
            "/change-request-attachments/{id}/content",
            get(change_requests::attachment_content),
        )
        // outgoing event webhooks (administrators)
        .route("/webhooks", get(webhooks::list).post(webhooks::create))
        .route(
            "/webhooks/{id}",
            get(webhooks::get)
                .patch(webhooks::update)
                .delete(webhooks::delete),
        )
        .route(
            "/webhooks/{id}/rotate-secret",
            post(webhooks::rotate_secret),
        )
        .route("/webhooks/{id}/test", post(webhooks::test))
        .route("/webhooks/{id}/deliveries", get(webhooks::deliveries))
        .route("/webhook-deliveries/{id}/retry", post(webhooks::retry))
        .fallback(not_found)
}

/// `Json` extractor whose rejections use the API error format.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => Err(AppError::BadRequest(rejection.body_text())),
        }
    }
}

/// Distinguishes "field missing" (`None`) from "field is null" (`Some(None)`).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

async fn not_found() -> AppError {
    AppError::NotFound("Unknown API endpoint. GET /api/v1 lists the available endpoints.".into())
}

async fn index() -> Json<Value> {
    Json(json!({
        "name": "TeamCraft REST API",
        "version": env!("CARGO_PKG_VERSION"),
        "authentication": "Authorization: Bearer <personal access token>",
        "endpoints": [
            "GET    /api/v1/me",
            "GET    /api/v1/worklist                                  my check-outs, reviews waiting for me, my open changes, change requests assigned to me and raised by me, dependents to check (impacts, and grouped by dependent revision as impact_groups), statistics",
            "POST   /api/v1/desktop/token                             {code, verifier} – desktop sign-in, no token needed",
            "GET    /api/v1/projects                                  the groups you may see (restricted groups only for members and admins)",
            "POST   /api/v1/projects                                  {code, name, description?, oidc_group?, visibility?}",
            "PUT    /api/v1/projects/{code}/visibility                {visibility: internal|restricted} – managers and admins",
            "GET    /api/v1/projects/{code}/members",
            "POST   /api/v1/projects/{code}/members                   {username, manager?} – managers and admins",
            "GET    /api/v1/projects/{code}/numbering?scheme=         next generated item numbers per type; scheme = preview another scheme",
            "PUT    /api/v1/projects/{code}/numbering                 {scheme} e.g. {PROJECT}-{TYPE}-{YYYY}-{SEQ:4}, empty = {TYPE}-{SEQ:6} – managers and admins",
            "POST   /api/v1/mcp                                       Model Context Protocol (JSON-RPC 2.0) for AI assistants, see the MCP reference",
            "GET    /api/v1/assistant                                 whether the built-in assistant is enabled, and its quick actions",
            "POST   /api/v1/assistant                                 {question?, action?: ask|release_notes|release_check|review_brief|attention, context: {item?, revision?, change?}, allow_drafts?}",
            "GET    /api/v1/formats                                   the CAD programs and file formats TeamCraft recognises",
            "GET    /api/v1/items?q=&type=&project=&state=&system=&class=&attr=&value=&limit=&offset=   system = CAD program, e.g. solidworks, nx, creo; class = make|purchased|standard; attr + value = custom attribute (value: text contains, yes/no, 12, >=10, <=20, 10..20, empty = has a value)",
            "POST   /api/v1/items                                     {item_type, name, number?, unit?, project?, description?, material?, mass_kg?, unit_cost?, classification?, attributes?}",
            "GET    /api/v1/items/{number}",
            "PATCH  /api/v1/items/{number}                            {project?, unit?, classification?: make|purchased|standard} – master data",
            "GET    /api/v1/attributes?all=                           custom attribute definitions (all=true: also deactivated ones)",
            "POST   /api/v1/attributes                                {key, data_type: text|number|boolean|choice, label, unit?, choices?, item_types?, position?} – admins",
            "PATCH  /api/v1/attributes/{key}                          {label?, unit?, choices?, item_types?, position?, active?} – admins",
            "GET    /api/v1/items/{number}/where-used?levels=         levels = N walks N levels up (parents of parents)",
            "GET    /api/v1/items/{number}/interchange                alternates, one-way 'alternate for', successor (superseded by), predecessors",
            "POST   /api/v1/items/{number}/alternates                 {item, note?, two_way? = true} – item may be built in instead",
            "DELETE /api/v1/items/{number}/alternates/{alternate}     both directions if you may edit the alternate's group",
            "PUT    /api/v1/items/{number}/supersession               {item, note?} – superseded by item (do not use for new designs)",
            "DELETE /api/v1/items/{number}/supersession",
            "GET    /api/v1/items/{number}/replace?with=              preview: BOM lines of the latest parent revisions and what replacing would do",
            "POST   /api/v1/items/{number}/replace                    {with} – replace in every parent that is in work and not checked out by others",
            "GET    /api/v1/items/{number}/history?limit=             audit log of the item and its revisions",
            "GET    /api/v1/items/{number}/impacts                    impacts on the item (open first)",
            "GET    /api/v1/items/{number}/revisions/{code|latest}",
            "PATCH  /api/v1/items/{number}/revisions/{code}           {name?, description?, material?, mass_kg?, unit_cost?, attributes?: {key: value|null}}",
            "POST   /api/v1/items/{number}/revisions/{code}/checkout",
            "POST   /api/v1/items/{number}/revisions/{code}/checkin   optional multipart: 'file' (new versions), 'comment', 'bases' = {\"name\": version | sha256} → 409 if a file moved on, 'folder' or 'folders' = {\"name\": \"folder\"}, 'uploads' = [upload id] (chunked uploads)",
            "POST   /api/v1/uploads                                   {item, revision, filename, size, sha256?, content_type?, base?, folder?} – start a chunked, resumable upload (checked-out revision)",
            "GET    /api/v1/uploads/{id}                              state and offset to resume from (also HEAD: Upload-Offset, Upload-Length)",
            "PATCH  /api/v1/uploads/{id}                              header Upload-Offset, body = next chunk (≤ chunk_bytes) → 409 at any other offset",
            "POST   /api/v1/uploads/{id}/finish                       {comment?, check_in?} – verify size and sha256, store the new version (and release the check-out)",
            "DELETE /api/v1/uploads/{id}                              cancel the upload",
            "GET    /api/v1/items/{number}/revisions/{code}/file-status   current version, sha256 and lock of every file",
            "POST   /api/v1/items/{number}/revisions/{code}/file-status   {files: [{filename, sha256, base_version? | base_sha256?}]} → up_to_date | modified | outdated | conflict | new | missing",
            "POST   /api/v1/worklist/checkin-all                      {revisions?: [id]} – check in my check-outs without new files (lock release), one by one",
            "POST   /api/v1/items/{number}/revisions/{code}/cancel-checkout",
            "POST   /api/v1/items/{number}/revisions/{code}/revise",
            "POST   /api/v1/items/{number}/revisions/{code}/obsolete",
            "GET    /api/v1/items/{number}/revisions/{code}/bom?rule=latest|released|as_released&expand=true",
            "POST   /api/v1/items/{number}/revisions/{code}/bom       {child, quantity, find_number?, notes?}",
            "GET    /api/v1/items/{number}/revisions/{code}/bom.xlsx?rule=  the expanded BOM as Excel workbook (the BOM import reads it back)",
            "GET    /api/v1/items/{number}/revisions/{code}/package?rule=  Pack and Go: ZIP with all files of the structure, bom.csv, manifest.json",
            "GET    /api/v1/items/{number}/revisions/{code}/workset?rule=&references=true  desktop Get: files of the structure and the CAD files it references, with folder, version, sha256, lock",
            "POST   /api/v1/bom-imports                               multipart: file (CSV/Excel BOM or STEP assembly of a CAD program), project, name?, numbers=file|generate, match_existing=true|false, dry_run=true → plan only; creates a new assembly",
            "POST   /api/v1/items/{number}/revisions/{code}/bom/import  same multipart fields – imports below the revision (checked out by me)",
            "PATCH  /api/v1/bom-lines/{id}                            {quantity?, find_number?, notes?}",
            "DELETE /api/v1/bom-lines/{id}",
            "GET    /api/v1/items/{number}/revisions/{code}/references  references, referrers, CAD file references, impacts",
            "POST   /api/v1/items/{number}/revisions/{code}/references  {item, kind: drawing|specification|reference, note?}",
            "DELETE /api/v1/references/{id}",
            "GET    /api/v1/items/{number}/revisions/{code}/simulation  basis links (out of date?), result values (pass/fail), summary",
            "GET    /api/v1/items/{number}/revisions/{code}/simulation/bases",
            "POST   /api/v1/items/{number}/revisions/{code}/simulation/bases  {item, revision?, file? | file_version_id?, role: geometry|mesh|material|load_case|other, note?}",
            "DELETE /api/v1/simulation-bases/{id}",
            "GET    /api/v1/items/{number}/revisions/{code}/simulation/results",
            "POST   /api/v1/items/{number}/revisions/{code}/simulation/results  {name, value, unit?, lower_limit?, upper_limit?, load_case?, solver?, solver_version?, note?}",
            "PUT    /api/v1/items/{number}/revisions/{code}/simulation/results  {results: [...], replace?: true} – the result set of a solver run",
            "POST   /api/v1/items/{number}/revisions/{code}/simulation/results/import  multipart: file (CSV/Excel: name,value,unit,limit_low,limit_high,load_case), replace?",
            "PATCH  /api/v1/simulation-results/{id}                   any result field",
            "DELETE /api/v1/simulation-results/{id}",
            "GET    /api/v1/items/{number}/revisions/{code}/simulation/compare?with=A   result values of two revisions",
            "GET    /api/v1/items/{number}/simulations                simulations based on the item (out of date?)",
            "GET    /api/v1/impacts                                   open impacts on items of my project groups",
            "GET    /api/v1/notifications?unread=true&limit=          my notifications and the unread count",
            "POST   /api/v1/notifications/read                        {ids?} – mark as read (all without a body)",
            "GET    /api/v1/notifications/settings",
            "PUT    /api/v1/notifications/settings                    {email, mattermost, mattermost_username?, muted: [kind]}",
            "POST   /api/v1/notifications/test                        test notification through every channel I use",
            "GET    /api/v1/watches                                   items and changes I watch",
            "POST   /api/v1/items/{number}/watch                      watch an item (DELETE: stop)",
            "GET    /api/v1/favorites                                 my favorites (latest revision each)",
            "POST   /api/v1/items/{number}/favorite                   add to my favorites (DELETE: remove)",
            "GET    /api/v1/searches                                  my saved item searches",
            "POST   /api/v1/searches                                  {name, q?, type?, project?, state?, system?}",
            "DELETE /api/v1/searches/{id}",
            "POST   /api/v1/items/{number}/revisions/{code}/save-as   {number?, name?, project?, files?, bom?} – new item from this revision",
            "GET    /api/v1/items/{number}/revisions/{code}/bom/compare?with=A&depth=1|all&rule=   added / removed / changed BOM lines (all levels with depth=all)",
            "GET    /api/v1/items/{number}/revisions/{code}/bom/compare.xlsx?with=A   the same comparison as an Excel workbook",
            "POST   /api/v1/changes/{number}/watch                    watch a change (DELETE: stop)",
            "POST   /api/v1/impacts/{id}/acknowledge                  {note} – the dependent needs no change",
            "POST   /api/v1/items/{number}/revisions/{code}/impacts/acknowledge  {note, impacts?: [id]} – acknowledge all open impacts on the dependent revision (or the listed ones) at once",
            "GET    /api/v1/items/{number}/revisions/{code}/files",
            "POST   /api/v1/items/{number}/revisions/{code}/files     multipart/form-data, field 'file' (same name = new version), 'comment', 'bases', 'folder' (in the project group) or 'folders' = {\"name\": \"folder\"}",
            "GET    /api/v1/files/{id}",
            "PATCH  /api/v1/files/{id}                                {folder} – its place in the working folder (revision checked out by you)",
            "GET    /api/v1/files/{id}/content",
            "GET    /api/v1/files/{id}/thumbnail                      preview image stored in Inventor, Solid Edge and SolidWorks files (PNG or BMP, 404 if none)",
            "GET    /api/v1/files/{id}/versions",
            "POST   /api/v1/files/{id}/restore                        {version}",
            "GET    /api/v1/file-versions/{id}/content",
            "DELETE /api/v1/files/{id}",
            "GET    /api/v1/changes?status=",
            "POST   /api/v1/changes                                   {title, description?, priority?}",
            "GET    /api/v1/changes/{number}",
            "GET    /api/v1/changes/{number}/impact                   impact analysis: items that depend on its revisions",
            "POST   /api/v1/changes/{number}/items                    {item, revision?}",
            "POST   /api/v1/changes/{number}/reviewers                {username}",
            "GET    /api/v1/changes/{number}/suggested-reviewers?limit=  ranked reviewer suggestions with reasons (never assigns)",
            "GET    /api/v1/changes/{number}/readiness                release readiness: unreleased (blocking) and obsolete or file-scan (warning) dependencies",
            "POST   /api/v1/changes/{number}/dependencies             add the in-work revisions of all unreleased dependencies, level by level",
            "POST   /api/v1/changes/{number}/submit",
            "POST   /api/v1/changes/{number}/review                   {decision: approve|reject, comment?}",
            "POST   /api/v1/changes/{number}/withdraw",
            "GET    /api/v1/change-requests?status=open|all|<status>&scope=mine|assigned&item=&limit=&offset=",
            "POST   /api/v1/change-requests                           {title, description?, reason?, priority?, due_date?, items?: [{item, revision?, note?}]} – any signed-in user",
            "GET    /api/v1/change-requests/{number}                  request, items, comments, attachments, change orders, history, what I may do",
            "PATCH  /api/v1/change-requests/{number}                  {title?, description?, reason?, priority?, due_date?}",
            "POST   /api/v1/change-requests/{number}/comments         {body} – any signed-in user",
            "POST   /api/v1/change-requests/{number}/items            {item, revision?, note?}",
            "POST   /api/v1/change-requests/{number}/triage           new → in triage (CAD editors of the affected groups, admins)",
            "POST   /api/v1/change-requests/{number}/assign           {username|null}",
            "POST   /api/v1/change-requests/{number}/decide           {decision: accept|reject|defer, comment?}",
            "POST   /api/v1/change-requests/{number}/reopen           deferred → in triage",
            "POST   /api/v1/change-requests/{number}/withdraw         the requester, before the decision",
            "POST   /api/v1/change-requests/{number}/create-change    {title?, priority?} – draft change order with the in-work revisions of the affected items",
            "POST   /api/v1/change-requests/{number}/link-change      {change} – link an open change order",
            "POST   /api/v1/change-requests/{number}/attachments      multipart 'file': PNG, JPEG, GIF, WebP or PDF, up to 10 MiB",
            "GET    /api/v1/change-request-attachments/{id}/content",
            "POST   /api/v1/change-requests/{number}/watch            watch a change request (DELETE: stop)",
            "GET    /api/v1/webhooks                                  outgoing event webhooks and the event names – admins",
            "POST   /api/v1/webhooks                                  {name, url, events, project?, active?} – returns the secret once",
            "GET    /api/v1/webhooks/{id}                             PATCH {name?, url?, events?, project?, active?}, DELETE",
            "POST   /api/v1/webhooks/{id}/rotate-secret               a new secret (shown once)",
            "POST   /api/v1/webhooks/{id}/test                        sends a ping now; returns the delivery",
            "GET    /api/v1/webhooks/{id}/deliveries?limit=           latest deliveries with status, attempts, last error",
            "POST   /api/v1/webhook-deliveries/{id}/retry             send a failed delivery again"
        ]
    }))
}

async fn me(State(st): State<AppState>, ApiUser(me): ApiUser) -> AppResult<Json<Value>> {
    // `hidden_projects` is never serialized; the client learns only which of
    // its own groups are restricted (for a badge).
    let mut body = json!(me);
    body["restricted_projects"] = json!(projects::restricted_memberships(&st.db, &me).await?);
    body["capabilities"] = json!({
        "assistant": crate::services::assistant::enabled(&st),
        "mcp": true,
    });
    Ok(Json(body))
}

/// `GET /api/v1/assistant`: whether the assistant is enabled and what it offers.
async fn assistant_info(State(st): State<AppState>, ApiUser(_me): ApiUser) -> Json<Value> {
    let actions: Vec<Value> = crate::assistant::Action::ALL
        .iter()
        .map(|a| json!({ "id": a.as_str(), "label": a.label(), "needs": a.needs() }))
        .collect();
    Json(json!({
        "enabled": crate::services::assistant::enabled(&st),
        "model": st.assistant.as_ref().map(|a| a.model.clone()),
        "max_question_chars": crate::assistant::MAX_QUESTION_CHARS,
        "actions": actions,
    }))
}

#[derive(Deserialize, Default)]
struct AssistantContext {
    item: Option<String>,
    revision: Option<String>,
    change: Option<String>,
}

#[derive(Deserialize)]
struct AssistantBody {
    question: Option<String>,
    action: Option<String>,
    #[serde(default)]
    context: AssistantContext,
    #[serde(default)]
    allow_drafts: bool,
}

/// `POST /api/v1/assistant`: runs the assistant with the caller's rights.
async fn assistant_ask(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<AssistantBody>,
) -> AppResult<Json<Value>> {
    if !crate::services::assistant::enabled(&st) {
        return Err(crate::assistant::AssistantError::Disabled.into_app_error());
    }
    let question = crate::assistant::Question::new(
        body.action.as_deref(),
        body.question.as_deref(),
        body.context.item.as_deref(),
        body.context.revision.as_deref(),
        body.context.change.as_deref(),
        body.allow_drafts,
    )
    .map_err(AppError::BadRequest)?;
    let run = crate::services::assistant::ask(&st, &me, question).await?;
    Ok(Json(json!(run)))
}

#[derive(Deserialize)]
struct DesktopTokenBody {
    code: String,
    verifier: String,
}

/// Desktop sign-in: one-time code from the browser + PKCE verifier → token.
/// Unauthenticated by design (the code is the credential).
async fn desktop_token(
    State(st): State<AppState>,
    ApiJson(body): ApiJson<DesktopTokenBody>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        desktop::exchange(&st.db, &body.code, &body.verifier).await?
    )))
}

/// The personal start page of the desktop app (and the web dashboard).
async fn worklist(State(st): State<AppState>, ApiUser(me): ApiUser) -> AppResult<Json<Value>> {
    // independent queries run concurrently (like the dashboard)
    let (checkouts, reviews, open, unread, my_changes, assigned, my_requests, stats) = tokio::try_join!(
        revisions::checked_out_by(&st.db, &me),
        changes::pending_reviews(&st.db, &me),
        impacts::open_for(&st.db, &me),
        notifications::unread_count(&st.db, &me),
        changes::open_by(&st.db, &me),
        requests::assigned_to(&st.db, &me),
        requests::open_by(&st.db, &me),
        stats::load(&st.db, &me),
    )?;
    Ok(Json(json!({
        "checkouts": checkouts,
        "reviews": reviews,
        "impact_groups": impacts::group_by_dependent(open.clone()),
        "impacts": open,
        "notifications_unread": unread,
        "my_changes": my_changes,
        "requests_assigned": assigned,
        "my_requests": my_requests,
        "stats": stats,
    })))
}

/// The file formats TeamCraft recognises, grouped by CAD program.
async fn formats(ApiUser(_me): ApiUser) -> Json<Value> {
    Json(json!({
        "systems": crate::cadformat::catalogue(),
        "kinds": crate::cadformat::FormatKind::ALL
            .iter()
            .map(|k| json!({ "id": k, "label": k.label() }))
            .collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

async fn list_projects(State(st): State<AppState>, ApiUser(me): ApiUser) -> AppResult<Json<Value>> {
    Ok(Json(json!(projects::list(&st.db, &me, false).await?)))
}

#[derive(Deserialize)]
struct CreateProject {
    code: String,
    name: String,
    #[serde(default)]
    description: String,
    oidc_group: Option<String>,
    /// `internal` (default) or `restricted`.
    visibility: Option<String>,
}

async fn create_project(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateProject>,
) -> AppResult<Response> {
    let project = projects::create(
        &st.db,
        &me,
        &body.code,
        &body.name,
        &body.description,
        body.oidc_group.as_deref(),
        parse_visibility(body.visibility.as_deref().unwrap_or("internal"))?,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(project))).into_response())
}

fn parse_visibility(raw: &str) -> AppResult<ProjectVisibility> {
    ProjectVisibility::parse(raw)
        .ok_or_else(|| AppError::BadRequest("visibility must be internal or restricted.".into()))
}

#[derive(Deserialize)]
struct SetVisibility {
    visibility: String,
}

/// `PUT /api/v1/projects/{code}/visibility` – managers of the group, admins.
async fn set_project_visibility(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<SetVisibility>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let visibility = parse_visibility(&body.visibility)?;
    Ok(Json(json!(
        projects::set_visibility(&st.db, &me, project.id, visibility).await?
    )))
}

async fn project_members(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    Ok(Json(json!(projects::members(&st.db, project.id).await?)))
}

#[derive(Deserialize)]
struct AddMember {
    username: String,
    #[serde(default)]
    manager: bool,
}

async fn add_project_member(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<AddMember>,
) -> AppResult<Response> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let member =
        projects::add_member(&st.db, &me, project.id, &body.username, body.manager).await?;
    Ok((StatusCode::CREATED, Json(json!(member))).into_response())
}

#[derive(Deserialize)]
struct NumberingQuery {
    scheme: Option<String>,
}

async fn project_numbering(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    Query(q): Query<NumberingQuery>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let preview = projects::numbering_preview(&st.db, &project, q.scheme.as_deref()).await?;
    Ok(Json(json!(preview)))
}

#[derive(Deserialize)]
struct SetNumbering {
    #[serde(default)]
    scheme: Option<String>,
}

async fn set_project_numbering(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<SetNumbering>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let project = projects::set_number_scheme(
        &st.db,
        &me,
        project.id,
        body.scheme.as_deref().unwrap_or(""),
    )
    .await?;
    let preview = projects::numbering_preview(&st.db, &project, None).await?;
    Ok(Json(json!({ "project": project, "numbering": preview })))
}

// ---------------------------------------------------------------------------
// Items & revisions
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ItemQuery {
    q: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
    /// Project code.
    project: Option<String>,
    state: Option<String>,
    /// CAD program, e.g. `solidworks`, `nx`, `creo` (see `GET /api/v1/formats`).
    system: Option<String>,
    /// `make`, `purchased` or `standard`.
    class: Option<String>,
    /// Custom attribute key, with `value` (text contains, yes/no, `12`,
    /// `>=10`, `<=20`, `10..20`; empty = has a value).
    attr: Option<String>,
    value: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_items(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<ItemQuery>,
) -> AppResult<Json<Value>> {
    let project_id = match q.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, &me, code).await?.id),
        None => None,
    };
    let item_type = match q.item_type.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => Some(
            ItemType::parse(t)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown item type '{t}'.")))?,
        ),
        None => None,
    };
    let state = match q.state.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            LifecycleState::parse(s)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown state '{s}'.")))?,
        ),
        None => None,
    };
    let system = match q.system.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if crate::cadformat::system(s).is_some() => Some(s.to_string()),
        Some(s) => {
            return Err(AppError::BadRequest(format!(
                "Unknown CAD program '{s}'; GET /api/v1/formats lists them."
            )))
        }
        None => None,
    };
    let filter = items::ItemFilter {
        query: q.q,
        item_type,
        project_id,
        state,
        system,
        classification: domain::Classification::parse_field(q.class.as_deref())?,
        attribute: q.attr,
        attribute_value: q.value,
    };
    let page = Page::new(q.limit, q.offset);
    let (rows, total) = items::search(&st.db, &me, &filter, page).await?;
    Ok(Json(
        json!({ "items": rows, "total": total, "limit": page.limit, "offset": page.offset }),
    ))
}

#[derive(Deserialize)]
struct CreateItem {
    item_type: String,
    name: String,
    number: Option<String>,
    unit: Option<String>,
    /// Project code.
    project: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    material: String,
    mass_kg: Option<f64>,
    unit_cost: Option<f64>,
    /// `make` (default), `purchased` or `standard`.
    classification: Option<String>,
    /// Custom attribute values by key, e.g. `{"vendor": "Bosch", "length_mm": 120}`.
    #[serde(default)]
    attributes: serde_json::Map<String, Value>,
}

/// Custom attribute values of a request body as `key → text` changes.
fn attribute_changes(values: &serde_json::Map<String, Value>) -> AppResult<Vec<(String, String)>> {
    values
        .iter()
        .map(|(k, v)| Ok((k.clone(), domain::attribute_input_text(v)?)))
        .collect()
}

async fn create_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateItem>,
) -> AppResult<Response> {
    let item_type = ItemType::parse(&body.item_type).ok_or_else(|| {
        AppError::BadRequest(
            "item_type must be part, assembly, document, software or simulation.".into(),
        )
    })?;
    let project_id = match body.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, &me, code).await?.id),
        None => None,
    };
    let rev = items::create(
        &st.db,
        &me,
        items::NewItem {
            number: body.number,
            item_type,
            unit: body.unit.unwrap_or_default(),
            project_id,
            name: body.name,
            description: body.description,
            material: body.material,
            mass_kg: body.mass_kg,
            unit_cost: body.unit_cost,
            classification: domain::Classification::parse_field(body.classification.as_deref())?
                .unwrap_or_default(),
            attributes: attribute_changes(&body.attributes)?,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(rev))).into_response())
}

#[derive(Deserialize)]
struct UpdateItem {
    /// Project code to move the item to.
    project: Option<String>,
    unit: Option<String>,
    classification: Option<String>,
}

/// Item master data: project group, unit (before the first release) and
/// classification.
async fn update_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<UpdateItem>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let project_id = match body.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, &me, code).await?.id),
        None => item.project_id,
    };
    let updated = items::update_master(
        &st.db,
        &me,
        item.id,
        project_id,
        body.unit.as_deref().unwrap_or(&item.unit),
        domain::Classification::parse_field(body.classification.as_deref())?,
    )
    .await?;
    Ok(Json(json!(updated)))
}

// ---------------------------------------------------------------------------
// Custom attributes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AttributeQuery {
    /// `true` also lists deactivated attributes.
    #[serde(default)]
    all: bool,
}

async fn list_attributes(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Query(q): Query<AttributeQuery>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(attributes::list(&st.db, !q.all).await?)))
}

#[derive(Deserialize)]
struct AttributeBody {
    key: Option<String>,
    data_type: Option<String>,
    label: Option<String>,
    unit: Option<String>,
    choices: Option<Vec<String>>,
    item_types: Option<Vec<String>>,
    position: Option<i32>,
    active: Option<bool>,
}

async fn create_attribute(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<AttributeBody>,
) -> AppResult<Response> {
    let def = attributes::create(
        &st.db,
        &me,
        body.key.as_deref().unwrap_or(""),
        body.data_type.as_deref().unwrap_or("text"),
        attributes::AttributeSettings {
            label: body.label.unwrap_or_default(),
            unit: body.unit.unwrap_or_default(),
            choices: body.choices.unwrap_or_default().join("\n"),
            item_types: body.item_types.unwrap_or_default(),
            position: body.position.unwrap_or(0),
            active: true,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(def))).into_response())
}

async fn update_attribute(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(key): Path<String>,
    ApiJson(body): ApiJson<AttributeBody>,
) -> AppResult<Json<Value>> {
    me.require(domain::Role::Admin)?;
    let current = attributes::get_by_key(&st.db, &key).await?;
    let def = attributes::update(
        &st.db,
        &me,
        current.id,
        attributes::AttributeSettings {
            label: body.label.unwrap_or(current.label),
            unit: body.unit.unwrap_or(current.unit),
            choices: body.choices.unwrap_or(current.choices).join("\n"),
            item_types: body.item_types.unwrap_or(current.item_types),
            position: body.position.unwrap_or(current.position),
            active: body.active.unwrap_or(current.active),
        },
    )
    .await?;
    Ok(Json(json!(def)))
}

async fn get_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let revs = items::revisions(&st.db, &me, item.id).await?;
    let watching = notifications::is_watching(&st.db, me.id, "item", item.id).await?;
    Ok(Json(
        json!({ "item": item, "revisions": revs, "watching": watching }),
    ))
}

#[derive(Deserialize)]
struct WhereUsedQuery {
    /// Walk up this many levels (multi-level report with `level` and
    /// `child_number` per row); without it the single-level list.
    levels: Option<i32>,
}

async fn where_used(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    Query(q): Query<WhereUsedQuery>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let body = Json(match q.levels {
        None => json!(items::where_used(&st.db, &me, item.id, items::MAX_WHERE_USED_ROWS).await?),
        Some(levels) => json!(items::where_used_levels(&st.db, &me, item.id, levels).await?),
    });
    let hidden = items::hidden_parents(&st.db, &me, item.id).await?;
    Ok(with_hidden_count(body, hidden))
}

#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
}

/// Audit log entries of an item and all its revisions, newest first.
async fn item_history(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let mut ids: Vec<uuid::Uuid> = items::revisions(&st.db, &me, item.id)
        .await?
        .iter()
        .map(|r| r.id)
        .collect();
    ids.push(item.id);
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    Ok(Json(json!(
        audit::for_entities(&st.db, &me, &ids, limit).await?
    )))
}

/// Resolves `/items/{number}/revisions/{code}`; `latest` selects the newest revision.
/// Items the caller may not read answer exactly like unknown numbers.
async fn revision_at(st: &AppState, me: &Actor, number: &str, code: &str) -> AppResult<Revision> {
    let item = items::get_by_number(&st.db, me, number).await?;
    if code.eq_ignore_ascii_case("latest") {
        items::latest_revision(&st.db, me, item.id).await
    } else {
        revisions::get_by_code(&st.db, me, item.id, code).await
    }
}

async fn get_revision(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let attachments = files::list(&st.db, &me, rev.id).await?;
    let lines = bom::lines(&st.db, &me, rev.id).await?;
    let (rev_changes, hidden_changes) = changes::for_revision(&st.db, &me, rev.id).await?;
    let open_requests = requests::open_for_item(&st.db, &me, rev.item_id).await?;
    Ok(Json(json!({
        "revision": rev, "files": attachments, "bom": lines, "changes": rev_changes,
        "hidden_changes": hidden_changes,
        "open_requests": open_requests,
    })))
}

#[derive(Deserialize)]
struct UpdateRevision {
    name: Option<String>,
    description: Option<String>,
    material: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    mass_kg: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option")]
    unit_cost: Option<Option<f64>>,
    /// Custom attribute changes by key; `null` or "" removes a value, keys not
    /// listed keep theirs.
    #[serde(default)]
    attributes: serde_json::Map<String, Value>,
}

async fn update_revision(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<UpdateRevision>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let updated = revisions::update(
        &st.db,
        &me,
        rev.id,
        revisions::RevisionUpdate {
            name: body.name.unwrap_or(rev.name),
            description: body.description.unwrap_or(rev.description),
            material: body.material.unwrap_or(rev.material),
            mass_kg: body.mass_kg.unwrap_or(rev.mass_kg),
            unit_cost: body.unit_cost.unwrap_or(rev.unit_cost),
            attributes: attribute_changes(&body.attributes)?,
        },
    )
    .await?;
    Ok(Json(json!(updated)))
}

async fn checkout(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(revisions::checkout(&st.db, &me, rev.id).await?)))
}

/// Check-in. Without a body it just releases the lock; with a
/// `multipart/form-data` body (fields `file` and optional `comment`) the files
/// become new versions and the lock is released in the same transaction.
async fn checkin(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    req: Request,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    if rev.checked_out_by != Some(me.id) {
        return Err(AppError::Conflict(format!(
            "You have not checked out {}.",
            revisions::label(&rev)
        )));
    }
    let is_multipart = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("multipart/form-data"));
    let mut received = if is_multipart {
        let mut multipart = Multipart::from_request(req, &st)
            .await
            .map_err(|e| AppError::BadRequest(e.body_text()))?;
        web::receive_uploads(&st, &mut multipart).await?
    } else {
        web::Received::default()
    };
    // Large files sent before as chunked uploads (field `uploads`).
    let sessions = web::take_session_uploads(&st, &me, rev.id, &mut received).await?;
    let (rev, stored) =
        files::check_in(&st.db, &me, rev.id, &received.uploads, &received.comment).await?;
    uploads::forget(&st.db, &st.vault, &sessions).await;
    web::warm_previews(&st, &received.uploads);
    let changed: Vec<Value> = stored
        .iter()
        .map(|s| json!({ "file_id": s.file_id, "filename": s.filename, "version": s.version, "changed": s.changed }))
        .collect();
    Ok(Json(json!({ "revision": rev, "files": changed })))
}

async fn cancel_checkout(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(
        revisions::cancel_checkout(&st.db, &me, rev.id).await?
    )))
}

async fn revise(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let new_rev = revisions::revise(&st.db, &me, rev.id).await?;
    Ok((StatusCode::CREATED, Json(json!(new_rev))).into_response())
}

async fn obsolete(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(revisions::obsolete(&st.db, &me, rev.id).await?)))
}

// ---------------------------------------------------------------------------
// BOM
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BomQuery {
    rule: Option<String>,
    /// `true` returns the expanded multi-level tree with roll-ups.
    expand: Option<bool>,
}

async fn get_bom(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<BomQuery>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    if q.expand.unwrap_or(false) {
        let rule = bom_rule(q.rule.as_deref())?;
        // serialized straight from the tree: an expanded BOM can be tens of
        // megabytes, an intermediate JSON value would double the work
        Ok(Json(bom::tree(&st.db, &me, rev.id, rule).await?).into_response())
    } else {
        Ok(Json(bom::lines(&st.db, &me, rev.id).await?).into_response())
    }
}

fn bom_rule(raw: Option<&str>) -> AppResult<RevisionRule> {
    match raw {
        Some(r) => RevisionRule::parse(r).ok_or_else(|| {
            AppError::BadRequest("rule must be latest, released or as_released.".into())
        }),
        None => Ok(RevisionRule::default()),
    }
}

/// The expanded BOM as Excel workbook.
async fn get_bom_xlsx(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<BomQuery>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let rule = bom_rule(q.rule.as_deref())?;
    let tree = bom::tree(&st.db, &me, rev.id, rule).await?;
    let filename = format!(
        "{}_{}_bom_{}.xlsx",
        rev.item_number,
        rev.code,
        rule.as_str()
    );
    Ok(web::xlsx_response(bom::to_xlsx(&tree)?, &filename))
}

/// BOM import that creates a new top-level assembly (multipart: `file`,
/// `project`, `name`, `numbers`, `match_existing`, `dry_run`).
async fn import_bom(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    mut multipart: Multipart,
) -> AppResult<Response> {
    bom_import(&st, &me, None, &mut multipart).await
}

/// BOM import into a revision the caller has checked out.
async fn import_bom_into(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    bom_import(&st, &me, Some(rev.id), &mut multipart).await
}

/// `dry_run=true` returns the plan (what the preview shows), otherwise the
/// import runs in one transaction and returns what it created.
async fn bom_import(
    st: &AppState,
    me: &crate::auth::Actor,
    target: Option<Uuid>,
    multipart: &mut Multipart,
) -> AppResult<Response> {
    let (file, fields) = web::receive_import(multipart).await?;
    let project_id = match fields.get("project").map(|p| p.trim()) {
        Some(code) if !code.is_empty() => Some(projects::get_by_code(&st.db, me, code).await?.id),
        _ => None,
    };
    let options = web::import_options(&fields, target, project_id)?;
    let (filename, content) = file.ok_or_else(|| {
        AppError::BadRequest(
            "Send the BOM table (CSV or Excel) or the STEP assembly in the multipart field 'file'."
                .into(),
        )
    })?;
    let table = imports::read_table(&filename, &content)?;
    let dry_run = fields
        .get("dry_run")
        .is_some_and(|v| matches!(v.trim(), "true" | "1" | "on" | "yes"));
    if dry_run {
        let mut conn = st.db.acquire().await?;
        let plan = imports::plan(&mut conn, me, &table, &filename, &options).await?;
        return Ok(Json(json!(plan)).into_response());
    }
    let source = imports::step_file(&st.vault, &filename, &content).await?;
    let result = imports::run(&st.db, me, &table, &filename, &options, source.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(json!(result))).into_response())
}

#[derive(Deserialize)]
struct PackageQuery {
    rule: Option<String>,
}

/// "Pack and Go": the revision and its structure as ZIP (files, bom.csv,
/// manifest.json). Default rule: as_released for released revisions,
/// latest otherwise.
async fn get_package(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<PackageQuery>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    if let Some(r) = q.rule.as_deref().filter(|r| !r.trim().is_empty()) {
        RevisionRule::parse(r).ok_or_else(|| {
            AppError::BadRequest("rule must be latest, released or as_released.".into())
        })?;
    }
    let rule = web::package_rule(q.rule.as_deref(), &rev.state);
    web::package_response(package::build(&st.db, &st.vault, &me, rev.id, rule).await?)
}

#[derive(Deserialize)]
struct WorksetQuery {
    rule: Option<String>,
    /// `false` leaves out the items found through references inside CAD files.
    references: Option<bool>,
}

/// The working set for the desktop "Get": the revision, its multi-level BOM
/// under a revision rule (default latest) and the items holding the CAD files
/// they reference – every file with folder, version, SHA-256 and the lock of
/// its revision.
async fn get_workset(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<WorksetQuery>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let rule = bom_rule(q.rule.as_deref().filter(|r| !r.trim().is_empty()))?;
    Ok(Json(json!(
        workset::resolve(&st.db, &me, rev.id, rule, q.references.unwrap_or(true)).await?
    )))
}

#[derive(Deserialize)]
struct SimilarQuery {
    limit: Option<i64>,
    /// Compare proportions and shape only.
    ignore_scale: Option<bool>,
    /// The 3D file to compare by (default: the STEP model, else the first mesh).
    file: Option<String>,
    /// Seconds to wait for a shape signature that is still being computed.
    wait: Option<u64>,
}

/// Similar parts by shape (see `services::similar`).
async fn get_similar(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<SimilarQuery>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let options = similar::Options {
        limit: q.limit.unwrap_or(10).clamp(1, similar::MAX_LIMIT),
        ignore_scale: q.ignore_scale.unwrap_or(false),
    };
    let wait = std::time::Duration::from_secs(q.wait.unwrap_or(30).min(200));
    let report = similar::for_revision(&st, &me, &rev, q.file.as_deref(), options, wait).await?;
    let mut out = json!(report);
    out["item"] = json!(rev.item_number);
    out["revision"] = json!(rev.code);
    Ok(Json(out))
}

#[derive(Deserialize)]
struct AddBomLine {
    child: String,
    quantity: f64,
    find_number: Option<i32>,
    #[serde(default)]
    notes: String,
}

async fn add_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AddBomLine>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let line = bom::add(
        &st.db,
        &me,
        rev.id,
        bom::NewBomLine {
            child_number: body.child,
            quantity: body.quantity,
            find_number: body.find_number,
            notes: body.notes,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(line))).into_response())
}

#[derive(Deserialize)]
struct UpdateBomLine {
    quantity: Option<f64>,
    find_number: Option<i32>,
    notes: Option<String>,
}

async fn update_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateBomLine>,
) -> AppResult<Json<Value>> {
    let current = bom::line(&st.db, &me, id).await?;
    let line = bom::update(
        &st.db,
        &me,
        id,
        bom::BomLineUpdate {
            quantity: body.quantity.unwrap_or(current.quantity),
            find_number: body.find_number.unwrap_or(current.find_number),
            notes: body.notes.unwrap_or(current.notes),
        },
    )
    .await?;
    Ok(Json(json!(line)))
}

async fn delete_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    bom::remove(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// References and change propagation
// ---------------------------------------------------------------------------

/// Everything a revision is connected to besides its BOM: its references to
/// other items, who references the item, the CAD files its files reference
/// (matched against the vault), the files of other items that reference its
/// files, and the impacts on the item.
async fn get_references(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!({
        "outgoing": references::outgoing(&st.db, &me, rev.id).await?,
        "incoming": references::incoming(&st.db, &me, rev.item_id).await?,
        "files": references::files_of_revision(&st.db, &me, rev.id).await?,
        "file_referrers": references::file_referrers(&st.db, &me, rev.id).await?,
        "hidden_incoming": references::incoming_hidden(&st.db, &me, rev.item_id).await?,
        "impacts": impacts::for_item(&st.db, &me, rev.item_id).await?,
    })))
}

#[derive(Deserialize)]
struct AddReference {
    /// Item number of the referenced item.
    item: String,
    /// `drawing`, `specification` or `reference`.
    kind: String,
    #[serde(default)]
    note: String,
}

async fn add_reference(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AddReference>,
) -> AppResult<Response> {
    let kind = ReferenceKind::parse(&body.kind).ok_or_else(|| {
        AppError::BadRequest("kind must be drawing, specification or reference.".into())
    })?;
    let rev = revision_at(&st, &me, &number, &code).await?;
    let reference = references::add(
        &st.db,
        &me,
        rev.id,
        references::NewReference {
            to_number: body.item,
            kind,
            note: body.note,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(reference))).into_response())
}

async fn delete_reference(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    references::remove(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Open impacts on items of my project groups (admins: all).
async fn list_impacts(State(st): State<AppState>, ApiUser(me): ApiUser) -> AppResult<Json<Value>> {
    Ok(Json(json!(impacts::open_for(&st.db, &me).await?)))
}

async fn item_impacts(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    Ok(Json(json!(impacts::for_item(&st.db, &me, item.id).await?)))
}

#[derive(Deserialize)]
struct Acknowledge {
    note: String,
}

async fn acknowledge_impact(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<Acknowledge>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        impacts::acknowledge(&st.db, &me, id, &body.note).await?
    )))
}

#[derive(Deserialize)]
struct AcknowledgeAll {
    note: String,
    /// Only these impacts (the ones the user saw); all open ones without.
    #[serde(default)]
    impacts: Option<Vec<Uuid>>,
}

/// Acknowledges the open impacts on a dependent revision at once.
async fn acknowledge_impacts(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AcknowledgeAll>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(
        impacts::acknowledge_all(&st.db, &me, rev.id, body.impacts.as_deref(), &body.note).await?
    )))
}

/// Impact analysis of a change: the items that depend on its revisions.
async fn change_impact(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Response> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let (dependents, hidden) = changes::impact(&st.db, &me, change.id).await?;
    // serialized straight from the list: a change of widely used parts has
    // tens of thousands of dependents
    Ok(with_hidden_count(Json(dependents), hidden))
}

/// A list answer plus `x-hidden-count`: how many entries were left out
/// because they belong to restricted project groups the caller is not a
/// member of (the list itself keeps its shape).
pub(super) fn with_hidden_count<T: Serialize>(body: Json<T>, hidden: i64) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        "x-hidden-count",
        axum::http::HeaderValue::from(hidden.max(0) as u64),
    );
    response
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

async fn list_files(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(files::list(&st.db, &me, rev.id).await?)))
}

async fn upload_files(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let stored = web::upload_files(&st, &me, rev.id, &mut multipart).await?;
    let mut records = Vec::with_capacity(stored.len());
    for s in &stored {
        records.push(files::get(&st.db, &me, s.file_id).await?);
    }
    Ok((StatusCode::CREATED, Json(json!(records))).into_response())
}

async fn file_versions(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(files::versions(&st.db, &me, id).await?)))
}

#[derive(Deserialize)]
struct RestoreBody {
    version: i32,
}

async fn restore_file(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<RestoreBody>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        files::restore_version(&st.db, &me, id, body.version).await?
    )))
}

async fn download_version(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    web::version_response(&st, &me, id).await
}

/// 3D preview mesh of the current version (binary `TCMESH01`, see `mesh`).
async fn file_mesh(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let file = files::get(&st.db, &me, id).await?;
    let sha = file.sha256.clone();
    web::mesh_response(&st, &file, &sha).await
}

/// 3D preview mesh of a file version.
async fn version_mesh(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (version, _) = files::get_version(&st.db, &me, id).await?;
    let file = files::get(&st.db, &me, version.file_id).await?;
    web::mesh_response(&st, &file, &version.sha256).await
}

async fn file_meta(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(files::get(&st.db, &me, id).await?)))
}

#[derive(Deserialize)]
struct UpdateFile {
    /// Folder inside the project group (`""` = top).
    folder: String,
}

/// Changes the folder of a file while its revision is checked out.
async fn update_file(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateFile>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        files::set_folder(&st.db, &me, id, &body.folder).await?
    )))
}

async fn download(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    web::file_response(&st, &me, id).await
}

/// The preview image embedded in an Inventor, Solid Edge or SolidWorks file.
async fn file_thumbnail(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    web::thumbnail_response(&st, &me, id).await
}

async fn delete_file(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    files::delete(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChangeQuery {
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_changes(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<ChangeQuery>,
) -> AppResult<Json<Value>> {
    let status = match q.status.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            ChangeStatus::parse(s)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown status '{s}'.")))?,
        ),
        None => None,
    };
    let page = Page::new(q.limit, q.offset);
    let (rows, total) = changes::list(&st.db, &me, status, page).await?;
    Ok(Json(
        json!({ "changes": rows, "total": total, "limit": page.limit, "offset": page.offset }),
    ))
}

#[derive(Deserialize)]
struct CreateChange {
    title: String,
    #[serde(default)]
    description: String,
    priority: Option<String>,
}

async fn create_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateChange>,
) -> AppResult<Response> {
    let priority =
        Priority::parse(body.priority.as_deref().unwrap_or("normal")).ok_or_else(|| {
            AppError::BadRequest("priority must be low, normal, high or critical.".into())
        })?;
    let change = changes::create(
        &st.db,
        &me,
        changes::ChangeInput {
            title: body.title,
            description: body.description,
            priority,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(change))).into_response())
}

async fn change_json(st: &AppState, me: &Actor, number: &str) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, number).await?;
    let affected = changes::items(&st.db, me, change.id).await?;
    let reviews = changes::reviews(&st.db, change.id).await?;
    Ok(json!({ "change": change, "items": affected, "reviews": reviews }))
}

async fn get_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let mut value = change_json(&st, &me, &number).await?;
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    value["watching"] =
        json!(notifications::is_watching(&st.db, me.id, "change", change.id).await?);
    Ok(Json(value))
}

// ---------------------------------------------------------------------------
// Notifications and watch lists
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct NotificationQuery {
    unread: Option<bool>,
    limit: Option<i64>,
}

/// My newest notifications and the unread count.
async fn list_notifications(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<NotificationQuery>,
) -> AppResult<Json<Value>> {
    let list = notifications::list(
        &st.db,
        &me,
        q.unread.unwrap_or(false),
        q.limit.unwrap_or(50),
    )
    .await?;
    let unread = notifications::unread_count(&st.db, &me).await?;
    Ok(Json(json!({ "unread": unread, "notifications": list })))
}

#[derive(Deserialize, Default)]
struct MarkRead {
    /// Notification ids; all if missing.
    ids: Option<Vec<Uuid>>,
}

/// Marks notifications as read: `{ids: [...]}`, or all with an empty body.
async fn read_notifications(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    let ids = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        serde_json::from_slice::<MarkRead>(&body)
            .map_err(|e| AppError::BadRequest(format!("Invalid JSON: {e}")))?
            .ids
    };
    let unread = notifications::mark_read(&st.db, &me, ids.as_deref()).await?;
    Ok(Json(json!({ "unread": unread })))
}

async fn get_notification_settings(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(notifications::settings(&st.db, me.id).await?)))
}

#[derive(Deserialize)]
struct NotificationSettingsBody {
    email: bool,
    mattermost: bool,
    mattermost_username: Option<String>,
    #[serde(default)]
    muted: Vec<String>,
}

async fn put_notification_settings(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<NotificationSettingsBody>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        notifications::update_settings(
            &st.db,
            &me,
            notifications::SettingsUpdate {
                email: body.email,
                mattermost: body.mattermost,
                mattermost_username: body.mattermost_username,
                muted: body.muted,
            },
        )
        .await?
    )))
}

async fn test_notification(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<StatusCode> {
    notifications::send_test(&st.db, &me).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn list_watches(State(st): State<AppState>, ApiUser(me): ApiUser) -> AppResult<Json<Value>> {
    Ok(Json(json!(notifications::watched(&st.db, &me).await?)))
}

async fn watch_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    method: axum::http::Method,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let watching = notifications::set_watch(
        &st.db,
        &me,
        "item",
        item.id,
        method != axum::http::Method::DELETE,
    )
    .await?;
    Ok(Json(json!({ "item": item.number, "watching": watching })))
}

async fn watch_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    method: axum::http::Method,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let watching = notifications::set_watch(
        &st.db,
        &me,
        "change",
        change.id,
        method != axum::http::Method::DELETE,
    )
    .await?;
    Ok(Json(
        json!({ "change": change.number, "watching": watching }),
    ))
}

#[derive(Deserialize)]
struct AddChangeItem {
    /// Item number.
    item: String,
    /// Revision code; defaults to the item's in-work revision.
    revision: Option<String>,
}

async fn add_change_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AddChangeItem>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let rev = match body.revision.as_deref() {
        Some(code) => revision_at(&st, &me, &body.item, code).await?,
        None => {
            let item = items::get_by_number(&st.db, &me, &body.item).await?;
            items::open_revision(&st.db, &me, item.id)
                .await?
                .ok_or_else(|| {
                    AppError::Conflict(format!("{} has no open revision.", item.number))
                })?
        }
    };
    changes::add_item(&st.db, &me, change.id, rev.id).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

#[derive(Deserialize)]
struct AddReviewer {
    username: String,
}

async fn add_change_reviewer(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AddReviewer>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    if users::find_by_username(&st.db, &body.username)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound(format!(
            "Unknown user '{}'.",
            body.username
        )));
    }
    changes::add_reviewer(&st.db, &me, change.id, &body.username).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

/// The release readiness of a change: unreleased (blocking) and obsolete or
/// heuristic (warning) dependencies of its revisions.
async fn change_readiness(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let readiness = changes::readiness(&st.db, &me, change.id).await?;
    Ok(Json(
        json!({ "ready": readiness.is_ready(), "blockers": readiness.blockers, "warnings": readiness.warnings }),
    ))
}

#[derive(Deserialize)]
struct SuggestQuery {
    limit: Option<i64>,
}

/// Who should review the change: ranked users with the reasons.
async fn change_suggested_reviewers(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    Query(q): Query<SuggestQuery>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let limit = q
        .limit
        .unwrap_or(reviewers::DEFAULT_LIMIT as i64)
        .clamp(1, reviewers::MAX_LIMIT as i64) as usize;
    let suggestions = reviewers::suggest(&st.db, &me, change.id, limit).await?;
    Ok(Json(
        json!({ "change": change.number, "suggestions": suggestions }),
    ))
}

/// Adds the in-work revisions of all unreleased dependencies, level by level.
async fn add_change_dependencies(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let added = changes::add_dependencies(&st.db, &me, change.id).await?;
    let mut value = change_json(&st, &me, &number).await?;
    value["added"] = json!(added);
    Ok(Json(value))
}

async fn submit_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    changes::submit(&st.db, &me, change.id).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

#[derive(Deserialize)]
struct ReviewBody {
    decision: String,
    #[serde(default)]
    comment: String,
}

async fn review_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<ReviewBody>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let decision = Decision::parse(&body.decision)
        .filter(|d| *d != Decision::Pending)
        .ok_or_else(|| AppError::BadRequest("decision must be approve or reject.".into()))?;
    changes::review(&st.db, &me, change.id, decision, &body.comment).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

async fn withdraw_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    changes::withdraw(&st.db, &me, change.id).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

// ---------------------------------------------------------------------------
// Concurrency: workspace file status and bulk check-in
// ---------------------------------------------------------------------------

/// A JSON body that may be missing (`None` for an empty body).
fn optional_json<T: DeserializeOwned>(body: &[u8]) -> AppResult<Option<T>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(Some)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))
}

#[derive(Default, Deserialize)]
struct FileStatusBody {
    #[serde(default)]
    files: Vec<checkouts::ClientFile>,
}

/// Status of every file of a revision: current version, SHA-256, who holds
/// the lock – and with a body `{files: [{filename, sha256, base_version? |
/// base_sha256?}]}` the sync state of each local copy (`up_to_date`,
/// `modified`, `outdated`, `conflict`, `new`, `missing`).
async fn file_status(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let body: FileStatusBody = optional_json(&body)?.unwrap_or_default();
    Ok(Json(json!(
        checkouts::file_status(&st.db, &me, rev.id, &body.files).await?
    )))
}

#[derive(Default, Deserialize)]
struct CheckInAllBody {
    /// Only these revisions (ids); all my check-outs without it.
    revisions: Option<Vec<Uuid>>,
}

/// "Check in all my check-outs": releases the locks without new files, one
/// transaction per revision; the result lists every revision with `ok` and
/// `error`.
async fn checkin_all(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    let body: CheckInAllBody = optional_json(&body)?.unwrap_or_default();
    let outcomes = checkouts::check_in_all(&st.db, &me, body.revisions.as_deref()).await?;
    Ok(Json(json!({
        "checked_in": outcomes.iter().filter(|o| o.ok).count(),
        "failed": outcomes.iter().filter(|o| !o.ok).count(),
        "summary": checkouts::summary(&outcomes),
        "revisions": outcomes,
    })))
}
