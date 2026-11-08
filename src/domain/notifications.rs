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
    /// The periodic e-mail of the change requests that wait for triage in a
    /// project group you edit (`services::request_digests`).
    RequestDigest,
    /// Security notice for the managers of a need-to-know group: an
    /// administrator opened an elevation or added themselves. Cannot be muted.
    AccessAlert,
    /// Test message from the account page.
    Test,
}

impl NotificationKind {
    /// The kinds a user can switch off.
    pub const MUTABLE: [NotificationKind; 7] = [
        NotificationKind::ReviewRequested,
        NotificationKind::ChangeUpdate,
        NotificationKind::Impact,
        NotificationKind::ItemUpdate,
        NotificationKind::Unlock,
        NotificationKind::ChangeRequest,
        NotificationKind::RequestDigest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::ReviewRequested => "review_requested",
            NotificationKind::ChangeUpdate => "change_update",
            NotificationKind::Impact => "impact",
            NotificationKind::ItemUpdate => "item_update",
            NotificationKind::Unlock => "unlock",
            NotificationKind::ChangeRequest => "change_request",
            NotificationKind::RequestDigest => "request_digest",
            NotificationKind::AccessAlert => "access_alert",
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
            NotificationKind::RequestDigest => {
                "E-mail digest of new change requests waiting for triage in my project groups"
            }
            NotificationKind::AccessAlert => "Access to a need-to-know group",
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
            NotificationKind::RequestDigest,
            NotificationKind::AccessAlert,
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

/// Found by cargo mutants: the stored names (database CHECK constraints, the
/// webhook payload) and the labels were not tested.
#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [NotificationKind; 9] = [
        NotificationKind::ReviewRequested,
        NotificationKind::ChangeUpdate,
        NotificationKind::Impact,
        NotificationKind::ItemUpdate,
        NotificationKind::Unlock,
        NotificationKind::ChangeRequest,
        NotificationKind::RequestDigest,
        NotificationKind::AccessAlert,
        NotificationKind::Test,
    ];

    #[test]
    fn notification_kinds() {
        let stored: Vec<&str> = KINDS.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            stored,
            [
                "review_requested",
                "change_update",
                "impact",
                "item_update",
                "unlock",
                "change_request",
                "request_digest",
                "access_alert",
                "test"
            ]
        );
        for k in KINDS {
            assert_eq!(NotificationKind::parse(k.as_str()), Some(k));
            assert_eq!(
                NotificationKind::parse(&format!(" {} ", k.as_str())),
                Some(k)
            );
        }
        assert_eq!(NotificationKind::parse("spam"), None);
        // every kind has its own, non-empty label (settings page)
        let mut labels: Vec<&str> = KINDS.iter().map(|k| k.label()).collect();
        assert!(labels.iter().all(|l| l.len() > 3), "{labels:?}");
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), KINDS.len());
        assert_eq!(NotificationKind::ReviewRequested.label(), "Review requests");
        // the security alert cannot be muted, the test message neither
        assert!(!NotificationKind::MUTABLE.contains(&NotificationKind::AccessAlert));
        assert!(!NotificationKind::MUTABLE.contains(&NotificationKind::Test));
    }

    #[test]
    fn webhook_events() {
        let stored: Vec<&str> = WebhookEvent::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            stored,
            [
                "revision.released",
                "revision.obsoleted",
                "change.submitted",
                "change.approved",
                "change.rejected",
                "change.released",
                "change_request.created",
                "change_request.decided",
                "simulation.result_added"
            ]
        );
        for e in WebhookEvent::ALL {
            assert_eq!(WebhookEvent::parse(e.as_str()), Some(e));
        }
        assert_eq!(WebhookEvent::parse("item.deleted"), None);
        let mut labels: Vec<&str> = WebhookEvent::ALL.iter().map(|e| e.label()).collect();
        assert!(labels.iter().all(|l| l.len() > 3), "{labels:?}");
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), WebhookEvent::ALL.len());
        assert_eq!(
            WebhookEvent::RevisionReleased.label(),
            "A revision was released"
        );
    }
}
