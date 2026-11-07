//! Custom attributes and classification.

use serde::{Deserialize, Serialize};

use super::*;
use crate::error::AppError;

/// Make, buy or standard part – master data of an item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// Designed and made in-house (the default).
    #[default]
    Make,
    /// Bought from a supplier (e.g. a motor, a bearing).
    Purchased,
    /// Standard or library part (screws, washers, Content Center, Toolbox).
    Standard,
}

impl Classification {
    pub const ALL: [Classification; 3] = [
        Classification::Make,
        Classification::Purchased,
        Classification::Standard,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Classification::Make => "make",
            Classification::Purchased => "purchased",
            Classification::Standard => "standard",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Classification::Make => "In-house",
            Classification::Purchased => "Purchased",
            Classification::Standard => "Standard part",
        }
    }

    pub fn parse(s: &str) -> Option<Classification> {
        match s.trim().to_ascii_lowercase().as_str() {
            "make" | "in-house" | "inhouse" | "normal" => Some(Classification::Make),
            "purchased" | "buy" | "bought" => Some(Classification::Purchased),
            "standard" | "standard part" | "library" => Some(Classification::Standard),
            _ => None,
        }
    }

    /// The classification a BOM export states in its *BOM structure* or
    /// procurement column (Inventor: Normal, Purchased, Phantom, Inseparable;
    /// German exports: Kaufteil, Normteil, Eigenfertigung …). `None` for an
    /// empty or unknown value.
    pub fn from_bom_structure(raw: &str) -> Option<Classification> {
        let s: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        match s.as_str() {
            "normal" | "make" | "inhouse" | "eigenfertigung" | "eigenteil" | "fertigungsteil"
            | "fertigung" | "phantom" | "inseparable" | "untrennbar" => Some(Classification::Make),
            "purchased" | "purchase" | "buy" | "bought" | "kaufteil" | "zukaufteil" | "zukauf"
            | "gekauft" | "einkauf" | "einkaufsteil" => Some(Classification::Purchased),
            "standard" | "standardpart" | "normteil" | "norm" | "normalien" | "library"
            | "librarypart" | "contentcenter" | "toolbox" | "katalogteil" => {
                Some(Classification::Standard)
            }
            _ => None,
        }
    }

    /// Parses an optional form or API value; empty means "unchanged/default".
    pub fn parse_field(raw: Option<&str>) -> Result<Option<Classification>, AppError> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => Classification::parse(s).map(Some).ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Unknown classification '{s}': use make, purchased or standard."
                ))
            }),
        }
    }
}

/// Data type of a custom attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeType {
    Text,
    Number,
    Boolean,
    /// One of a fixed list of values.
    Choice,
}

impl AttributeType {
    pub const ALL: [AttributeType; 4] = [
        AttributeType::Text,
        AttributeType::Number,
        AttributeType::Boolean,
        AttributeType::Choice,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AttributeType::Text => "text",
            AttributeType::Number => "number",
            AttributeType::Boolean => "boolean",
            AttributeType::Choice => "choice",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AttributeType::Text => "Text",
            AttributeType::Number => "Number",
            AttributeType::Boolean => "Yes/no",
            AttributeType::Choice => "Choice list",
        }
    }

    pub fn parse(s: &str) -> Option<AttributeType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" => Some(AttributeType::Text),
            "number" => Some(AttributeType::Number),
            "boolean" | "bool" | "yes/no" => Some(AttributeType::Boolean),
            "choice" | "list" => Some(AttributeType::Choice),
            _ => None,
        }
    }
}

/// Longest text value of an attribute.
pub const MAX_ATTRIBUTE_TEXT: usize = 500;

/// Attribute keys: `a-z 0-9 _`, starting with a letter, at most 40
/// characters (`vendor`, `length_mm`). Upper case and spaces/dashes are
/// folded (`Order No.` → `order_no`).
pub fn normalize_attribute_key(raw: &str) -> Result<String, AppError> {
    let mut key = String::new();
    for c in raw.trim().chars() {
        match c {
            'a'..='z' | '0'..='9' => key.push(c),
            'A'..='Z' => key.push(c.to_ascii_lowercase()),
            ' ' | '-' | '_' | '.' | '/' if !key.is_empty() && !key.ends_with('_') => key.push('_'),
            ' ' | '-' | '_' | '.' | '/' => {}
            _ => {
                return Err(AppError::BadRequest(format!(
                    "Invalid attribute key '{raw}': use letters, digits and '_'."
                )))
            }
        }
    }
    let key = key.trim_end_matches('_').to_string();
    if key.is_empty() || key.len() > 40 || !key.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(AppError::BadRequest(format!(
            "Invalid attribute key '{raw}': 1–40 characters a–z, 0–9 and '_', starting with a letter."
        )));
    }
    Ok(key)
}

