//! Custom attributes (feature 5): definitions maintained by administrators
//! and the validation of values against them.
//!
//! Values live in `revisions.attributes` (JSON object by key), so they are
//! versioned with the revision like name, material and mass: frozen once
//! released, copied by *revise* and *Save as*. The key and the data type of a
//! definition never change after it was created; label, unit, choices, item
//! types, order and the active flag do. Deactivating keeps stored values
//! visible, it only stops offering the attribute.

use serde_json::{json, Map, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, AttributeType, ItemType, Role},
    error::{AppError, AppResult},
    models::AttributeDef,
    services::audit,
};

const COLUMNS: &str =
    "id, key, label, data_type, unit, choices, item_types, position, active, created_at";

/// All definitions in display order; `active_only` leaves out deactivated ones.
pub async fn list(pool: &PgPool, active_only: bool) -> AppResult<Vec<AttributeDef>> {
    let mut conn = pool.acquire().await?;
    list_conn(&mut conn, active_only).await
}

pub(crate) async fn list_conn(
    conn: &mut PgConnection,
    active_only: bool,
) -> AppResult<Vec<AttributeDef>> {
    Ok(sqlx::query_as::<_, AttributeDef>(&format!(
        "SELECT {COLUMNS} FROM attribute_defs WHERE active OR NOT $1 ORDER BY position, label"
    ))
    .bind(active_only)
    .fetch_all(&mut *conn)
    .await?)
}

/// Whether the definition applies to items of `item_type`.
pub fn applies_to(def: &AttributeDef, item_type: &str) -> bool {
    def.item_types.is_empty() || def.item_types.iter().any(|t| t == item_type)
}

/// Active definitions for items of `item_type` – the fields of the forms.
pub async fn for_item_type(pool: &PgPool, item_type: &str) -> AppResult<Vec<AttributeDef>> {
    Ok(list(pool, true)
        .await?
        .into_iter()
        .filter(|d| applies_to(d, item_type))
        .collect())
}

