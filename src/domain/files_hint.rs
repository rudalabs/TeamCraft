//! What the *Files* card of a revision tells the user about adding files:
//! pure decision, shared by the page and its tests.

use uuid::Uuid;

use super::LifecycleState;

/// The way to add files that the *Files* card shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilesHint {
    /// Checked out by the user: the upload form.
    Upload,
    /// Free and the user may work on it: *Check out and add files*.
    CheckOut,
    /// Checked out by somebody else: files can be added after check-in.
    LockedByOther,
    /// In review: frozen until the change is decided.
    InReview,
    /// Released or obsolete: frozen, a new revision takes the files.
    Frozen { can_revise: bool },
    /// Nothing to say (viewers, users outside the project group).
    Hidden,
}

impl FilesHint {
    /// Template key.
    pub fn as_str(self) -> &'static str {
        match self {
            FilesHint::Upload => "upload",
            FilesHint::CheckOut => "checkout",
            FilesHint::LockedByOther => "locked",
            FilesHint::InReview => "review",
            FilesHint::Frozen { .. } => "frozen",
            FilesHint::Hidden => "hidden",
        }
    }
}

/// `editable`: the user holds the check-out and may modify (the rule of
/// `ensure_editable`); `can_work`: the user may check out (project group,
/// role, revision in work, not locked by someone else).
pub fn files_hint(
    state: LifecycleState,
    locked_by: Option<Uuid>,
    me: Uuid,
    editable: bool,
    can_work: bool,
    can_revise: bool,
) -> FilesHint {
    if editable {
        return FilesHint::Upload;
    }
    match state {
        LifecycleState::InWork => match locked_by {
            Some(owner) if owner != me => FilesHint::LockedByOther,
            Some(_) => FilesHint::Hidden,
            None if can_work => FilesHint::CheckOut,
            None => FilesHint::Hidden,
        },
        LifecycleState::InReview => FilesHint::InReview,
        LifecycleState::Released | LifecycleState::Obsolete => FilesHint::Frozen { can_revise },
    }
}

/// What to put on an item of this type (the `item_type` text value).
pub fn files_type_hint(item_type: &str) -> Option<&'static str> {
    match item_type {
        "simulation" => Some(
            "Solver input, models, result files and reports. Large result files are stored, not parsed.",
        ),
        "part" => Some("The CAD model, the drawing and a STEP export (for the 3D preview)."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LifecycleState::*;

    #[test]
    fn each_state_has_its_hint() {
        let me = Uuid::new_v4();
        let other = Uuid::new_v4();
        let h = |s, l, e, w, r| files_hint(s, l, me, e, w, r);
        assert_eq!(h(InWork, Some(me), true, true, false), FilesHint::Upload);
        assert_eq!(h(InWork, None, false, true, false), FilesHint::CheckOut);
        assert_eq!(
            h(InWork, Some(other), false, false, false),
            FilesHint::LockedByOther
        );
        assert_eq!(h(InReview, None, false, false, false), FilesHint::InReview);
        assert_eq!(
            h(Released, None, false, false, true),
            FilesHint::Frozen { can_revise: true }
        );
        assert_eq!(
            h(Obsolete, None, false, false, false),
            FilesHint::Frozen { can_revise: false }
        );
    }

    #[test]
    fn viewers_and_outsiders_get_no_add_path() {
        let me = Uuid::new_v4();
        assert_eq!(
            files_hint(InWork, None, me, false, false, false),
            FilesHint::Hidden
        );
        assert_eq!(
            files_hint(InWork, Some(me), me, false, false, false),
            FilesHint::Hidden
        );
    }

    #[test]
    fn type_hints() {
        assert!(files_type_hint("simulation").unwrap().contains("result"));
        assert!(files_type_hint("part").unwrap().contains("STEP"));
        assert!(files_type_hint("document").is_none());
    }

    /// The names `templates/revisions/show.html` switches on (`fh.kind`).
    #[test]
    fn kinds_are_the_names_the_template_uses() {
        let kinds = [
            (FilesHint::Upload, "upload"),
            (FilesHint::CheckOut, "checkout"),
            (FilesHint::LockedByOther, "locked"),
            (FilesHint::InReview, "review"),
            (FilesHint::Frozen { can_revise: true }, "frozen"),
            (FilesHint::Frozen { can_revise: false }, "frozen"),
            (FilesHint::Hidden, "hidden"),
        ];
        let template = include_str!("../../templates/revisions/show.html");
        for (hint, name) in kinds {
            assert_eq!(hint.as_str(), name);
            if hint != FilesHint::Hidden {
                assert!(
                    template.contains(&format!("fh.kind == \"{name}\"")),
                    "the template handles {name}"
                );
            }
        }
    }
}
