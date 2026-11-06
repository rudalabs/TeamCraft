//! Personal working sets: favorites (Teamcenter "Favorites") and saved item
//! searches (Teamcenter "Saved Queries").
//! Both belong to one user only, so they are not audited.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain,
    error::{AppError, AppResult},
    ids::{ItemId, UserId},
    util,
};

/// Keys of the item search that a saved search keeps, in this order.
pub const SEARCH_KEYS: [&str; 8] = [
    "q", "type", "project", "state", "system", "class", "attr", "value",
];

/// Most saved searches per user.
pub const MAX_SAVED_SEARCHES: i64 = 50;

pub async fn is_favorite(pool: &PgPool, user_id: UserId, item_id: ItemId) -> AppResult<bool> {
    let user_id = user_id.uuid();
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM favorites WHERE user_id = $1 AND item_id = $2)",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_one(pool)
    .await?)
}

/// Adds the item to the actor's favorites (`on`) or removes it.
pub async fn set_favorite(pool: &PgPool, actor: &Actor, item_id: Uuid, on: bool) -> AppResult<()> {
    // Hidden items give the same "not found" as missing ones.
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM items i WHERE i.id = $1 AND {})",
        crate::services::access::item_visible("i", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?;
    if !exists {
        return Err(AppError::not_found("Item not found."));
    }
    if on {
        sqlx::query(
            "INSERT INTO favorites (user_id, item_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(actor.id)
        .bind(item_id)
        .execute(pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM favorites WHERE user_id = $1 AND item_id = $2")
            .bind(actor.id)
            .bind(item_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// A saved item search.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct SavedSearch {
    pub id: Uuid,
    pub name: String,
    /// Query string of `/items` (`q=bracket&state=released`).
    pub query: String,
    pub created_at: DateTime<Utc>,
}

/// The normalised query string of a search: only [`SEARCH_KEYS`], empty
/// values dropped, values percent-encoded.
pub fn normalize_query(pairs: &[(&str, &str)]) -> String {
    SEARCH_KEYS
        .iter()
        .filter_map(|key| {
            pairs
                .iter()
                .find(|(k, v)| k == key && !v.trim().is_empty())
                .map(|(_, v)| format!("{key}={}", util::pct_encode(v.trim())))
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub async fn saved_searches(pool: &PgPool, user_id: Uuid) -> AppResult<Vec<SavedSearch>> {
    Ok(sqlx::query_as::<_, SavedSearch>(
        "SELECT id, name, query, created_at FROM saved_searches WHERE user_id = $1 ORDER BY name",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// Saves (or, with an existing name, replaces) a search of the actor.
pub async fn save_search(
    pool: &PgPool,
    actor: &Actor,
    name: &str,
    pairs: &[(&str, &str)],
) -> AppResult<SavedSearch> {
    let name = domain::require_text("Name of the search", name, 100)?;
    let query = normalize_query(pairs);
    if query.is_empty() {
        return Err(AppError::BadRequest(
            "Set a search text or a filter before saving the search.".into(),
        ));
    }
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM saved_searches WHERE user_id = $1 AND name <> $2")
            .bind(actor.id)
            .bind(&name)
            .fetch_one(pool)
            .await?;
    if count >= MAX_SAVED_SEARCHES {
        return Err(AppError::BadRequest(format!(
            "At most {MAX_SAVED_SEARCHES} saved searches – delete one first."
        )));
    }
    Ok(sqlx::query_as::<_, SavedSearch>(
        "INSERT INTO saved_searches (id, user_id, name, query) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, name) DO UPDATE SET query = EXCLUDED.query
         RETURNING id, name, query, created_at",
    )
    .bind(Uuid::new_v4())
    .bind(actor.id)
    .bind(&name)
    .bind(&query)
    .fetch_one(pool)
    .await?)
}

pub async fn delete_search(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<()> {
    let deleted = sqlx::query("DELETE FROM saved_searches WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(actor.id)
        .execute(pool)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(AppError::not_found("Saved search not found."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_normalised() {
        assert_eq!(
            normalize_query(&[
                ("state", "released"),
                ("q", " a b "),
                ("page", "3"),
                ("type", "")
            ]),
            "q=a%20b&state=released"
        );
        assert_eq!(normalize_query(&[("page", "2")]), "");
    }
}
