//! Release readiness of a change order.
//!
//! A change order releases its revisions atomically, like a release
//! candidate that carries a whole product. Before it goes into review, every
//! revision in it is checked against what it depends on:
//!
//! * **BOM children and item references** (drawing, specification,
//!   reference) must be released already or be part of the same change
//!   order. Otherwise the released assembly could not be reproduced, so this
//!   **blocks** the submit.
//! * **CAD file references** come from a heuristic scan of the files
//!   (`cadrefs`). An unreleased item found that way is only a **warning**.
//! * A dependency whose revisions are all **obsolete** is a **warning**: the
//!   design still uses a part that must not be used any more.
//!
//! Dependencies that are released already were checked when they were
//! released, so one level per revision is enough.

use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::AppResult;

/// Why a dependency is not ready for the release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// No revision is released and none is part of this change order.
    Unreleased,
    /// Every revision of the item is obsolete.
    Obsolete,
}

/// An item a revision of the change order depends on and that is not ready.
#[derive(Clone, Debug, Serialize)]
pub struct Dependency {
    /// The revision of the change order that depends on the item.
    pub revision_id: Uuid,
    pub revision_label: String,
    /// `bom`, `drawing`, `specification`, `reference` or `file`.
    pub relation: String,
    pub item_id: Uuid,
    pub item_number: String,
    pub item_name: String,
    pub latest_revision_id: Uuid,
    pub latest_code: String,
    pub latest_state: String,
    /// Number of the other open change order its latest revision is part of.
    pub other_change: Option<String>,
    pub problem: Problem,
    /// The dependency prevents the submit.
    pub blocking: bool,
    /// Its latest revision can be added to this change order.
    pub addable: bool,
    /// Placeholder for a dependency the reader may not see: it still
    /// blocks, but its identity is removed.
    pub restricted: bool,
    /// The depending revision is hidden from the reader (never sent).
    #[serde(skip)]
    pub revision_restricted: bool,
    /// Project groups of the dependency and of the depending revision (for
    /// the read check; never sent).
    #[serde(skip)]
    pub project_id: Option<Uuid>,
    #[serde(skip)]
    pub revision_project_id: Option<Uuid>,
    /// Who may see the other open change (for the read check; never sent).
    #[serde(skip)]
    pub other_change_access: Option<ChangeAccess>,
}

/// What decides whether a reader sees a change order (see
/// `access::change_visible`), loaded for the other open changes named in
/// the result.
#[derive(Clone, Debug, Default)]
pub struct ChangeAccess {
    pub created_by: Uuid,
    pub reviewers: Vec<Uuid>,
    /// The groups of its items (`None` = items without a group).
    pub projects: Vec<Option<Uuid>>,
}

impl ChangeAccess {
    fn visible_to(&self, actor: &crate::auth::Actor) -> bool {
        actor.is_admin()
            || self.created_by == actor.id
            || self.reviewers.contains(&actor.id)
            || (!self.projects.is_empty()
                && self.projects.iter().all(|p| actor.can_see_project(*p)))
    }
}

/// Text that stands for an open change order the reader may not see.
pub const OTHER_CHANGE: &str = "another open change order";

impl Dependency {
    /// The depending revision of the change order is hidden from the reader.
    fn mask_revision(&mut self) {
        self.revision_id = Uuid::nil();
        self.revision_label = crate::services::access::RESTRICTED_LABEL.into();
        self.revision_restricted = true;
    }

    /// The other open change is named only to readers who may see it.
    fn mask_other_change(&mut self, actor: &crate::auth::Actor) {
        if self.other_change.is_some()
            && !self
                .other_change_access
                .as_ref()
                .is_some_and(|a| a.visible_to(actor))
        {
            self.other_change = Some(OTHER_CHANGE.into());
        }
    }

    fn mask(&mut self) {
        self.item_id = Uuid::nil();
        self.item_number = crate::services::access::RESTRICTED_LABEL.into();
        self.item_name = String::new();
        self.latest_revision_id = Uuid::nil();
        self.latest_code = String::new();
        self.latest_state = String::new();
        self.other_change = None;
        self.addable = false;
        self.restricted = true;
    }
}

