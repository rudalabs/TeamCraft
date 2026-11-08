//! Lifecycle operations on many items at once: revise or obsolete the latest
//! revision of every listed item. Each item goes through the same service
//! as a single operation (`revisions::revise` / `revisions::obsolete`) in
//! its own transaction, so permissions, locks, audit records, notifications
//! and webhooks are exactly those of the single operation, and one refusal
//! does not stop the others; the outcome lists every item. (Releasing many
//! items together is a change order with many revisions.)

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::Role,
    error::{AppError, AppResult},
    services::{items, revisions},
};

/// Most items of one bulk request.
pub const MAX_ITEMS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A new revision in work, checked out to the actor (`revision.revise`).
    Revise,
    /// The released latest revision becomes obsolete (`revision.obsolete`).
    Obsolete,
}

impl Action {
    pub fn parse(s: &str) -> Option<Action> {
        match s.trim() {
            "revise" => Some(Action::Revise),
            "obsolete" => Some(Action::Obsolete),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub item_number: String,
    /// The revision the action produced or changed.
    pub revision_id: Option<Uuid>,
    pub code: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
}

/// The message of a refused item: user errors as they are, internal errors
/// only logged.
fn message(number: &str, e: AppError) -> String {
    match e {
        AppError::Database(_) | AppError::Io(_) | AppError::Other(_) => {
            tracing::error!(item = number, error = %e, "bulk operation failed");
            "Internal error – see the server log.".to_string()
        }
        e => e.to_string(),
    }
}

/// The item numbers once each, in the given order (trimmed, upper-case).
pub fn unique_numbers(numbers: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in numbers {
        let n = n.trim().to_uppercase();
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// Applies `action` to the latest revision of every listed item.
///
/// # Invariants
///
/// * **Permission:** CAD editor (403); per item those of the single
///   operation – a hidden or unknown item is reported as "not found".
/// * **Limits:** 1 to [`MAX_ITEMS`] items (400).
/// * **Atomic:** per item; the outcome says which items succeeded.
pub async fn apply(
    pool: &PgPool,
    actor: &Actor,
    action: Action,
    numbers: &[String],
) -> AppResult<Vec<Outcome>> {
    actor.require(Role::Editor)?;
    let numbers = unique_numbers(numbers);
    if numbers.is_empty() {
        return Err(AppError::BadRequest(
            "List at least one item number.".into(),
        ));
    }
    if numbers.len() > MAX_ITEMS {
        return Err(AppError::BadRequest(format!(
            "At most {MAX_ITEMS} items at a time."
        )));
    }
    let mut outcomes = Vec::with_capacity(numbers.len());
    for number in numbers {
        let result = async {
            let item = items::get_by_number(pool, actor, &number).await?;
            let latest = items::latest_revision(pool, actor, item.id).await?;
            match action {
                Action::Revise => revisions::revise(pool, actor, latest.id).await,
                Action::Obsolete => revisions::obsolete(pool, actor, latest.id).await,
            }
        }
        .await;
        outcomes.push(match result {
            Ok(rev) => Outcome {
                item_number: number,
                revision_id: Some(rev.id),
                code: Some(rev.code),
                ok: true,
                error: None,
            },
            Err(e) => Outcome {
                error: Some(message(&number, e)),
                item_number: number,
                revision_id: None,
                code: None,
                ok: false,
            },
        });
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_and_numbers() {
        assert_eq!(Action::parse("revise"), Some(Action::Revise));
        assert_eq!(Action::parse(" obsolete "), Some(Action::Obsolete));
        assert_eq!(Action::parse("release"), None);
        assert_eq!(
            unique_numbers(&[
                " prt-000001".into(),
                "PRT-000001".into(),
                "".into(),
                "ASM-000002".into()
            ]),
            ["PRT-000001", "ASM-000002"]
        );
    }
}
