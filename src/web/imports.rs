//! BOM import from CAD exports: upload → preview → import (see
//! [`crate::services::imports`]).

use std::collections::HashMap;

use axum::{
    extract::{Multipart, Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    finish, is_user_error, redirect_flash, render, revisions::multipart_error, Flash, WebCtx,
};
use crate::ids::RevisionId;
use crate::{
    domain::{self, Role},
    error::{AppError, AppResult},
    services::{
        imports::{self, ImportOptions, NumberSource, Plan},
        projects, revisions,
    },
    AppState,
};

#[derive(Deserialize)]
pub struct UploadQuery {
    into: Option<Uuid>,
}

fn upload_page(target: Option<Uuid>) -> String {
    match target {
        Some(id) => format!("/import?into={id}"),
        None => "/import".into(),
    }
}

/// GET /import – choose a file and the options.
pub async fn upload_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<UploadQuery>,
) -> AppResult<Response> {
    ctx.me.require(Role::Editor)?;
    let target = match q.into {
        Some(id) => Some(revisions::get(&st.db, &ctx.me, RevisionId(id)).await?),
        None => None,
    };
    let projects = projects::editable_by(&st.db, &ctx.me).await?;
    render(
        &st,
        &ctx,
        "imports/upload.html",
        json!({
            "target": target,
            "projects": projects,
            "max_mib": imports::MAX_TABLE_BYTES / 1024 / 1024,
            "max_step_mib": imports::MAX_STEP_BYTES / 1024 / 1024,
        }),
    )
}

/// The file and the other fields of the upload form.
pub(crate) async fn receive(
    multipart: &mut Multipart,
) -> AppResult<(Option<(String, Vec<u8>)>, HashMap<String, String>)> {
    let mut file = None;
    let mut fields = HashMap::new();
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };
        if name == "file" {
            let filename = field.file_name().map(str::to_string).unwrap_or_default();
            let max = imports::max_bytes(&filename);
            let mut data = Vec::new();
            while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                if data.len() + chunk.len() > max {
                    return Err(imports::too_large(&filename));
                }
                data.extend_from_slice(&chunk);
            }
            if !filename.trim().is_empty() {
                file = Some((filename, data));
            }
        } else {
            fields.insert(name, field.text().await.map_err(multipart_error)?);
        }
    }
    Ok((file, fields))
}

/// Import options from form fields (web form and REST API).
pub(crate) fn options_from(
    fields: &HashMap<String, String>,
    target: Option<Uuid>,
    project_id: Option<Uuid>,
) -> AppResult<ImportOptions> {
    let flag = |name: &str, default: bool| match fields.get(name).map(|v| v.trim()) {
        None => default,
        Some(v) => matches!(v, "on" | "true" | "1" | "yes"),
    };
    Ok(ImportOptions {
        target,
        project_id,
        name: fields.get("name").cloned().unwrap_or_default(),
        numbers: NumberSource::parse(fields.get("numbers").map_or("", String::as_str))?,
        // Web forms send the checkbox only when it is ticked (see `match_existing_sent`).
        match_existing: flag(
            "match_existing",
            !fields.contains_key("match_existing_sent"),
        ),
    })
}

/// POST /import – stages the upload and shows the preview.
pub async fn upload(
    State(st): State<AppState>,
    ctx: WebCtx,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let received = receive(&mut multipart).await;
    let target = received
        .as_ref()
        .ok()
        .and_then(|(_, f)| f.get("into"))
        .and_then(|v| Uuid::parse_str(v.trim()).ok());
    let back = upload_page(target);
    let result = async {
        let (file, fields) = received?;
        let project_id =
            super::parse_opt_uuid("Project", fields.get("project_id").map(String::as_str))?;
        let options = options_from(&fields, target, project_id)?;
        let (filename, content) = file.ok_or_else(|| {
            AppError::BadRequest(
                "Choose the BOM file (CSV, Excel or STEP assembly) to import.".into(),
            )
        })?;
        imports::stage(&st.db, &st.vault, &ctx.me, &filename, content, options).await
    }
    .await;
    finish(&st, result, &back, |id| {
        (
            format!("/import/{id}"),
            "Check the preview, then start the import.".into(),
        )
    })
}

/// Rows of the preview table, in file order.
fn preview_rows(plan: &Plan) -> Vec<Value> {
    plan.lines
        .iter()
        .map(|l| {
            let it = &plan.items[l.child];
            let indent = match l.level {
                1 => String::new(),
                2..=6 => format!("indent-{}", l.level),
                _ => "indent-deep".into(),
            };
            json!({
                "row": l.row,
                "level": l.level,
                "indent": indent,
                "find_number": l.find_number,
                "file_number": it.file_number,
                "number": it.number,
                "existing": it.existing_label,
                "type": it.item_type.label(),
                "name": it.name,
                "quantity": l.quantity,
                "unit": it.unit,
                "material": it.material,
                "mass_kg": it.mass_kg,
                "classification": if it.existing.is_none() && it.classification != domain::Classification::Make {
                    it.classification.label()
                } else {
                    ""
                },
                "attributes": it.attributes,
            })
        })
        .collect()
}

/// GET /import/{id} – what the import will do.
pub async fn preview(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    match imports::preview(&st.db, &st.vault, &ctx.me, id).await {
        Ok((_, plan)) => render(
            &st,
            &ctx,
            "imports/preview.html",
            json!({ "id": id, "rows": preview_rows(&plan), "plan": plan }),
        ),
        Err(e) if is_user_error(&e) => {
            // The upload cannot be imported as it is: start over.
            let options = imports::discard(&st.db, &ctx.me, id).await?;
            Ok(redirect_flash(
                &st,
                &upload_page(options.target),
                Flash::error(e.public_message()),
            ))
        }
        Err(e) => Err(e),
    }
}

/// POST /import/{id}/run
pub async fn run(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let result = imports::run_staged(&st.db, &st.vault, &ctx.me, id).await;
    finish(&st, result, &format!("/import/{id}"), |r| {
        let message = format!(
            "Imported {} BOM lines into {}: {} new items, {} existing items used.",
            r.lines,
            r.root,
            r.created.len(),
            r.existing_items
        );
        (format!("/revisions/{}#bom", r.root_revision_id), message)
    })
}

/// POST /import/{id}/discard
pub async fn discard(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let options = imports::discard(&st.db, &ctx.me, id).await?;
    let to = match options.target {
        Some(rev) => format!("/revisions/{rev}#bom"),
        None => "/items".into(),
    };
    Ok(redirect_flash(
        &st,
        &to,
        Flash::success("Import discarded."),
    ))
}