pub async fn get_by_key(pool: &PgPool, key: &str) -> AppResult<AttributeDef> {
    let key = domain::normalize_attribute_key(key)?;
    sqlx::query_as::<_, AttributeDef>(&format!(
        "SELECT {COLUMNS} FROM attribute_defs WHERE key = $1"
    ))
    .bind(&key)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found(format!("Attribute '{key}' not found.")))
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<AttributeDef> {
    sqlx::query_as::<_, AttributeDef>(&format!(
        "SELECT {COLUMNS} FROM attribute_defs WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("Attribute not found."))
}

/// How many revisions store a value of the attribute.
pub async fn usage(pool: &PgPool, key: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM revisions WHERE attributes ? $1")
            .bind(key)
            .fetch_one(pool)
            .await?,
    )
}

fn data_type_of(def: &AttributeDef) -> AttributeType {
    AttributeType::parse(&def.data_type).unwrap_or(AttributeType::Text)
}

/// The editable part of a definition.
#[derive(Clone, Debug, Default)]
pub struct AttributeSettings {
    pub label: String,
    pub unit: String,
    /// Choices of a `choice` attribute (free text: one per line or `;`).
    pub choices: String,
    /// Item types (`part`, `assembly`, …); empty = all.
    pub item_types: Vec<String>,
    pub position: i32,
    pub active: bool,
}

struct CleanSettings {
    label: String,
    unit: String,
    choices: Vec<String>,
    item_types: Vec<String>,
}

fn clean(settings: &AttributeSettings, data_type: AttributeType) -> AppResult<CleanSettings> {
    let label = domain::require_text("Label", &settings.label, 100)?;
    let unit = domain::limit_text("Unit", &settings.unit, 16)?;
    let choices = domain::parse_attribute_choices(&settings.choices)?;
    if data_type == AttributeType::Choice && choices.is_empty() {
        return Err(AppError::BadRequest(
            "A choice list needs at least one choice.".into(),
        ));
    }
    if data_type != AttributeType::Choice && !choices.is_empty() {
        return Err(AppError::BadRequest(
            "Only choice lists have choices.".into(),
        ));
    }
    let mut item_types = Vec::new();
    for t in settings.item_types.iter().filter(|t| !t.trim().is_empty()) {
        let parsed = ItemType::parse(t)
            .ok_or_else(|| AppError::BadRequest(format!("Unknown item type '{t}'.")))?;
        if !item_types.contains(&parsed.as_str().to_string()) {
            item_types.push(parsed.as_str().to_string());
        }
    }
    Ok(CleanSettings {
        label,
        unit,
        choices,
        item_types,
    })
}

/// Creates a definition (administrators).
///
/// # Invariants
///
/// * **Permission:** admin role (403 otherwise). Key, data type and settings
///   are validated (400); an existing key is a 409.
/// * **Locks:** none beyond the unique key of the definition.
/// * **Audit:** `attribute.create`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    key: &str,
    data_type: &str,
    settings: AttributeSettings,
) -> AppResult<AttributeDef> {
    actor.require(Role::Admin)?;
    let key = domain::normalize_attribute_key(key)?;
    let data_type = AttributeType::parse(data_type).ok_or_else(|| {
        AppError::BadRequest("Data type must be text, number, boolean or choice.".into())
    })?;
    let s = clean(&settings, data_type)?;
    let mut tx = pool.begin().await?;
    let def = sqlx::query_as::<_, AttributeDef>(&format!(
        "INSERT INTO attribute_defs (id, key, label, data_type, unit, choices, item_types,
                                     position, active, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE, $9)
         RETURNING {COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(&key)
    .bind(&s.label)
    .bind(data_type.as_str())
    .bind(&s.unit)
    .bind(&s.choices)
    .bind(&s.item_types)
    .bind(settings.position)
    .bind(actor.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(e, format!("The attribute key '{key}' is already in use."))
    })?;
    audit::record(
        &mut tx,
        Some(actor),
        "attribute.create",
        "attribute",
        Some(def.id),
        format!("Created attribute {} ({})", def.label, def.key),
        json!({ "key": def.key, "data_type": def.data_type, "item_types": def.item_types }),
    )
    .await?;
    tx.commit().await?;
    Ok(def)
}

/// Changes label, unit, choices, item types, order and the active flag
/// (administrators). Key and data type stay.
///
/// # Invariants
///
/// * **Permission:** admin role (403 otherwise); an unknown definition is
///   "not found".
/// * **Locks:** the definition row (`FOR UPDATE`).
/// * **Audit:** `attribute.update`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    settings: AttributeSettings,
) -> AppResult<AttributeDef> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let current = sqlx::query_as::<_, AttributeDef>(&format!(
        "SELECT {COLUMNS} FROM attribute_defs WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Attribute not found."))?;
    let s = clean(&settings, data_type_of(&current))?;
    let def = sqlx::query_as::<_, AttributeDef>(&format!(
        "UPDATE attribute_defs SET label = $2, unit = $3, choices = $4, item_types = $5,
                                   position = $6, active = $7
         WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(&s.label)
    .bind(&s.unit)
    .bind(&s.choices)
    .bind(&s.item_types)
    .bind(settings.position)
    .bind(settings.active)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "attribute.update",
        "attribute",
        Some(id),
        format!("Updated attribute {} ({})", def.label, def.key),
        json!({
            "label": def.label, "unit": def.unit, "choices": def.choices,
            "item_types": def.item_types, "position": def.position, "active": def.active,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(def)
}

/// Whether `raw` (form or API text) denotes the stored value `v`.
fn same_value(v: &Value, raw: &str) -> bool {
    match v {
        Value::String(s) => s == raw,
        Value::Bool(b) => matches!(
            (b, raw.to_ascii_lowercase().as_str()),
            (true, "yes" | "true") | (false, "no" | "false")
        ),
        Value::Number(n) => n
            .as_f64()
            .zip(raw.replace(',', ".").parse::<f64>().ok())
            .is_some_and(|(a, b)| a == b),
        _ => false,
    }
}

/// Applies attribute changes (`key → text`, empty text removes the value) to
/// the stored values of a revision of an item of `item_type` and returns the
/// new object. Values of other keys stay as they are.
pub(crate) async fn apply_changes(
    conn: &mut PgConnection,
    item_type: &str,
    current: &Value,
    changes: &[(String, String)],
) -> AppResult<Value> {
    let mut values: Map<String, Value> = current.as_object().cloned().unwrap_or_default();
    if changes.is_empty() {
        return Ok(Value::Object(values));
    }
    let defs = list_conn(conn, false).await?;
    for (raw_key, raw_value) in changes {
        let key = domain::normalize_attribute_key(raw_key)?;
        // A value sent back unchanged (the edit form sends every field) is
        // kept even if it would no longer pass: a removed choice, an
        // attribute deactivated or narrowed to other item types meanwhile.
        if values
            .get(&key)
            .is_some_and(|v| same_value(v, raw_value.trim()))
        {
            continue;
        }
        let def = defs
            .iter()
            .find(|d| d.key == key)
            .ok_or_else(|| AppError::BadRequest(format!("Unknown attribute '{key}'.")))?;
        let parsed =
            domain::parse_attribute_value(&def.label, data_type_of(def), &def.choices, raw_value)?;
        match parsed {
            None => {
                values.remove(&key);
            }
            Some(value) => {
                if !def.active {
                    return Err(AppError::BadRequest(format!(
                        "The attribute '{}' is no longer in use.",
                        def.label
                    )));
                }
                if !applies_to(def, item_type) {
                    return Err(AppError::BadRequest(format!(
                        "The attribute '{}' does not apply to items of type {item_type}.",
                        def.label
                    )));
                }
                values.insert(key, value);
            }
        }
    }
    Ok(Value::Object(values))
}