/// Moves masked rows behind the visible ones (see [`Readiness::redact`]).
/// The sort is stable, so the visible rows keep their order; rows of a
/// hidden depending revision and placeholders are ordered by what the
/// reader sees only, so their order says nothing about hidden numbers.
fn placeholders_last(rows: &mut [Dependency]) {
    // the place of each visible depending revision in the checked order
    let mut first: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();
    for (i, d) in rows.iter().enumerate() {
        first.entry(d.revision_id).or_insert(i);
    }
    rows.sort_by_cached_key(|d| {
        if d.revision_restricted {
            (
                true,
                0,
                d.restricted,
                d.item_number.clone(),
                d.relation.clone(),
            )
        } else {
            // placeholders by what they show: their relation
            let relation = if d.restricted {
                d.relation.clone()
            } else {
                String::new()
            };
            (
                false,
                first[&d.revision_id],
                d.restricted,
                String::new(),
                relation,
            )
        }
    });
}

/// Result of the readiness check.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Readiness {
    pub blockers: Vec<Dependency>,
    pub warnings: Vec<Dependency>,
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        self.blockers.is_empty()
    }

    /// Removes what the actor may not see: blockers on hidden items become
    /// placeholders that still block; warnings on hidden items (heuristic
    /// file-name matches, obsolete items) are dropped. Hidden depending
    /// revisions lose their label, and another open change the actor may not
    /// see is not named.
    ///
    /// The check sorts by the item numbers of the depending revision and of
    /// the dependency, so a placeholder left in place would bound the hidden
    /// number between its visible neighbours. Rows of hidden depending
    /// revisions therefore go last (ordered by what the reader sees), and
    /// within each depending revision the hidden dependencies follow the
    /// visible ones.
    pub fn redact(mut self, actor: &crate::auth::Actor) -> Self {
        self.warnings
            .retain(|d| actor.can_see_project(d.project_id));
        for d in self.warnings.iter_mut().chain(self.blockers.iter_mut()) {
            if !actor.can_see_project(d.revision_project_id) {
                d.mask_revision();
            }
            if actor.can_see_project(d.project_id) {
                d.mask_other_change(actor);
            } else {
                d.mask();
            }
        }
        placeholders_last(&mut self.blockers);
        placeholders_last(&mut self.warnings);
        self
    }

    /// Latest revisions of the blocking dependencies that can be added to
    /// the change order (each revision once).
    pub fn addable_blockers(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .blockers
            .iter()
            .filter(|d| d.addable)
            .map(|d| d.latest_revision_id)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// One sentence for an error message, e.g. when submitting.
    pub fn blocker_summary(&self) -> String {
        let names: Vec<String> = self
            .blockers
            .iter()
            .take(5)
            .map(|d| {
                if d.restricted {
                    return format!(
                        "a restricted dependency ({} of {}) – ask a member of its group or an admin",
                        relation_label(&d.relation),
                        d.revision_label
                    );
                }
                let place = match &d.other_change {
                    Some(c) => format!("in {c}"),
                    None => d.latest_state.replace('_', " "),
                };
                format!(
                    "{}/{} ({} of {}, {place})",
                    d.item_number,
                    d.latest_code,
                    relation_label(&d.relation),
                    d.revision_label
                )
            })
            .collect();
        let more = self.blockers.len().saturating_sub(names.len());
        let mut text = names.join(", ");
        if more > 0 {
            text.push_str(&format!(" and {more} more"));
        }
        text
    }
}

fn relation_label(relation: &str) -> &'static str {
    match relation {
        "bom" => "BOM line",
        "drawing" => "drawing",
        "specification" => "specification",
        "file" => "CAD file reference",
        _ => "reference",
    }
}

/// Classifies a dependency. `None` = it is ready (a revision is released).
fn classify(
    relation: &str,
    has_released: bool,
    latest_state: &str,
    other_change: Option<&str>,
) -> Option<(Problem, bool, bool)> {
    if has_released {
        return None;
    }
    if latest_state == "obsolete" {
        return Some((Problem::Obsolete, false, false));
    }
    let blocking = relation != "file";
    let addable = latest_state == "in_work" && other_change.is_none();
    Some((Problem::Unreleased, blocking, addable))
}

