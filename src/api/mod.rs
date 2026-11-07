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
mod tokens_api;
pub(crate) mod uploads_api;
mod webhooks;

mod bom_api;
mod changes_api;
mod concurrency_api;
mod files_api;
mod items_api;
mod notifications_api;
mod projects_api;
mod references_api;

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

use bom_api::*;
use changes_api::*;
use concurrency_api::*;
use files_api::*;
use items_api::*;
use notifications_api::*;
use projects_api::*;
use references_api::*;

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
        .route(
            "/tokens",
            get(tokens_api::list_tokens).post(tokens_api::create_token),
        )
        .route("/tokens/{id}", delete(tokens_api::revoke_token))
        .route("/worklist", get(worklist))
        .route("/worklist/checkin-all", post(checkin_all))
        .route("/mcp", post(crate::mcp::http).get(crate::mcp::http_get))
        .route("/assistant", get(assistant_info).post(assistant_ask))
        .route(
            "/assistant/thread",
            get(assistant_thread).delete(assistant_thread_reset),
        )
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
            "/items/{number}/revisions/{code}/simulation/proposals",
            get(simulations::list_proposals),
        )
        .route(
            "/simulation-proposals/{id}/accept",
            post(simulations::accept_proposal),
        )
        .route(
            "/simulation-proposals/{id}/reject",
            post(simulations::reject_proposal),
        )
        .route(
            "/items/{number}/revisions/{code}/simulation/compare",
            get(simulations::compare),
        )
        .route(
            "/items/{number}/simulations",
            get(simulations::item_simulations),
        )
        .route(
            "/projects/{code}/result-templates",
            get(simulations::list_templates).post(simulations::create_template),
        )
        .route(
            "/result-templates/{id}",
            put(simulations::put_template).delete(simulations::delete_template),
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
            "GET    /api/v1/me                                        the caller, its groups and token_scope",
            "GET    /api/v1/worklist                                  my check-outs, reviews waiting for me, my open changes, change requests assigned to me and raised by me, dependents to check (impacts, and grouped by dependent revision as impact_groups), statistics",
            "POST   /api/v1/desktop/token                             {code, verifier} – desktop sign-in, no token needed",
            "GET    /api/v1/tokens                                    your personal access tokens (name, prefix, scope, expiry; never the value)",
            "POST   /api/v1/tokens                                    {name, expires_in_days?: 1..3650 (absent = never), scope?: full|read_only|mcp_only} – the value is answered once",
            "DELETE /api/v1/tokens/{id}                               revoke a token (your own; admins: any)",
            "GET    /api/v1/projects                                  the groups you may see (restricted groups only for members and admins)",
            "POST   /api/v1/projects                                  {code, name, description?, oidc_group?, visibility?}",
            "PUT    /api/v1/projects/{code}/visibility                {visibility: internal|restricted} – managers and admins",
            "GET    /api/v1/projects/{code}/members",
            "POST   /api/v1/projects/{code}/members                   {username, manager?} – managers and admins",
            "GET    /api/v1/projects/{code}/numbering?scheme=         next generated item numbers per type; scheme = preview another scheme",
            "PUT    /api/v1/projects/{code}/numbering                 {scheme} e.g. {PROJECT}-{TYPE}-{YYYY}-{SEQ:4}, empty = {TYPE}-{SEQ:6} – managers and admins",
            "POST   /api/v1/mcp                                       Model Context Protocol (JSON-RPC 2.0) for AI assistants, see the MCP reference",
            "GET    /api/v1/assistant                                 whether the built-in assistant is enabled, and its quick actions",
            "POST   /api/v1/assistant                                 {question?, action?: ask|release_notes|release_check|review_brief|attention|impact|simulation_status|result_values, context: {kind?, item?, revision?, change?, request?, project?, view?, rule?, query?, filters?, selected?, shown?}, allow_drafts?, thread?}",
            "GET    /api/v1/assistant/thread                          the caller's conversation (thread: true questions), newest message last",
            "DELETE /api/v1/assistant/thread                          new conversation: deletes the caller's thread",
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
            "GET    /api/v1/items/{number}/revisions/{code}/simulation/proposals?status=   values proposed by the assistant from reports (pending|accepted|rejected|all)",
            "POST   /api/v1/simulation-proposals/{id}/accept          makes a proposed value a result value – CAD editors, revision checked out",
            "POST   /api/v1/simulation-proposals/{id}/reject",
            "GET    /api/v1/items/{number}/revisions/{code}/simulation/compare?with=A   result values of two revisions",
            "GET    /api/v1/items/{number}/simulations                simulations based on the item (out of date?)",
            "GET    /api/v1/projects/{code}/result-templates         extraction templates of the group (see the simulation guide)",
            "POST   /api/v1/projects/{code}/result-templates         {name, file_pattern, method: regex|csv, pattern | csv_column [csv_key_column, csv_key_value], pick?: first|last, unit?, lower_limit?, upper_limit?, load_case?, solver?, enabled?} – managers and admins",
            "PUT    /api/v1/result-templates/{id}                    the same fields; replaces the template",
            "DELETE /api/v1/result-templates/{id}",
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
            "GET    /api/v1/files/{id}/content                   resumable: Range: bytes=N- with If-Range: \"<sha256>\" (ETag) → 206",
            "GET    /api/v1/files/{id}/thumbnail                      preview image stored in Inventor, Solid Edge and SolidWorks files (PNG or BMP, 404 if none)",
            "GET    /api/v1/files/{id}/versions",
            "POST   /api/v1/files/{id}/restore                        {version}",
            "GET    /api/v1/file-versions/{id}/content           resumable like /files/{id}/content",
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

async fn me(
    State(st): State<AppState>,
    crate::auth::ApiCaller { actor: me, scope }: crate::auth::ApiCaller,
) -> AppResult<Json<Value>> {
    // `hidden_projects` is never serialized; the client learns only which of
    // its own groups are restricted (for a badge).
    let mut body = json!(me);
    body["restricted_projects"] = json!(projects::restricted_memberships(&st.db, &me).await?);
    body["token_scope"] = json!(scope);
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
        "provider": st.assistant.as_ref().map(|a| a.provider_name),
        // where the questions go (host only, never the key or a path)
        "host": st.assistant.as_ref().map(|a| a.host.clone()),
        "max_question_chars": crate::assistant::MAX_QUESTION_CHARS,
        "actions": actions,
    }))
}

