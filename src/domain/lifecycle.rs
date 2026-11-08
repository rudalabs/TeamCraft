//! Revision lifecycle and revision letters.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// Being worked on; editable.
    InWork,
    /// Part of a change order that is under review; frozen.
    InReview,
    /// Approved and released; immutable.
    Released,
    /// Superseded / must not be used for new designs; immutable.
    Obsolete,
}

impl LifecycleState {
    pub const ALL: [LifecycleState; 4] = [
        LifecycleState::InWork,
        LifecycleState::InReview,
        LifecycleState::Released,
        LifecycleState::Obsolete,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleState::InWork => "in_work",
            LifecycleState::InReview => "in_review",
            LifecycleState::Released => "released",
            LifecycleState::Obsolete => "obsolete",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LifecycleState::InWork => "WIP",
            LifecycleState::InReview => "In Review",
            LifecycleState::Released => "Released",
            LifecycleState::Obsolete => "Obsolete",
        }
    }

    pub fn parse(s: &str) -> Option<LifecycleState> {
        match s.trim().to_ascii_lowercase().as_str() {
            "in_work" => Some(LifecycleState::InWork),
            "in_review" => Some(LifecycleState::InReview),
            "released" => Some(LifecycleState::Released),
            "obsolete" => Some(LifecycleState::Obsolete),
            _ => None,
        }
    }

    /// Whether attributes, files and BOM of a revision in this state may change.
    pub fn is_editable(self) -> bool {
        matches!(self, LifecycleState::InWork)
    }

    /// Whether the revision still counts as "open" (blocks creating a new revision).
    pub fn is_open(self) -> bool {
        matches!(self, LifecycleState::InWork | LifecycleState::InReview)
    }

    /// Allowed lifecycle transitions. Everything else is rejected.
    pub fn can_transition_to(self, to: LifecycleState) -> bool {
        use LifecycleState::*;
        matches!(
            (self, to),
            (InWork, InReview) | (InReview, Released) | (InReview, InWork) | (Released, Obsolete)
        )
    }
}

/// One allowed lifecycle transition: how it happens and who may trigger it.
/// The services enforce these rules; this table documents them in the UI and
/// is checked against [`LifecycleState::can_transition_to`] by a unit test.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TransitionRule {
    pub from: LifecycleState,
    pub to: LifecycleState,
    /// How the transition happens.
    pub how: &'static str,
    /// Who may trigger it.
    pub who: &'static str,
}

pub const LIFECYCLE_RULES: [TransitionRule; 4] = [
    TransitionRule {
        from: LifecycleState::InWork,
        to: LifecycleState::InReview,
        how: "Submit a change order that contains the revision (it must not be checked out by somebody else).",
        who: "The author of the change – a CAD editor of the item's project group – or an admin.",
    },
    TransitionRule {
        from: LifecycleState::InReview,
        to: LifecycleState::Released,
        how: "Automatically when every reviewer of the change has approved; all its revisions are released together.",
        who: "The assigned reviewers: CAD editors or admins, never the author, the submitter or anyone who changed the revisions (four-eyes principle).",
    },
    TransitionRule {
        from: LifecycleState::InReview,
        to: LifecycleState::InWork,
        how: "A reviewer rejects the change (with a reason) or its author withdraws it.",
        who: "Any assigned reviewer; the author of the change or an admin (for a deactivated author also a manager of every affected project group).",
    },
    TransitionRule {
        from: LifecycleState::Released,
        to: LifecycleState::Obsolete,
        how: "Mark obsolete – final, cannot be undone.",
        who: "CAD editors of the item's project group, admins.",
    },
];

/// The rules that lead away from `state`.
pub fn rules_from(state: LifecycleState) -> Vec<TransitionRule> {
    LIFECYCLE_RULES
        .iter()
        .filter(|r| r.from == state)
        .copied()
        .collect()
}

/// Revision letters per ASME Y14.35: the letters I, O, Q, S, X and Z are not
/// used because they are easily confused with digits.
pub(super) const REVISION_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPRTUVWY";

/// Converts a 1-based revision sequence number into its revision code:
/// 1 → `A`, 2 → `B`, …, 20 → `Y`, 21 → `AA`, 22 → `AB`, …
pub fn revision_code(seq: i32) -> String {
    assert!(seq >= 1, "revision sequence numbers start at 1");
    let base = REVISION_ALPHABET.len() as u64;
    let mut n = seq as u64;
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(REVISION_ALPHABET[(n % base) as usize]);
        n /= base;
    }
    out.reverse();
    String::from_utf8(out).expect("alphabet is ASCII")
}

/// Inverse of [`revision_code`]. Returns `None` for invalid codes.
pub fn revision_seq(code: &str) -> Option<i32> {
    if code.is_empty() || code.len() > 6 {
        return None;
    }
    let base = REVISION_ALPHABET.len() as i64;
    let mut n: i64 = 0;
    for b in code.trim().to_ascii_uppercase().bytes() {
        let digit = REVISION_ALPHABET.iter().position(|&a| a == b)? as i64;
        n = n * base + digit + 1;
    }
    i32::try_from(n).ok().filter(|n| *n >= 1)
}

/// What happens to the old versions of a file when a newer one is uploaded
/// (`files.retention`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRetention {
    /// Every version is kept (the default).
    All,
    /// Only the newest version is kept: older versions are removed by the
    /// housekeeping after a grace period, as long as the revision is in work.
    Latest,
}

impl FileRetention {
    pub fn as_str(self) -> &'static str {
        match self {
            FileRetention::All => "all",
            FileRetention::Latest => "latest",
        }
    }

    pub fn parse(s: &str) -> Option<FileRetention> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" => Some(FileRetention::All),
            "latest" => Some(FileRetention::Latest),
            _ => None,
        }
    }
}
