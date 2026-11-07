//! Instance logos: the public image route and the administration page
//! (`/admin/branding`).

use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;

use super::{finish, render, revisions::multipart_error, WebCtx};
use crate::{
    domain::Role,
    error::{AppError, AppResult},
    services::branding::{self, Kind},
    AppState,
};

/// The image of a slot with cache headers; 404 when the built-in logo is in
/// use (clients then show their own copy). Shared by the web route and the
/// REST API.
pub(crate) async fn image(st: &AppState, kind: &str, request: &HeaderMap) -> AppResult<Response> {
    let kind = Kind::parse(kind).ok_or_else(|| AppError::not_found("No such logo."))?;
    let logo = branding::get(&st.db, kind)
        .await?
        .ok_or_else(|| AppError::not_found("The built-in logo is in use."))?;
    let etag = format!("\"{}\"", logo.sha256);
    let not_modified = request
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        logo.data.into_response()
    };
    let h = response.headers_mut();
    if !not_modified {
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&logo.content_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        );
    }
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    // the ?v= token in the pages changes with the file
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300, must-revalidate"),
    );
    // an SVG opened directly runs in an empty sandbox
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    Ok(response)
}

pub async fn logo(
    State(st): State<AppState>,
    Path(kind): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    image(&st, &kind, &headers).await
}

pub async fn page(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    let slots: Vec<_> = Kind::ALL
        .iter()
        .map(|k| {
            json!({
                "kind": k.as_str(),
                "label": k.label(),
                "custom": ctx.branding.get(*k),
                "hint": match k {
                    Kind::Wide => "Top left of the header and the desktop title bar. About 5:1 (for example 600 × 120 px), transparent background.",
                    Kind::Square => "Bottom left of the page and the desktop navigation rail, and the tab icon. 1:1 (for example 256 × 256 px).",
                },
            })
        })
        .collect();
    render(
        &st,
        &ctx,
        "admin/branding.html",
        json!({
            "slots": slots,
            "max_kib": branding::MAX_LOGO_BYTES / 1024,
            "max_pixels": branding::MAX_LOGO_PIXELS,
        }),
    )
}

pub async fn upload(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(kind): Path<String>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let result = async {
        ctx.me.require(Role::Admin)?;
        let kind = Kind::parse(&kind).ok_or_else(|| AppError::not_found("No such logo."))?;
        let mut data = None;
        while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
            if field.name() == Some("file") {
                data = Some(field.bytes().await.map_err(multipart_error)?);
            }
        }
        let data = data.ok_or_else(|| AppError::BadRequest("Choose an image file.".into()))?;
        branding::set(&st.db, &ctx.me, kind, &data).await?;
        Ok(kind)
    }
    .await;
    finish(&st, result, "/admin/branding", |k| {
        (
            "/admin/branding".to_string(),
            format!("The {} logo was replaced.", k.as_str()),
        )
    })
}

pub async fn reset(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(kind): Path<String>,
) -> AppResult<Response> {
    let result = async {
        let kind = Kind::parse(&kind).ok_or_else(|| AppError::not_found("No such logo."))?;
        let existed = branding::reset(&st.db, &ctx.me, kind).await?;
        Ok((kind, existed))
    }
    .await;
    finish(&st, result, "/admin/branding", |(k, existed)| {
        (
            "/admin/branding".to_string(),
            if existed {
                format!("The built-in {} logo is back.", k.as_str())
            } else {
                format!("The {} logo was not customised.", k.as_str())
            },
        )
    })
}
