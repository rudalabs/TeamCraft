//! Engineering change requests (ECR): the step *before* a change order.
//!
//! ```text
//!   anybody signed in            CAD editors of the affected groups / admins
//!   ─────────────────            ───────────────────────────────────────────
//!   raise ──▶ new ──▶ in_triage ──▶ accepted ──▶ create / link change order (ECO)
//!              │          │    ──▶ rejected               │
//!              │          │    ──▶ deferred ──▶ in_triage │ change order released
//!              └──────────┴──▶ withdrawn (requester)      ▼
//!                                                    implemented
//! ```
//!
//! Anyone who finds a problem – shop floor, simulation, test, purchasing,
//! viewers included – raises a request and discusses it. CAD editors of the
//! project groups of the affected items (or admins) triage and decide. An
//! accepted request is implemented by a change order: either one created
//! from the request (with the in-work revisions of the affected items) or an
//! existing open one that is linked. When a linked change order is released,
//! the request becomes *implemented* in the same transaction
//! (`on_change_released`, called from `changes::review`).

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{
        self, NotificationKind, Priority, RequestDecision, RequestReason, RequestStatus, Role,
        WebhookEvent,
    },
    error::{AppError, AppResult},
    models::{Change, ChangeRequest, RequestAttachment, RequestComment, RequestItem},
    services::{
        access, audit, changes, next_counter,
        notifications::{self, Notice},
        users, webhooks, Page,
    },
    vault::Vault,
};

mod attachment_files;
mod implementation;
mod notices;
mod permissions;
mod queries;
mod raising;
#[cfg(test)]
mod tests;
mod triage;

pub use attachment_files::*;
pub use implementation::*;
use notices::*;
pub use permissions::*;
pub use queries::*;
pub use raising::*;
pub use triage::*;

/// Largest attachment (screenshot, mark-up, PDF report).
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
/// At most this many attachments per request.
pub const MAX_ATTACHMENTS: i64 = 20;
/// At most this many affected items per request.
pub const MAX_ITEMS: i64 = 100;

const REQUEST_SELECT: &str = "
    SELECT cr.id, cr.number, cr.title, cr.description, cr.reason, cr.priority, cr.status,
           cr.requested_by, rq.display_name AS requested_by_name,
           cr.assignee, asg.display_name AS assignee_name, asg.username AS assignee_username,
           cr.decision_comment, cr.decided_by, dby.display_name AS decided_by_name, cr.decided_at,
           cr.due_date, cr.created_at, cr.updated_at, cr.closed_at,
           (SELECT COUNT(*) FROM change_request_items x WHERE x.request_id = cr.id) AS item_count
    FROM change_requests cr
    JOIN users rq ON rq.id = cr.requested_by
    LEFT JOIN users asg ON asg.id = cr.assignee
    LEFT JOIN users dby ON dby.id = cr.decided_by";

const OPEN_STATES: &str = "('new', 'in_triage', 'accepted', 'deferred')";

pub fn status_of(request: &ChangeRequest) -> RequestStatus {
    RequestStatus::parse(&request.status).unwrap_or(RequestStatus::Withdrawn)
}
