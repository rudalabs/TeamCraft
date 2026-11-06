//! Typed ids: one newtype over [`Uuid`] per kind of object, so that a service
//! function that takes the ids of different kinds cannot be called with them
//! swapped – `changes::add_item(pool, actor, revision, change)` does not
//! compile.
//!
//! The newtypes change nothing on the wire or in the database: they are
//! `#[serde(transparent)]` (JSON, query strings, form fields and URL paths
//! carry the plain UUID), `#[sqlx(transparent)]` (bound and read as `uuid`)
//! and display like the UUID. There is deliberately no `From<Uuid>`: an id
//! gets its kind where it is made – `ChangeId(id)` from a URL path, or
//! `change.typed_id()` from a loaded row – so that `.into()` cannot hide a
//! swap.
//!
//! ```
//! use teamcraft::ids::{ChangeId, RevisionId};
//! fn add_item(_change: ChangeId, _revision: RevisionId) {}
//! let (change, revision) = (ChangeId(uuid::Uuid::nil()), RevisionId(uuid::Uuid::nil()));
//! add_item(change, revision);
//! ```
//!
//! ```compile_fail
//! use teamcraft::ids::{ChangeId, RevisionId};
//! fn add_item(_change: ChangeId, _revision: RevisionId) {}
//! let (change, revision) = (ChangeId(uuid::Uuid::nil()), RevisionId(uuid::Uuid::nil()));
//! add_item(revision, change); // swapped: does not compile
//! ```
//!
//! First step (see `docs/OPEN_WORK.md`, *Typed ids*): the services that take
//! ids of different kinds. Everything else still takes [`Uuid`]; the
//! candidates for the next steps are listed in the pull request that added
//! this module.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
            sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// The plain UUID, for the code that does not take typed ids yet.
            pub fn uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Uuid {
                id.0
            }
        }
    };
}

typed_id!(
    /// An item (`items.id`): a part, an assembly, a document …
    ItemId
);
typed_id!(
    /// A revision of an item (`revisions.id`).
    RevisionId
);
typed_id!(
    /// A change order (`changes.id`).
    ChangeId
);
typed_id!(
    /// A project group (`projects.id`).
    ProjectId
);
typed_id!(
    /// A user account (`users.id`).
    UserId
);

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "6f1c7e2a-3b4d-4c5e-8f60-718293a4b5c6";

    #[derive(Serialize, Deserialize)]
    struct Plain {
        change: Uuid,
        revision: Option<Uuid>,
        items: Vec<Uuid>,
    }

    #[derive(Serialize, Deserialize)]
    struct Typed {
        change: ChangeId,
        revision: Option<RevisionId>,
        items: Vec<ItemId>,
    }

    /// The wire format is the UUID's, byte for byte, both ways.
    #[test]
    fn typed_ids_serialize_and_display_like_the_uuid() {
        let raw = Uuid::parse_str(RAW).unwrap();
        let plain = Plain {
            change: raw,
            revision: Some(raw),
            items: vec![raw, Uuid::nil()],
        };
        let typed = Typed {
            change: ChangeId(raw),
            revision: Some(RevisionId(raw)),
            items: vec![ItemId(raw), ItemId(Uuid::nil())],
        };
        let json = serde_json::to_string(&plain).unwrap();
        assert_eq!(serde_json::to_string(&typed).unwrap(), json);
        let back: Typed = serde_json::from_str(&json).unwrap();
        assert_eq!(back.change, ChangeId(raw));
        assert_eq!(back.items, [ItemId(raw), ItemId(Uuid::nil())]);
        assert_eq!(
            serde_json::to_string(&back).unwrap(),
            json,
            "and back to the same bytes"
        );
        assert!(serde_json::from_str::<ChangeId>("\"not-a-uuid\"").is_err());
        assert_eq!(ItemId(raw).to_string(), raw.to_string());
        assert_eq!(format!("{:>40}", RevisionId(raw)), format!("{raw:>40}"));
        assert_eq!(Uuid::from(ChangeId(raw)), raw);
        assert_eq!(ChangeId(raw).uuid(), raw);
    }
}