#[derive(sqlx::FromRow)]
struct Row {
    revision_id: Uuid,
    revision_number: String,
    revision_code: String,
    relation: String,
    item_id: Uuid,
    item_number: String,
    item_name: String,
    latest_revision_id: Uuid,
    latest_code: String,
    latest_state: String,
    has_released: bool,
    other_change: Option<String>,
    other_change_id: Option<Uuid>,
    project_id: Option<Uuid>,
    revision_project_id: Option<Uuid>,
}

/// Author, reviewers and item groups of the given changes.
async fn change_access(
    conn: &mut PgConnection,
    ids: &[Uuid],
) -> AppResult<std::collections::HashMap<Uuid, ChangeAccess>> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let rows = sqlx::query_as::<_, (Uuid, Uuid, Vec<Uuid>, Vec<Option<Uuid>>)>(
        "SELECT c.id, c.created_by,
                ARRAY(SELECT cr.reviewer_id FROM change_reviews cr WHERE cr.change_id = c.id),
                ARRAY(SELECT DISTINCT i.project_id FROM change_items ci
                      JOIN revisions r ON r.id = ci.revision_id JOIN items i ON i.id = r.item_id
                      WHERE ci.change_id = c.id)
         FROM changes c WHERE c.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, created_by, reviewers, projects)| {
            (
                id,
                ChangeAccess {
                    created_by,
                    reviewers,
                    projects,
                },
            )
        })
        .collect())
}

