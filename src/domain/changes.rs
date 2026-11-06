//! Change orders (ECO) and change requests (ECR).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Draft,
    InReview,
    Rejected,
    Released,
    Cancelled,
}

impl ChangeStatus {
    pub const ALL: [ChangeStatus; 5] = [
        ChangeStatus::Draft,
        ChangeStatus::InReview,
        ChangeStatus::Rejected,
        ChangeStatus::Released,
        ChangeStatus::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChangeStatus::Draft => "draft",
            ChangeStatus::InReview => "in_review",
            ChangeStatus::Rejected => "rejected",
            ChangeStatus::Released => "released",
            ChangeStatus::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChangeStatus::Draft => "Draft",
            ChangeStatus::InReview => "In Review",
            ChangeStatus::Rejected => "Rejected",
            ChangeStatus::Released => "Released",
            ChangeStatus::Cancelled => "Cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<ChangeStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "draft" => Some(ChangeStatus::Draft),
            "in_review" => Some(ChangeStatus::InReview),
            "rejected" => Some(ChangeStatus::Rejected),
            "released" => Some(ChangeStatus::Released),
            "cancelled" => Some(ChangeStatus::Cancelled),
            _ => None,
        }
    }

    /// Open changes still hold on to their affected revisions.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            ChangeStatus::Draft | ChangeStatus::InReview | ChangeStatus::Rejected
        )
    }

    /// Whether items, reviewers and texts of the change may be modified.
    pub fn is_modifiable(self) -> bool {
        matches!(self, ChangeStatus::Draft)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Critical,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<Priority> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Priority::Low),
            "normal" | "" => Some(Priority::Normal),
            "high" => Some(Priority::High),
            "critical" => Some(Priority::Critical),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Pending,
    Approved,
    Rejected,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Pending => "pending",
            Decision::Approved => "approved",
            Decision::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Decision> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pending" => Some(Decision::Pending),
            "approved" | "approve" => Some(Decision::Approved),
            "rejected" | "reject" => Some(Decision::Rejected),
            _ => None,
        }
    }
}

/// Outcome of evaluating all review decisions of a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// At least one reviewer has not decided yet and nobody rejected.
    Pending,
    /// Every reviewer approved.
    Approved,
    /// At least one reviewer rejected.
    Rejected,
}

/// A single rejection rejects the change; all reviewers must approve.
pub fn review_outcome(decisions: &[Decision]) -> ReviewOutcome {
    if decisions.contains(&Decision::Rejected) {
        ReviewOutcome::Rejected
    } else if !decisions.is_empty() && decisions.iter().all(|d| *d == Decision::Approved) {
        ReviewOutcome::Approved
    } else {
        ReviewOutcome::Pending
    }
}

// ---------------------------------------------------------------------------
// Change requests (ECR) – the step before a change order (ECO)
// ---------------------------------------------------------------------------

/// Formats an automatically generated change request number, e.g. `ECR-000007`.
pub fn format_request_number(value: i64) -> String {
    format!("ECR-{value:06}")
}

/// Status of an engineering change request.
///
/// ```text
///   new ──▶ in_triage ──▶ accepted ──▶ implemented (linked change order released)
///    │          │      ──▶ rejected
///    │          │      ──▶ deferred ──▶ in_triage (reopened) / accepted / rejected
///    └──────────┴──▶ withdrawn (by the requester)
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    New,
    InTriage,
    Accepted,
    Rejected,
    Deferred,
    Implemented,
    Withdrawn,
}

impl RequestStatus {
    pub const ALL: [RequestStatus; 7] = [
        RequestStatus::New,
        RequestStatus::InTriage,
        RequestStatus::Accepted,
        RequestStatus::Rejected,
        RequestStatus::Deferred,
        RequestStatus::Implemented,
        RequestStatus::Withdrawn,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RequestStatus::New => "new",
            RequestStatus::InTriage => "in_triage",
            RequestStatus::Accepted => "accepted",
            RequestStatus::Rejected => "rejected",
            RequestStatus::Deferred => "deferred",
            RequestStatus::Implemented => "implemented",
            RequestStatus::Withdrawn => "withdrawn",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RequestStatus::New => "New",
            RequestStatus::InTriage => "In Triage",
            RequestStatus::Accepted => "Accepted",
            RequestStatus::Rejected => "Rejected",
            RequestStatus::Deferred => "Deferred",
            RequestStatus::Implemented => "Implemented",
            RequestStatus::Withdrawn => "Withdrawn",
        }
    }

    pub fn parse(s: &str) -> Option<RequestStatus> {
        let s = s.trim().to_ascii_lowercase();
        RequestStatus::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Not finished yet: still waiting for a decision or for its change order.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            RequestStatus::New
                | RequestStatus::InTriage
                | RequestStatus::Accepted
                | RequestStatus::Deferred
        )
    }

    /// Allowed status transitions. Everything else is rejected.
    pub fn can_transition_to(self, to: RequestStatus) -> bool {
        use RequestStatus::*;
        matches!(
            (self, to),
            (New, InTriage)
                | (New | InTriage | Deferred, Accepted | Rejected)
                | (New | InTriage, Deferred)
                | (Deferred, InTriage)
                | (New | InTriage, Withdrawn)
                | (Accepted, Implemented)
        )
    }
}

/// Why a change is requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestReason {
    Defect,
    Improvement,
    Cost,
    Manufacturing,
    Supplier,
    Safety,
    Customer,
    Other,
}

impl RequestReason {
    pub const ALL: [RequestReason; 8] = [
        RequestReason::Defect,
        RequestReason::Improvement,
        RequestReason::Cost,
        RequestReason::Manufacturing,
        RequestReason::Supplier,
        RequestReason::Safety,
        RequestReason::Customer,
        RequestReason::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RequestReason::Defect => "defect",
            RequestReason::Improvement => "improvement",
            RequestReason::Cost => "cost",
            RequestReason::Manufacturing => "manufacturing",
            RequestReason::Supplier => "supplier",
            RequestReason::Safety => "safety",
            RequestReason::Customer => "customer",
            RequestReason::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RequestReason::Defect => "Defect / error",
            RequestReason::Improvement => "Improvement",
            RequestReason::Cost => "Cost reduction",
            RequestReason::Manufacturing => "Manufacturing / assembly",
            RequestReason::Supplier => "Supplier / purchasing",
            RequestReason::Safety => "Safety / compliance",
            RequestReason::Customer => "Customer request",
            RequestReason::Other => "Other",
        }
    }

    pub fn parse(s: &str) -> Option<RequestReason> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return Some(RequestReason::Other);
        }
        RequestReason::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// The decision of the triage on a change request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestDecision {
    Accept,
    Reject,
    Defer,
}

impl RequestDecision {
    pub fn parse(s: &str) -> Option<RequestDecision> {
        match s.trim().to_ascii_lowercase().as_str() {
            "accept" | "accepted" => Some(RequestDecision::Accept),
            "reject" | "rejected" => Some(RequestDecision::Reject),
            "defer" | "deferred" => Some(RequestDecision::Defer),
            _ => None,
        }
    }

    pub fn status(self) -> RequestStatus {
        match self {
            RequestDecision::Accept => RequestStatus::Accepted,
            RequestDecision::Reject => RequestStatus::Rejected,
            RequestDecision::Defer => RequestStatus::Deferred,
        }
    }
}
