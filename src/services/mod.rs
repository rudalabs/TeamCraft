//! Business logic. Every mutation checks permissions and lifecycle rules,
//! runs in a database transaction and writes an audit log entry.
//!
//! Both the web UI and the REST API call into these functions, so the rules
//! are enforced identically for both.

pub mod access;
pub mod access_log;
pub mod access_review;
pub mod annotations;
pub mod as_built;
pub mod assemblies;
pub mod assistant;
pub mod assistant_threads;
pub mod attributes;
pub mod audit;
pub mod bom;
pub mod bom_compare;
pub mod bom_references;
pub mod branding;
pub mod bulk;
pub mod changes;
pub mod checkouts;
pub mod deactivation;
pub mod desktop;
pub mod duplicates;
pub mod explorer;
pub mod files;
pub mod impacts;
pub mod imports;
pub mod insights;
pub mod interchange;
pub mod items;
pub mod manager_gaps;
pub mod mass_geometry;
pub mod mass_properties;
pub mod membership_conditions;
pub mod notifications;
pub mod offboarding;
pub mod ownership;
pub mod package;
pub mod personal;
pub mod project_policy;
pub mod projects;
pub mod quotas;
pub mod readiness;
pub mod references;
pub mod request_digests;
pub mod requests;
pub mod result_proposals;
pub mod result_templates;
pub mod retention;
pub mod reviewers;
pub mod revisions;
pub mod save_as;
pub mod season;
pub mod similar;
pub mod simulations;
pub mod stats;
pub mod structure;
pub mod uploads;
pub mod users;
pub mod webhooks;
pub mod workset;

use sqlx::PgConnection;

use crate::error::AppResult;

/// Atomically increments and returns the named counter (starting at 1).
pub(crate) async fn next_counter(conn: &mut PgConnection, name: &str) -> AppResult<i64> {
    let value = sqlx::query_scalar::<_, i64>(
        "INSERT INTO counters (name, value) VALUES ($1, 1)
         ON CONFLICT (name) DO UPDATE SET value = counters.value + 1
         RETURNING value",
    )
    .bind(name)
    .fetch_one(&mut *conn)
    .await?;
    Ok(value)
}

/// Pagination helper used by list endpoints.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

impl Page {
    pub const DEFAULT_LIMIT: i64 = 50;
    pub const MAX_LIMIT: i64 = 500;

    pub fn new(limit: Option<i64>, offset: Option<i64>) -> Self {
        Page {
            limit: limit
                .unwrap_or(Self::DEFAULT_LIMIT)
                .clamp(1, Self::MAX_LIMIT),
            offset: offset.unwrap_or(0).max(0),
        }
    }

    /// 1-based page number used by the web UI.
    pub fn from_page_number(page: Option<i64>, per_page: i64) -> Self {
        let page = page.unwrap_or(1).max(1);
        Page {
            limit: per_page,
            offset: (page - 1) * per_page,
        }
    }
}
