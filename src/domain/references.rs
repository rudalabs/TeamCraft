//! BOM revision rules, references and change propagation.

use serde::{Deserialize, Serialize};

/// Decides which revision of a child item is shown in a BOM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RevisionRule {
    /// The newest revision regardless of its state.
    #[default]
    Latest,
    /// The newest released revision; lines without one are flagged.
    Released,
    /// Released assemblies show exactly the child revisions they were
    /// released with; lines of unreleased assemblies use the newest
    /// released revision.
    AsReleased,
}

impl RevisionRule {
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionRule::Latest => "latest",
            RevisionRule::Released => "released",
            RevisionRule::AsReleased => "as_released",
        }
    }

    pub fn parse(s: &str) -> Option<RevisionRule> {
        match s.trim().to_ascii_lowercase().as_str() {
            "latest" | "" => Some(RevisionRule::Latest),
            "released" => Some(RevisionRule::Released),
            "as_released" | "as-released" => Some(RevisionRule::AsReleased),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// References and change propagation
// ---------------------------------------------------------------------------

/// How a revision references another item besides the BOM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    /// The referencing item is a drawing of the target.
    Drawing,
    /// The referencing item is a specification that applies to the target.
    Specification,
    /// The referencing model uses geometry of the target (skeleton,
    /// interface, mating part).
    Reference,
}

impl ReferenceKind {
    pub const ALL: [ReferenceKind; 3] = [
        ReferenceKind::Drawing,
        ReferenceKind::Specification,
        ReferenceKind::Reference,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReferenceKind::Drawing => "drawing",
            ReferenceKind::Specification => "specification",
            ReferenceKind::Reference => "reference",
        }
    }

    /// "`<this item>` … `<target>`", e.g. "is a drawing of".
    pub fn label(self) -> &'static str {
        match self {
            ReferenceKind::Drawing => "Drawing of",
            ReferenceKind::Specification => "Specification for",
            ReferenceKind::Reference => "References",
        }
    }

    pub fn parse(s: &str) -> Option<ReferenceKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "drawing" => Some(ReferenceKind::Drawing),
            "specification" => Some(ReferenceKind::Specification),
            "reference" => Some(ReferenceKind::Reference),
            _ => None,
        }
    }
}
