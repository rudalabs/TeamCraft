//! Help page: the file formats TeamCraft recognises (see `cadformat`).

use axum::{extract::State, response::Response};
use serde_json::json;

use super::{render, WebCtx};
use crate::{cadformat, error::AppResult, preview::PreviewFormat, AppState};

pub async fn show(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    // Extensions with a browser 3D preview, e.g. ".step".
    let previews: Vec<String> = cadformat::FORMATS
        .iter()
        .filter(|f| PreviewFormat::of_format(f.id).is_some())
        .flat_map(|f| f.extensions.iter().map(|e| format!(".{e}")))
        .collect();
    render(
        &st,
        &ctx,
        "formats.html",
        json!({ "systems": cadformat::catalogue(), "previews": previews }),
    )
}
