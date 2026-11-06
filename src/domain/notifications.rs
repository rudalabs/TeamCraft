//! Notification kinds and webhook events.

use serde::{Deserialize, Serialize};

/// What a notification is about. Users can mute every kind except `Test`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// You were asked to review a change.
    ReviewRequested,
    /// A change you authored, review or watch was decided, released,
    /// rejected, withdrawn or cancelled.
    ChangeUpdate,
    /// An item you created or watch has to be checked (change propagation).
    Impact,
    /// An item you watch got a new revision, was released, made obsolete or
    /// checked in.
    ItemUpdate,
    /// Somebody else released your check-out.
    Unlock,
    /// A change request you raised, are assigned to, watch or have to triage
    /// was raised, commented, assigned, decided or implemented.
    ChangeRequest,
    /// Test message from the account page.
    Test,
}

impl NotificationKind {
    /// The kinds a user can switch off.
    pub const MUTABLE: [NotificationKind; 6] = [
        NotificationKind::ReviewRequested,
        NotificationKind::ChangeUpdate,
        NotificationKind::Impact,
        NotificationKind::ItemUpdate,
        NotificationKind::Unlock,
        NotificationKind::ChangeRequest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::ReviewRequested => "review_requested",
            NotificationKind::ChangeUpdate => "change_update",
            NotificationKind::Impact => "impact",
            NotificationKind::ItemUpdate => "item_update",
            NotificationKind::Unlock => "unlock",
            NotificationKind::ChangeRequest => "change_request",
            NotificationKind::Test => "test",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NotificationKind::ReviewRequested => "Review requests",
            NotificationKind::ChangeUpdate => "Changes I authored, review or watch",
            NotificationKind::Impact => "Checks needed on my items (change propagation)",
            NotificationKind::ItemUpdate => "Items I watch: new revisions, releases, check-ins",
            NotificationKind::Unlock => "Somebody released my check-out",
            NotificationKind::ChangeRequest => {
                "Change requests I raised, triage, am assigned to or watch"
            }
            NotificationKind::Test => "Test messages",
        }
    }

    pub fn parse(s: &str) -> Option<NotificationKind> {
        [
            NotificationKind::ReviewRequested,
            NotificationKind::ChangeUpdate,
            NotificationKind::Impact,
            NotificationKind::ItemUpdate,
            NotificationKind::Unlock,
            NotificationKind::ChangeRequest,
            NotificationKind::Test,
        ]
        .into_iter()
        .find(|k| k.as_str() == s.trim())
    }
}

// ---------------------------------------------------------------------------
// Webhooks
// ---------------------------------------------------------------------------

/// An event an outgoing webhook can subscribe to (`webhooks.events`; the
/// CHECK constraint in migration 0022 lists the same names).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebhookEvent {
    #[serde(rename = "revision.released")]
    RevisionReleased,
    #[serde(rename = "revision.obsoleted")]
    RevisionObsoleted,
    #[serde(rename = "change.submitted")]
    ChangeSubmitted,
    #[serde(rename = "change.approved")]
    ChangeApproved,
    #[serde(rename = "change.rejected")]
    ChangeRejected,
    #[serde(rename = "change.released")]
    ChangeReleased,
    #[serde(rename = "change_request.created")]
    ChangeRequestCreated,
    #[serde(rename = "change_request.decided")]
    ChangeRequestDecided,
    #[serde(rename = "simulation.result_added")]
    SimulationResultAdded,
}

impl WebhookEvent {
    pub const ALL: [WebhookEvent; 9] = [
        WebhookEvent::RevisionReleased,
        WebhookEvent::RevisionObsoleted,
        WebhookEvent::ChangeSubmitted,
        WebhookEvent::ChangeApproved,
        WebhookEvent::ChangeRejected,
        WebhookEvent::ChangeReleased,
        WebhookEvent::ChangeRequestCreated,
        WebhookEvent::ChangeRequestDecided,
        WebhookEvent::SimulationResultAdded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WebhookEvent::RevisionReleased => "revision.released",
            WebhookEvent::RevisionObsoleted => "revision.obsoleted",
            WebhookEvent::ChangeSubmitted => "change.submitted",
            WebhookEvent::ChangeApproved => "change.approved",
            WebhookEvent::ChangeRejected => "change.rejected",
            WebhookEvent::ChangeReleased => "change.released",
            WebhookEvent::ChangeRequestCreated => "change_request.created",
            WebhookEvent::ChangeRequestDecided => "change_request.decided",
            WebhookEvent::SimulationResultAdded => "simulation.result_added",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WebhookEvent::RevisionReleased => "A revision was released",
            WebhookEvent::RevisionObsoleted => "A revision was made obsolete",
            WebhookEvent::ChangeSubmitted => "A change was submitted for review",
            WebhookEvent::ChangeApproved => "A reviewer approved a change",
            WebhookEvent::ChangeRejected => "A change was rejected",
            WebhookEvent::ChangeReleased => "A change was released",
            WebhookEvent::ChangeRequestCreated => "A change request was raised",
            WebhookEvent::ChangeRequestDecided => {
                "A change request was accepted, rejected or deferred"
            }
            WebhookEvent::SimulationResultAdded => "Simulation result values were added",
        }
    }

    pub fn parse(s: &str) -> Option<WebhookEvent> {
        WebhookEvent::ALL
            .into_iter()
            .find(|e| e.as_str() == s.trim())
    }
}
