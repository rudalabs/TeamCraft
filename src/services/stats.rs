//! Figures for the dashboard.

use serde::Serialize;
use sqlx::PgPool;

use crate::{auth::Actor, error::AppResult, services::access};

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

/// Counts over what the actor may see (admins: the whole vault).
pub async fn load(pool: &PgPool, actor: &Actor) -> AppResult<Stats> {
    let item = access::item_visible("i", 1);
    let change = access::change_visible("c", 2, 1);
    Ok(sqlx::query_as::<_, Stats>(&format!(
        "WITH vr AS (SELECT r.id, r.state FROM revisions r JOIN items i ON i.id = r.item_id
                     WHERE {item}),
              vf AS (SELECT f.sha256, f.size_bytes FROM files f JOIN vr ON vr.id = f.revision_id)
         SELECT
            (SELECT COUNT(*) FROM items i WHERE {item}) AS items,
            (SELECT COUNT(*) FROM vr WHERE state = 'in_work') AS revisions_in_work,
            (SELECT COUNT(*) FROM vr WHERE state = 'in_review') AS revisions_in_review,
            (SELECT COUNT(*) FROM vr WHERE state = 'released') AS revisions_released,
            (SELECT COUNT(*) FROM changes c
             WHERE c.status IN ('draft', 'in_review', 'rejected') AND {change}) AS open_changes,
            (SELECT COUNT(*) FROM vf) AS files,
            -- byte-wise comparison: the hex digests sort several times
            -- faster than with the database's linguistic collation
            (SELECT COALESCE(SUM(size_bytes), 0)::bigint FROM
                (SELECT DISTINCT sha256 COLLATE \"C\", size_bytes FROM vf) blobs) AS stored_bytes"
    ))
    .bind(&actor.hidden_projects)
    .bind(actor.id)
    .fetch_one(pool)
    .await?)
}
