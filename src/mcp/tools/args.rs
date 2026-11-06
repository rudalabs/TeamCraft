//! Typed access to the arguments of a tool call.

use super::*;

pub(super) fn bad(message: impl Into<String>) -> AppError {
    AppError::BadRequest(message.into())
}

/// Typed access to the `arguments` object of a tool call.
pub struct Args<'a>(Option<&'a Map<String, Value>>);

impl<'a> Args<'a> {
    pub fn new(value: &'a Value) -> Self {
        Args(value.as_object())
    }

    pub(super) fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.and_then(|m| m.get(key)).filter(|v| !v.is_null())
    }

    /// A trimmed, non-empty string (`None` when missing or empty).
    pub fn opt_str(&self, key: &str) -> AppResult<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => {
                let s = s.trim();
                Ok((!s.is_empty()).then(|| s.to_string()))
            }
            Some(_) => Err(bad(format!("`{key}` must be a string."))),
        }
    }

    pub fn req_str(&self, key: &str) -> AppResult<String> {
        self.opt_str(key)?
            .ok_or_else(|| bad(format!("`{key}` is required.")))
    }

    /// A positive count, clamped to `1..=max`; numeric strings are accepted.
    pub fn limit(&self, key: &str, default: i64, max: i64) -> AppResult<i64> {
        let n = match self.get(key) {
            None => default,
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .ok_or_else(|| bad(format!("`{key}` must be an integer.")))?,
            Some(Value::String(s)) => s
                .trim()
                .parse()
                .map_err(|_| bad(format!("`{key}` must be an integer.")))?,
            Some(_) => return Err(bad(format!("`{key}` must be an integer."))),
        };
        Ok(n.clamp(1, max))
    }

    /// A count of zero or more (no clamping above `max`: larger is an error).
    pub fn count(&self, key: &str, default: i64, max: i64) -> AppResult<i64> {
        let n = match self.get(key) {
            None => default,
            Some(Value::Number(n)) => n
                .as_i64()
                .ok_or_else(|| bad(format!("`{key}` must be an integer.")))?,
            Some(Value::String(s)) => s
                .trim()
                .parse()
                .map_err(|_| bad(format!("`{key}` must be an integer.")))?,
            Some(_) => return Err(bad(format!("`{key}` must be an integer."))),
        };
        if !(0..=max).contains(&n) {
            return Err(bad(format!("`{key}` must be between 0 and {max}.")));
        }
        Ok(n)
    }

    pub fn flag(&self, key: &str, default: bool) -> AppResult<bool> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(true),
                "false" | "no" | "0" => Ok(false),
                _ => Err(bad(format!("`{key}` must be true or false."))),
            },
            Some(_) => Err(bad(format!("`{key}` must be true or false."))),
        }
    }

    /// A list of strings; a single string counts as a list of one.
    pub fn list(&self, key: &str, max: usize) -> AppResult<Vec<String>> {
        let out: Vec<String> = match self.get(key) {
            None => Vec::new(),
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| bad(format!("`{key}` must be a list of strings.")))
                })
                .collect::<AppResult<_>>()?,
            Some(_) => return Err(bad(format!("`{key}` must be a list of strings."))),
        };
        let out: Vec<String> = out
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if out.len() > max {
            return Err(bad(format!("`{key}` takes at most {max} entries.")));
        }
        Ok(out)
    }

    pub(super) fn rule(&self) -> AppResult<Option<RevisionRule>> {
        self.opt_str("rule")?
            .map(|r| {
                RevisionRule::parse(&r)
                    .ok_or_else(|| bad("rule must be latest, released or as_released."))
            })
            .transpose()
    }
}

/// `PRT-000012` or `PRT-000012/B` → number and optional revision code.
/// Item numbers never contain a slash.
pub fn parse_item_ref(s: &str) -> (String, Option<String>) {
    match s.trim().split_once('/') {
        Some((number, code)) if !code.trim().is_empty() => {
            (number.trim().to_string(), Some(code.trim().to_string()))
        }
        Some((number, _)) => (number.trim().to_string(), None),
        None => (s.trim().to_string(), None),
    }
}
