//! Figures for the dashboard.

use serde::Serialize;
use sqlx::PgPool;

use crate::error::AppResult;

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Stats {
    pub items: i64,
    pub revisions_in_work: i64,
    pub revisions_in_review: i64,
    pub revisions_released: i64,
    pub open_changes: i64,
    pub files: i64,
    pub stored_bytes: i64,
}

pub async fn load(pool: &PgPool) -> AppResult<Stats> {
    Ok(sqlx::query_as::<_, Stats>(
        "SELECT
            (SELECT COUNT(*) FROM items) AS items,
            (SELECT COUNT(*) FROM revisions WHERE state = 'in_work') AS revisions_in_work,
            (SELECT COUNT(*) FROM revisions WHERE state = 'in_review') AS revisions_in_review,
            (SELECT COUNT(*) FROM revisions WHERE state = 'released') AS revisions_released,
            (SELECT COUNT(*) FROM changes WHERE status IN ('draft', 'in_review', 'rejected')) AS open_changes,
            (SELECT COUNT(*) FROM files) AS files,
            -- byte-wise comparison: the hex digests sort several times
            -- faster than with the database's linguistic collation
            (SELECT COALESCE(SUM(size_bytes), 0)::bigint FROM
                (SELECT DISTINCT sha256 COLLATE \"C\", size_bytes FROM files) blobs) AS stored_bytes",
    )
    .fetch_one(pool)
    .await?)
}