/// The choices of a choice attribute from free text (one per line or
/// separated by `;`), trimmed, without duplicates.
pub fn parse_attribute_choices(raw: &str) -> Result<Vec<String>, AppError> {
    let mut out: Vec<String> = Vec::new();
    for c in raw
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        if c.chars().count() > 100 {
            return Err(AppError::BadRequest(format!(
                "Choice '{c}' is longer than 100 characters."
            )));
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(c)) {
            out.push(c.to_string());
        }
    }
    if out.len() > 200 {
        return Err(AppError::BadRequest("At most 200 choices.".into()));
    }
    Ok(out)
}

fn parse_yes_no(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" | "1" | "on" | "ja" | "x" => Some(true),
        "no" | "n" | "false" | "0" | "off" | "nein" => Some(false),
        _ => None,
    }
}

/// Validates one attribute value given as text (form field, or an API value
/// converted to text) and returns it as JSON; empty input removes the value.
pub fn parse_attribute_value(
    label: &str,
    data_type: AttributeType,
    choices: &[String],
    raw: &str,
) -> Result<Option<serde_json::Value>, AppError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let value = match data_type {
        AttributeType::Text => {
            serde_json::Value::String(limit_text(label, raw, MAX_ATTRIBUTE_TEXT)?)
        }
        AttributeType::Number => {
            let n = parse_decimal(label, Some(raw))?.unwrap_or_default();
            if !n.is_finite() {
                return Err(AppError::BadRequest(format!(
                    "{label}: '{raw}' is not a number."
                )));
            }
            serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| AppError::BadRequest(format!("{label}: '{raw}' is not a number.")))?
        }
        AttributeType::Boolean => {
            serde_json::Value::Bool(parse_yes_no(raw).ok_or_else(|| {
                AppError::BadRequest(format!("{label}: '{raw}' is not yes or no."))
            })?)
        }
        AttributeType::Choice => {
            let choice = choices
                .iter()
                .find(|c| c.eq_ignore_ascii_case(raw))
                .ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "{label}: '{raw}' is not one of {}.",
                        choices.join(", ")
                    ))
                })?;
            serde_json::Value::String(choice.clone())
        }
    };
    Ok(Some(value))
}

/// An API value as the text [`parse_attribute_value`] reads (`null` → empty).
pub fn attribute_input_text(value: &serde_json::Value) -> Result<String, AppError> {
    match value {
        serde_json::Value::Null => Ok(String::new()),
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        _ => Err(AppError::BadRequest(
            "Attribute values are text, numbers, true/false or null.".into(),
        )),
    }
}

/// A search condition on one attribute.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeCondition {
    /// The attribute has any value.
    Present,
    /// Text or choice containing the text (case-insensitive).
    Contains(String),
    Bool(bool),
    /// Number in `[min, max]`; either bound may be open.
    Range(Option<f64>, Option<f64>),
}

/// Parses a search value for an attribute of `data_type`: empty = has a
/// value; text and choices match as substring; yes/no; numbers accept `12`,
/// `>=10`, `<=20` and `10..20` (no `10-20`, which would clash with negative
/// numbers).
pub fn parse_attribute_condition(
    label: &str,
    data_type: AttributeType,
    raw: &str,
) -> Result<AttributeCondition, AppError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(AttributeCondition::Present);
    }
    let num = |s: &str| -> Result<Option<f64>, AppError> {
        parse_decimal(label, Some(s)).and_then(|n| match n {
            Some(v) if !v.is_finite() => Err(AppError::BadRequest(format!(
                "{label}: '{s}' is not a number."
            ))),
            other => Ok(other),
        })
    };
    Ok(match data_type {
        AttributeType::Text | AttributeType::Choice => AttributeCondition::Contains(raw.into()),
        AttributeType::Boolean => AttributeCondition::Bool(
            parse_yes_no(raw)
                .ok_or_else(|| AppError::BadRequest(format!("{label}: search for yes or no.")))?,
        ),
        AttributeType::Number => {
            if let Some((lo, hi)) = raw.split_once("..") {
                AttributeCondition::Range(num(lo)?, num(hi)?)
            } else if let Some(v) = raw.strip_prefix(">=") {
                AttributeCondition::Range(num(v)?, None)
            } else if let Some(v) = raw.strip_prefix("<=") {
                AttributeCondition::Range(None, num(v)?)
            } else {
                let v = num(raw)?;
                AttributeCondition::Range(v, v)
            }
        }
    })
}
