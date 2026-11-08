//! Likely duplicate items by name: before (or right after) an item is
//! created, the items whose latest revision has a similar name are offered,
//! so an existing part is reused instead of created twice. Read-only and
//! limited to the items the actor may read.
//!
//! The candidates come from SQL (a name containing one of the words of the
//! new name, at most `CANDIDATES`); the similarity is computed here, so it
//! works without the optional `pg_trgm` extension.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{auth::Actor, domain::ItemType, error::AppResult, services::access};

/// Rows the SQL pre-filter hands over at most.
const CANDIDATES: i64 = 200;
/// Score from which a name counts as similar.
pub const THRESHOLD: f64 = 0.6;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Duplicate {
    pub item_id: Uuid,
    pub number: String,
    pub item_type: String,
    pub project_code: Option<String>,
    pub revision_id: Uuid,
    pub code: String,
    pub state: String,
    pub name: String,
    /// 0..1, 1 = the same name.
    pub score: f64,
}

#[derive(sqlx::FromRow)]
struct Candidate {
    item_id: Uuid,
    number: String,
    item_type: String,
    project_code: Option<String>,
    revision_id: Uuid,
    code: String,
    state: String,
    name: String,
}

/// Lower-case words of a name (letters and digits, at least two characters),
/// each once: `"Bracket, M8 – left"` → `["bracket", "m8", "left"]`.
pub fn words(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in name
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 2)
    {
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// Levenshtein distance over characters.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Similarity of two names, 0..1: the better of the word overlap (Jaccard)
/// and the edit similarity of the names without spaces and punctuation, so
/// both "Bracket left M8" / "M8 bracket left" and "Brakcet" / "Bracket"
/// count as similar.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (wa, wb) = (words(a), words(b));
    // the Jaccard index |A∩B| / |A∪B| (the word lists hold each word once)
    let common = wa.iter().filter(|w| wb.contains(w)).count();
    let union = wa.len() + wb.len() - common;
    let jaccard = if union == 0 {
        0.0
    } else {
        common as f64 / union as f64
    };
    let compact = |s: &str| -> Vec<char> { words(s).concat().chars().collect() };
    let (ca, cb) = (compact(a), compact(b));
    let longest = ca.len().max(cb.len());
    let edit = if longest == 0 {
        0.0
    } else {
        1.0 - edit_distance(&ca, &cb) as f64 / longest as f64
    };
    jaccard.max(edit)
}

/// The items most similar to `name` (best first, at most `limit`), of the
/// same type when one is given; `exclude` leaves out an item (the one just
/// created).
pub async fn likely_duplicates(
    pool: &PgPool,
    actor: &Actor,
    name: &str,
    item_type: Option<ItemType>,
    exclude: Option<Uuid>,
    limit: usize,
) -> AppResult<Vec<Duplicate>> {
    let patterns: Vec<String> = words(name)
        .into_iter()
        .filter(|w| w.chars().count() >= 3)
        .take(8)
        .map(|w| crate::domain::like_pattern(&w))
        .collect();
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT i.id AS item_id, i.number, i.item_type, p.code AS project_code,
                lr.id AS revision_id, lr.code, lr.state, lr.name
         FROM items i
         LEFT JOIN projects p ON p.id = i.project_id
         CROSS JOIN LATERAL (
             SELECT r.id, r.code, r.state, r.name FROM revisions r
             WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1
         ) lr
         WHERE i.id IN (SELECT r.item_id FROM revisions r WHERE r.name ILIKE ANY($1))
           AND lr.name ILIKE ANY($1)
           AND ($2::text IS NULL OR i.item_type = $2)
           AND ($3::uuid IS NULL OR i.id <> $3)
           AND {}
         ORDER BY i.number
         LIMIT {CANDIDATES}",
        access::item_visible("i", 4)
    );
    let rows = sqlx::query_as::<_, Candidate>(&sql)
        .bind(&patterns)
        .bind(item_type.map(|t| t.as_str()))
        .bind(exclude)
        .bind(&actor.hidden_projects)
        .fetch_all(pool)
        .await?;
    let mut found: Vec<Duplicate> = rows
        .into_iter()
        .filter_map(|c| {
            let score = similarity(name, &c.name);
            (score >= THRESHOLD).then(|| Duplicate {
                item_id: c.item_id,
                number: c.number,
                item_type: c.item_type,
                project_code: c.project_code,
                revision_id: c.revision_id,
                code: c.code,
                state: c.state,
                name: c.name,
                score: (score * 100.0).round() / 100.0,
            })
        })
        .collect();
    found.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.number.cmp(&b.number))
    });
    found.truncate(limit);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_lower_case_and_unique() {
        assert_eq!(
            words("Bracket, M8 – left bracket"),
            ["bracket", "m8", "left"]
        );
        assert!(words("a - b").is_empty());
        assert_eq!(words("Lagerbock Ø20"), ["lagerbock", "ø20"]);
    }

    #[test]
    fn similar_names() {
        assert_eq!(similarity("Bracket left", "bracket LEFT"), 1.0);
        assert_eq!(similarity("M8 bracket left", "Bracket left M8"), 1.0);
        assert!(similarity("Bracket", "Brakcet") >= THRESHOLD, "typo");
        assert!(similarity("Motor mount plate", "Motor mount plate v2") >= THRESHOLD);
        assert!(similarity("Bracket", "Gearbox housing") < THRESHOLD);
        assert!(similarity("Shaft", "Shaft collar clamp large") < THRESHOLD);
        assert_eq!(similarity("", ""), 0.0);
        assert_eq!(edit_distance(&['a', 'b'], &[]), 2);
    }
}
