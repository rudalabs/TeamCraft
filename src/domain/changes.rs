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

/// A project group that must be represented among the approvals (a role
/// group such as QA): `members` are the people whose approval counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleRequirement {
    /// Name shown in messages (the group code).
    pub label: String,
    pub members: Vec<uuid::Uuid>,
}

/// What the review policies of all affected project groups ask of a change.
/// The default asks for nothing beyond "every reviewer approves".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewRequirement {
    /// `None` = every reviewer must approve.
    pub min_approvals: Option<usize>,
    /// People who must be reviewers and approve (with their display names).
    pub required_users: Vec<(uuid::Uuid, String)>,
    /// Role groups that need at least one approval from a member.
    pub role_groups: Vec<RoleRequirement>,
}

/// Combines the minimum approvals of several groups: a group without a
/// value wants all reviewers, so the strictest wins.
pub fn combine_min_approvals(mins: &[Option<usize>]) -> Option<usize> {
    if mins.is_empty() || mins.contains(&None) {
        None
    } else {
        mins.iter().copied().flatten().max()
    }
}

/// Outcome of a change under a review policy. A single rejection still
/// rejects; the change is approved when enough reviewers approved, every
/// required person did, and every role group is represented. With the
/// default requirement this equals [`review_outcome`].
pub fn review_outcome_with(
    votes: &[(uuid::Uuid, Decision)],
    req: &ReviewRequirement,
) -> ReviewOutcome {
    if votes.iter().any(|(_, d)| *d == Decision::Rejected) {
        return ReviewOutcome::Rejected;
    }
    if votes.is_empty() {
        return ReviewOutcome::Pending;
    }
    let approved: Vec<uuid::Uuid> = votes
        .iter()
        .filter(|(_, d)| *d == Decision::Approved)
        .map(|(u, _)| *u)
        .collect();
    let needed = req
        .min_approvals
        .map_or(votes.len(), |m| m.min(votes.len()));
    let enough = approved.len() >= needed.max(1);
    let people = req.required_users.iter().all(|(u, _)| approved.contains(u));
    let roles = req
        .role_groups
        .iter()
        .all(|g| g.members.iter().any(|m| approved.contains(m)));
    if enough && people && roles {
        ReviewOutcome::Approved
    } else {
        ReviewOutcome::Pending
    }
}

/// Why a set of reviewers can never satisfy the policy (empty = it can).
/// `submit` refuses a change with any of these.
pub fn policy_shortfalls(reviewers: &[uuid::Uuid], req: &ReviewRequirement) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(min) = req.min_approvals {
        if min > reviewers.len() {
            out.push(format!(
                "{min} approvals are required but only {} reviewer{} assigned",
                reviewers.len(),
                if reviewers.len() == 1 { " is" } else { "s are" }
            ));
        }
    }
    for (user, name) in &req.required_users {
        if !reviewers.contains(user) {
            out.push(format!("{name} must be a reviewer"));
        }
    }
    for group in &req.role_groups {
        if !group.members.iter().any(|m| reviewers.contains(m)) {
            out.push(format!(
                "a reviewer from the group {} is required",
                group.label
            ));
        }
    }
    out
}

/// Why somebody may not review a change order (separation of duties).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewConflict {
    /// Created the change order.
    Author,
    /// Submitted (or, as an administrator, prepared) the change order for the author.
    Submitter,
    /// Changed the data under review: created a revision, uploaded a file or edited it.
    Editor,
}

impl ReviewConflict {
    /// Sentence fragment for error messages, e.g. "edited the revisions under review".
    pub fn describe(self) -> &'static str {
        match self {
            ReviewConflict::Author => "created this change",
            ReviewConflict::Submitter => "prepared or submitted this change",
            ReviewConflict::Editor => "changed the revisions under review",
        }
    }
}

/// Four-eyes check: the reviewer must be neither the author, nor the person
/// who submitted the change (an administrator acting for the author), nor
/// anyone who changed the affected revisions (`editors`).
pub fn conflicting_reviewer(
    reviewer: uuid::Uuid,
    author: uuid::Uuid,
    submitter: Option<uuid::Uuid>,
    editors: &[uuid::Uuid],
) -> Option<ReviewConflict> {
    if reviewer == author {
        Some(ReviewConflict::Author)
    } else if submitter == Some(reviewer) {
        Some(ReviewConflict::Submitter)
    } else if editors.contains(&reviewer) {
        Some(ReviewConflict::Editor)
    } else {
        None
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
