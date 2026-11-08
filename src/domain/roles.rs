//! Roles and project group visibility.

use serde::{Deserialize, Serialize};

/// Global user role. Ordering matters: `Viewer < Editor < Admin`.
///
/// What a CAD editor may change is further limited to the project groups the
/// user is a member of (see `Actor::can_edit_project`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Read-only access to internal project groups and to restricted groups
    /// the user is a member of.
    Viewer,
    /// "CAD editor": edits the groups they are a member of (create and modify
    /// product data, author changes, review changes); reads like a viewer.
    Editor,
    /// Everything, including restricted groups, plus user/project
    /// administration and lock overrides.
    Admin,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Viewer, Role::Editor, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Admin => "admin",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "Viewer",
            Role::Editor => "CAD editor",
            Role::Admin => "Admin",
        }
    }

    /// Accepts the stored values plus a few spellings (`cad-editor`, and the
    /// former name `engineer`).
    pub fn parse(s: &str) -> Option<Role> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '_'], "-")
            .as_str()
        {
            "viewer" => Some(Role::Viewer),
            "editor" | "cad-editor" | "engineer" => Some(Role::Editor),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// How administrators who are not members read a restricted group (the
/// need-to-know policy, `project_policies.admin_read`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminRead {
    /// Administrators read every restricted group (the default).
    Implicit,
    /// The group is hidden from an administrator until they open a
    /// time-limited, audited elevation with a written reason.
    WithReason,
    /// Administrators manage the group but read nothing in it unless they
    /// are members.
    None,
}

impl AdminRead {
    pub fn as_str(self) -> &'static str {
        match self {
            AdminRead::Implicit => "implicit",
            AdminRead::WithReason => "with_reason",
            AdminRead::None => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AdminRead::Implicit => "Administrators read the group",
            AdminRead::WithReason => "Administrators need a reason (time-limited, audited)",
            AdminRead::None => "Administrators manage but cannot read",
        }
    }

    pub fn parse(s: &str) -> Option<AdminRead> {
        match s.trim().to_ascii_lowercase().as_str() {
            "implicit" => Some(AdminRead::Implicit),
            "with_reason" => Some(AdminRead::WithReason),
            "none" => Some(AdminRead::None),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Project group visibility
// ---------------------------------------------------------------------------

/// Who may read the data of a project group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectVisibility {
    /// Every signed-in user reads; members edit (the default).
    Internal,
    /// Only members (of any role) and administrators read anything in it;
    /// everybody else gets "not found".
    Restricted,
}

impl ProjectVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectVisibility::Internal => "internal",
            ProjectVisibility::Restricted => "restricted",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProjectVisibility::Internal => "Internal",
            ProjectVisibility::Restricted => "Restricted",
        }
    }

    pub fn parse(s: &str) -> Option<ProjectVisibility> {
        match s.trim().to_ascii_lowercase().as_str() {
            "internal" => Some(ProjectVisibility::Internal),
            "restricted" => Some(ProjectVisibility::Restricted),
            _ => None,
        }
    }
}

/// Found by cargo mutants: the stored values, labels and aliases of roles,
/// admin read policies and visibilities were not pinned.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles() {
        let roles: Vec<(&str, &str)> = Role::ALL.iter().map(|r| (r.as_str(), r.label())).collect();
        assert_eq!(
            roles,
            [
                ("viewer", "Viewer"),
                ("editor", "CAD editor"),
                ("admin", "Admin")
            ]
        );
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        for alias in ["CAD editor", "cad_editor", "Engineer", " EDITOR "] {
            assert_eq!(Role::parse(alias), Some(Role::Editor), "{alias}");
        }
        assert_eq!(Role::parse("owner"), None);
    }

    #[test]
    fn admin_read_policies() {
        let all = [AdminRead::Implicit, AdminRead::WithReason, AdminRead::None];
        let stored: Vec<&str> = all.iter().map(|a| a.as_str()).collect();
        assert_eq!(stored, ["implicit", "with_reason", "none"]);
        for a in all {
            assert_eq!(AdminRead::parse(a.as_str()), Some(a));
        }
        assert_eq!(AdminRead::parse(" NONE "), Some(AdminRead::None));
        assert_eq!(AdminRead::parse("never"), None);
        assert_eq!(AdminRead::Implicit.label(), "Administrators read the group");
        assert_eq!(
            AdminRead::WithReason.label(),
            "Administrators need a reason (time-limited, audited)"
        );
        assert_eq!(
            AdminRead::None.label(),
            "Administrators manage but cannot read"
        );
    }

    #[test]
    fn visibilities() {
        let all = [ProjectVisibility::Internal, ProjectVisibility::Restricted];
        let v: Vec<(&str, &str)> = all.iter().map(|v| (v.as_str(), v.label())).collect();
        assert_eq!(v, [("internal", "Internal"), ("restricted", "Restricted")]);
        for v in all {
            assert_eq!(ProjectVisibility::parse(v.as_str()), Some(v));
        }
        assert_eq!(ProjectVisibility::parse("secret"), None);
    }
}