/// Checks the dependencies of all revisions of a change order. One query,
/// usable inside the transaction that submits the change.
pub async fn check(conn: &mut PgConnection, change_id: Uuid) -> AppResult<Readiness> {
    let rows = sqlx::query_as::<_, Row>(
        "WITH cr AS (
             SELECT ci.revision_id, r.item_id
             FROM change_items ci JOIN revisions r ON r.id = ci.revision_id
             WHERE ci.change_id = $1
         ),
         deps AS (
             SELECT b.parent_revision_id AS revision_id, b.child_item_id AS item_id, 'bom'::text AS relation
             FROM bom_lines b JOIN cr ON cr.revision_id = b.parent_revision_id
             UNION
             SELECT ir.from_revision_id, ir.to_item_id, ir.kind
             FROM item_references ir JOIN cr ON cr.revision_id = ir.from_revision_id
             UNION
             SELECT f.revision_id, r2.item_id, 'file'
             FROM files f
             JOIN cr ON cr.revision_id = f.revision_id
             JOIN file_versions fv ON fv.file_id = f.id AND fv.version = f.current_version
             JOIN file_references fr ON fr.file_version_id = fv.id
             JOIN files f2 ON lower(f2.filename) = lower(fr.name)
             JOIN revisions r2 ON r2.id = f2.revision_id
             WHERE r2.item_id <> cr.item_id
               AND r2.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = r2.item_id)
         ),
         -- An item found both in the BOM and by the file scan counts once,
         -- with the stronger relation.
         one AS (
             SELECT DISTINCT ON (revision_id, item_id) revision_id, item_id, relation
             FROM deps ORDER BY revision_id, item_id, (relation = 'file'), relation
         )
         SELECT d.revision_id, pi.number AS revision_number, pr.code AS revision_code, d.relation,
                i.id AS item_id, i.number AS item_number, lr.name AS item_name,
                i.project_id, pi.project_id AS revision_project_id,
                lr.id AS latest_revision_id, lr.code AS latest_code, lr.state AS latest_state,
                EXISTS (SELECT 1 FROM revisions x WHERE x.item_id = i.id AND x.state = 'released')
                    AS has_released,
                (SELECT c.number FROM change_items ci
                   JOIN changes c ON c.id = ci.change_id
                   JOIN revisions x ON x.id = ci.revision_id
                 WHERE x.item_id = i.id AND c.id <> $1
                   AND c.status IN ('draft', 'in_review', 'rejected')
                 ORDER BY c.number LIMIT 1) AS other_change,
                (SELECT c.id FROM change_items ci
                   JOIN changes c ON c.id = ci.change_id
                   JOIN revisions x ON x.id = ci.revision_id
                 WHERE x.item_id = i.id AND c.id <> $1
                   AND c.status IN ('draft', 'in_review', 'rejected')
                 ORDER BY c.number LIMIT 1) AS other_change_id
         FROM one d
         JOIN revisions pr ON pr.id = d.revision_id
         JOIN items pi ON pi.id = pr.item_id
         JOIN items i ON i.id = d.item_id
         CROSS JOIN LATERAL (
             SELECT r.id, r.code, r.state, r.name FROM revisions r
             WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1
         ) lr
         WHERE NOT EXISTS (SELECT 1 FROM cr WHERE cr.item_id = d.item_id)
         ORDER BY pi.number, pr.code, i.number",
    )
    .bind(change_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut others: Vec<Uuid> = rows.iter().filter_map(|r| r.other_change_id).collect();
    others.sort();
    others.dedup();
    let access = change_access(conn, &others).await?;

    let mut result = Readiness::default();
    for row in rows {
        let Some((problem, blocking, addable)) = classify(
            &row.relation,
            row.has_released,
            &row.latest_state,
            row.other_change.as_deref(),
        ) else {
            continue;
        };
        let dep = Dependency {
            revision_id: row.revision_id,
            revision_label: format!("{}/{}", row.revision_number, row.revision_code),
            relation: row.relation,
            item_id: row.item_id,
            item_number: row.item_number,
            item_name: row.item_name,
            latest_revision_id: row.latest_revision_id,
            latest_code: row.latest_code,
            latest_state: row.latest_state,
            other_change: row.other_change,
            problem,
            blocking,
            addable,
            restricted: false,
            revision_restricted: false,
            project_id: row.project_id,
            revision_project_id: row.revision_project_id,
            other_change_access: row.other_change_id.and_then(|id| access.get(&id).cloned()),
        };
        if blocking {
            result.blockers.push(dep);
        } else {
            result.warnings.push(dep);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_dependencies_are_ready() {
        assert_eq!(classify("bom", true, "in_work", None), None);
        assert_eq!(classify("file", true, "obsolete", None), None);
    }

    #[test]
    fn unreleased_bom_and_references_block() {
        for rel in ["bom", "drawing", "specification", "reference"] {
            assert_eq!(
                classify(rel, false, "in_work", None),
                Some((Problem::Unreleased, true, true)),
                "{rel}"
            );
        }
        // In review elsewhere: blocks, but cannot be added here.
        assert_eq!(
            classify("bom", false, "in_review", Some("CHG-000002")),
            Some((Problem::Unreleased, true, false))
        );
        assert_eq!(
            classify("bom", false, "in_work", Some("CHG-000002")),
            Some((Problem::Unreleased, true, false))
        );
    }

    #[test]
    fn file_references_and_obsolete_items_only_warn() {
        assert_eq!(
            classify("file", false, "in_work", None),
            Some((Problem::Unreleased, false, true))
        );
        assert_eq!(
            classify("bom", false, "obsolete", None),
            Some((Problem::Obsolete, false, false))
        );
    }

    fn dep(number: &str, relation: &str, addable: bool, latest: Uuid) -> Dependency {
        Dependency {
            revision_id: Uuid::nil(),
            revision_label: "ASM-000001/A".into(),
            relation: relation.into(),
            item_id: Uuid::nil(),
            item_number: number.into(),
            item_name: String::new(),
            latest_revision_id: latest,
            latest_code: "A".into(),
            latest_state: "in_work".into(),
            other_change: None,
            problem: Problem::Unreleased,
            blocking: true,
            addable,
            restricted: false,
            revision_restricted: false,
            project_id: None,
            revision_project_id: None,
            other_change_access: None,
        }
    }

    #[test]
    fn redact_masks_hidden_revisions_and_other_changes() {
        use crate::{auth::Actor, domain::Role};
        let secret = Uuid::new_v4();
        let reader = Actor::test(Role::Editor, vec![], vec![secret]);
        let mut warning = dep("PRT-000002", "file", false, Uuid::new_v4());
        warning.blocking = false;
        warning.revision_label = "SEC-000001/A".into();
        warning.revision_project_id = Some(secret);
        let mut blocker = dep("PRT-000003", "bom", false, Uuid::new_v4());
        blocker.other_change = Some("CHG-000061".into());
        blocker.other_change_access = Some(ChangeAccess {
            created_by: Uuid::new_v4(),
            reviewers: vec![],
            projects: vec![None, Some(secret)],
        });
        let mut own = dep("PRT-000004", "bom", false, Uuid::new_v4());
        own.other_change = Some("CHG-000062".into());
        own.other_change_access = Some(ChangeAccess {
            created_by: reader.id,
            reviewers: vec![],
            projects: vec![Some(secret)],
        });
        let r = Readiness {
            blockers: vec![blocker, own],
            warnings: vec![warning],
        }
        .redact(&reader);
        assert_eq!(r.warnings[0].revision_label, "restricted item");
        assert_eq!(r.warnings[0].revision_id, Uuid::nil());
        assert_eq!(r.blockers[0].other_change.as_deref(), Some(OTHER_CHANGE));
        assert_eq!(r.blockers[1].other_change.as_deref(), Some("CHG-000062"));
    }

    #[test]
    fn redact_puts_placeholders_last() {
        use crate::{auth::Actor, domain::Role};
        let secret = Uuid::new_v4();
        let reader = Actor::test(Role::Editor, vec![], vec![secret]);
        let asm = Uuid::new_v4();
        let other = Uuid::new_v4();
        // as the check sorts them: by depending item, then dependency number
        let on = |rev: Uuid, label: &str, number: &str, hidden_dep: bool, hidden_rev: bool| {
            let mut d = dep(number, "bom", true, Uuid::new_v4());
            d.revision_id = rev;
            d.revision_label = label.into();
            if hidden_dep {
                d.project_id = Some(secret);
            }
            if hidden_rev {
                d.revision_project_id = Some(secret);
            }
            d
        };
        let hidden_a = Uuid::new_v4();
        let hidden_b = Uuid::new_v4();
        let r = Readiness {
            blockers: vec![
                on(asm, "ASM-000001/A", "PRT-000010", false, false),
                on(asm, "ASM-000001/A", "PRT-000057", true, false),
                on(asm, "ASM-000001/A", "PRT-000099", false, false),
                on(hidden_a, "ASM-000005/A", "PRT-000030", false, true),
                on(hidden_b, "ASM-000006/A", "PRT-000020", false, true),
                on(other, "ASM-000009/A", "PRT-000011", false, false),
            ],
            warnings: vec![],
        }
        .redact(&reader);
        let seen: Vec<(String, String)> = r
            .blockers
            .iter()
            .map(|d| (d.revision_label.clone(), d.item_number.clone()))
            .collect();
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            seen,
            vec![
                pair("ASM-000001/A", "PRT-000010"),
                pair("ASM-000001/A", "PRT-000099"),
                pair("ASM-000001/A", "restricted item"),
                pair("ASM-000009/A", "PRT-000011"),
                // hidden depending revisions: last, by the visible dependency
                pair("restricted item", "PRT-000020"),
                pair("restricted item", "PRT-000030"),
            ]
        );
        assert!(r.blocker_summary().contains("PRT-000099/A"));
    }

    #[test]
    fn summary_and_addable_list() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let r = Readiness {
            blockers: vec![
                dep("PRT-000002", "bom", true, a),
                dep("PRT-000002", "drawing", true, a),
                dep("PRT-000003", "bom", false, b),
            ],
            warnings: vec![],
        };
        assert!(!r.is_ready());
        assert_eq!(r.addable_blockers(), vec![a]);
        assert!(r
            .blocker_summary()
            .starts_with("PRT-000002/A (BOM line of ASM-000001/A, in work)"));
        let many = Readiness {
            blockers: (0..7)
                .map(|i| dep(&format!("P{i}"), "bom", true, a))
                .collect(),
            warnings: vec![],
        };
        assert!(many.blocker_summary().ends_with(" and 2 more"));
    }
}
