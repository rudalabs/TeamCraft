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
    /// Name shown in messages, e.g. "the group QA".
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

/// The part of the requirement the assigned `reviewers` can still meet.
/// Reviewers are only changed in Draft, so a policy or a group that changed
/// after submit must not make an in-review change undecidable.
pub fn attainable(req: &ReviewRequirement, reviewers: &[uuid::Uuid]) -> ReviewRequirement {
    ReviewRequirement {
        min_approvals: req.min_approvals,
        required_users: req
            .required_users
            .iter()
            .filter(|(u, _)| reviewers.contains(u))
            .cloned()
            .collect(),
        role_groups: req
            .role_groups
            .iter()
            .filter(|g| g.members.iter().any(|m| reviewers.contains(m)))
            .cloned()
            .collect(),
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
            out.push(format!("a reviewer from {} is required", group.label));
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

/// What a line of a change order does when the change is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineAction {
    /// Releases an in-work revision (the classic line).
    Release,
    /// Marks a released revision obsolete.
    Obsolete,
    /// Marks a released revision obsolete and declares its item superseded by another item.
    Supersede,
}

impl LineAction {
    pub const ALL: [LineAction; 3] = [
        LineAction::Release,
        LineAction::Obsolete,
        LineAction::Supersede,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LineAction::Release => "release",
            LineAction::Obsolete => "obsolete",
            LineAction::Supersede => "supersede",
        }
    }

    pub fn parse(raw: &str) -> Option<LineAction> {
        let raw = raw.trim().to_ascii_lowercase();
        LineAction::ALL.into_iter().find(|a| a.as_str() == raw)
    }

    /// The lifecycle state the revision of the line must be in when the line is added.
    pub fn required_state(self) -> crate::domain::LifecycleState {
        match self {
            LineAction::Release => crate::domain::LifecycleState::InWork,
            LineAction::Obsolete | LineAction::Supersede => crate::domain::LifecycleState::Released,
        }
    }
}

#[cfg(test)]
mod line_action_tests {
    use super::*;
    use crate::domain::LifecycleState;

    #[test]
    fn parses_and_round_trips() {
        for a in LineAction::ALL {
            assert_eq!(LineAction::parse(a.as_str()), Some(a));
        }
        assert_eq!(
            LineAction::parse(" Supersede "),
            Some(LineAction::Supersede)
        );
        assert_eq!(LineAction::parse("delete"), None);
        assert_eq!(LineAction::parse(""), None);
    }

    #[test]
    fn actions_need_a_released_revision_and_release_an_in_work_one() {
        assert_eq!(LineAction::Release.required_state(), LifecycleState::InWork);
        assert_eq!(
            LineAction::Obsolete.required_state(),
            LifecycleState::Released
        );
        assert_eq!(
            LineAction::Supersede.required_state(),
            LifecycleState::Released
        );
        // an action line is valid only because the lifecycle allows the transition
        assert!(LineAction::Obsolete
            .required_state()
            .can_transition_to(LifecycleState::Obsolete));
    }
}

/// The stored values and labels (found by cargo mutants: the round-trip
/// tests above do not pin the strings that are in the database and the API).
#[cfg(test)]
mod text_tests {
    use super::*;