#[derive(Deserialize)]
struct AssistantBody {
    question: Option<String>,
    action: Option<String>,
    /// The page the caller is on (`crate::pagecontext`): identifiers are
    /// hints, resolved with the caller's rights.
    #[serde(default)]
    context: Value,
    #[serde(default)]
    allow_drafts: bool,
    /// Ask in the caller's conversation (stored, follow-ups see it).
    #[serde(default)]
    thread: bool,
}

/// `POST /api/v1/assistant`: runs the assistant with the caller's rights.
async fn assistant_ask(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<AssistantBody>,
) -> AppResult<Json<Value>> {
    let run = crate::services::assistant::chat(
        &st,
        &me,
        body.action.as_deref(),
        body.question.as_deref(),
        &body.context,
        body.allow_drafts,
        body.thread,
    )
    .await?;
    Ok(Json(json!(run)))
}

/// `GET /api/v1/assistant/thread`: the caller's conversation.
async fn assistant_thread(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    let Some(client) = st.assistant.as_deref() else {
        return Err(crate::assistant::AssistantError::Disabled.into_app_error());
    };
    let thread =
        crate::services::assistant_threads::load(&st.db, &me, client.retention_days).await?;
    Ok(Json(json!(thread)))
}

/// `DELETE /api/v1/assistant/thread`: a new conversation.
async fn assistant_thread_reset(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    if !crate::services::assistant::enabled(&st) {
        return Err(crate::assistant::AssistantError::Disabled.into_app_error());
    }
    crate::services::assistant_threads::reset(&st.db, &me).await?;
    Ok(Json(json!({ "deleted": true })))
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