    #[test]
    fn stored_values_and_labels() {
        let stored: Vec<&str> = ChangeStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            stored,
            ["draft", "in_review", "rejected", "released", "cancelled"]
        );
        let labels: Vec<&str> = ChangeStatus::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            ["Draft", "In Review", "Rejected", "Released", "Cancelled"]
        );

        let stored: Vec<&str> = Priority::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(stored, ["low", "normal", "high", "critical"]);

        let decisions = [Decision::Pending, Decision::Approved, Decision::Rejected];
        let stored: Vec<&str> = decisions.iter().map(|d| d.as_str()).collect();
        assert_eq!(stored, ["pending", "approved", "rejected"]);

        let stored: Vec<&str> = RequestStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            stored,
            [
                "new",
                "in_triage",
                "accepted",
                "rejected",
                "deferred",
                "implemented",
                "withdrawn"
            ]
        );
        let labels: Vec<&str> = RequestStatus::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            [
                "New",
                "In Triage",
                "Accepted",
                "Rejected",
                "Deferred",
                "Implemented",
                "Withdrawn"
            ]
        );

        let stored: Vec<&str> = RequestReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            stored,
            [
                "defect",
                "improvement",
                "cost",
                "manufacturing",
                "supplier",
                "safety",
                "customer",
                "other"
            ]
        );
        let labels: Vec<&str> = RequestReason::ALL.iter().map(|r| r.label()).collect();
        assert_eq!(
            labels,
            [
                "Defect / error",
                "Improvement",
                "Cost reduction",
                "Manufacturing / assembly",
                "Supplier / purchasing",
                "Safety / compliance",
                "Customer request",
                "Other"
            ]
        );

        let stored: Vec<&str> = LineAction::ALL.iter().map(|a| a.as_str()).collect();
        assert_eq!(stored, ["release", "obsolete", "supersede"]);

        assert_eq!(
            [
                RequestDecision::Accept.status(),
                RequestDecision::Reject.status(),
                RequestDecision::Defer.status()
            ],
            [
                RequestStatus::Accepted,
                RequestStatus::Rejected,
                RequestStatus::Deferred
            ]
        );
        assert_eq!(ReviewConflict::Author.describe(), "created this change");
        assert_eq!(
            ReviewConflict::Submitter.describe(),
            "prepared or submitted this change"
        );
        assert_eq!(
            ReviewConflict::Editor.describe(),
            "changed the revisions under review"
        );
    }

    /// Which changes still hold their revisions, and which may be edited;
    /// which change requests are still open (found by cargo mutants).
    #[test]
    fn open_and_modifiable() {
        let open: Vec<bool> = ChangeStatus::ALL.iter().map(|s| s.is_open()).collect();
        // draft, in_review, rejected, released, cancelled
        assert_eq!(open, [true, true, true, false, false]);
        let modifiable: Vec<bool> = ChangeStatus::ALL
            .iter()
            .map(|s| s.is_modifiable())
            .collect();
        assert_eq!(modifiable, [true, false, false, false, false]);
        let open: Vec<bool> = RequestStatus::ALL.iter().map(|s| s.is_open()).collect();
        // new, in_triage, accepted, rejected, deferred, implemented, withdrawn
        assert_eq!(open, [true, true, true, false, true, false, false]);
    }

    /// Every stored value reads back, in any case and with white space;
    /// unknown values are refused (found by cargo mutants).
    #[test]
    fn parse_round_trips_and_aliases() {
        for s in ChangeStatus::ALL {
            assert_eq!(ChangeStatus::parse(s.as_str()), Some(s));
            assert_eq!(
                ChangeStatus::parse(&format!(" {} ", s.as_str().to_uppercase())),
                Some(s)
            );
        }
        assert_eq!(ChangeStatus::parse("open"), None);
        for p in Priority::ALL {
            assert_eq!(Priority::parse(p.as_str()), Some(p));
        }
        assert_eq!(Priority::parse(""), Some(Priority::Normal));
        assert_eq!(Priority::parse("urgent"), None);
        for d in [Decision::Pending, Decision::Approved, Decision::Rejected] {
            assert_eq!(Decision::parse(d.as_str()), Some(d));
        }
        assert_eq!(Decision::parse("Approve"), Some(Decision::Approved));
        assert_eq!(Decision::parse("reject"), Some(Decision::Rejected));
        assert_eq!(Decision::parse("maybe"), None);
        for s in RequestStatus::ALL {
            assert_eq!(RequestStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(
            RequestStatus::parse(" In_Triage "),
            Some(RequestStatus::InTriage)
        );
        assert_eq!(RequestStatus::parse("done"), None);
        for r in RequestReason::ALL {
            assert_eq!(RequestReason::parse(r.as_str()), Some(r));
        }
        assert_eq!(RequestReason::parse(""), Some(RequestReason::Other));
        assert_eq!(RequestReason::parse("boredom"), None);
        for (raw, decision) in [
            ("accept", RequestDecision::Accept),
            ("Accepted", RequestDecision::Accept),
            ("reject", RequestDecision::Reject),
            ("rejected", RequestDecision::Reject),
            ("defer", RequestDecision::Defer),
            (" Deferred ", RequestDecision::Defer),
        ] {
            assert_eq!(
                RequestDecision::parse(raw).map(RequestDecision::status),
                Some(decision.status()),
                "{raw}"
            );
        }
        assert!(RequestDecision::parse("later").is_none());
    }
}
