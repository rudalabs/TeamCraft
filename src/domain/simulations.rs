//! Simulations: bases and result values.

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// What a simulation takes from the revision it is based on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisRole {
    /// The analysed geometry (part, assembly).
    Geometry,
    /// A mesh / FE model kept as its own item.
    Mesh,
    /// Material data (specification, data sheet).
    Material,
    /// Where the loads come from (load specification, test report).
    LoadCase,
    Other,
}

impl BasisRole {
    pub const ALL: [BasisRole; 5] = [
        BasisRole::Geometry,
        BasisRole::Mesh,
        BasisRole::Material,
        BasisRole::LoadCase,
        BasisRole::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BasisRole::Geometry => "geometry",
            BasisRole::Mesh => "mesh",
            BasisRole::Material => "material",
            BasisRole::LoadCase => "load_case",
            BasisRole::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BasisRole::Geometry => "Geometry",
            BasisRole::Mesh => "Mesh",
            BasisRole::Material => "Material",
            BasisRole::LoadCase => "Load case",
            BasisRole::Other => "Other",
        }
    }

    /// Also accepts `load-case` and `load case`.
    pub fn parse(s: &str) -> Option<BasisRole> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "geometry" => Some(BasisRole::Geometry),
            "mesh" => Some(BasisRole::Mesh),
            "material" => Some(BasisRole::Material),
            "load_case" => Some(BasisRole::LoadCase),
            "other" => Some(BasisRole::Other),
            _ => None,
        }
    }
}

/// Whether a result value is within its limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Pass,
    Fail,
    /// No limit given: the value is informative only (`"none"`).
    #[serde(rename = "none")]
    NoLimits,
}

impl ResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultStatus::Pass => "pass",
            ResultStatus::Fail => "fail",
            ResultStatus::NoLimits => "none",
        }
    }
}

/// Pass when `lower <= value <= upper` (limits inclusive, either may be
/// missing), fail otherwise; [`ResultStatus::NoLimits`] without limits.
pub fn result_status(value: f64, lower: Option<f64>, upper: Option<f64>) -> ResultStatus {
    if lower.is_none() && upper.is_none() {
        return ResultStatus::NoLimits;
    }
    let above_lower = lower.is_none_or(|l| value >= l);
    let below_upper = upper.is_none_or(|u| value <= u);
    if value.is_finite() && above_lower && below_upper {
        ResultStatus::Pass
    } else {
        ResultStatus::Fail
    }
}

/// Validates a result value and its limits (finite numbers, lower ≤ upper).
pub fn validate_result_numbers(
    name: &str,
    value: f64,
    lower: Option<f64>,
    upper: Option<f64>,
) -> Result<(), AppError> {
    if !value.is_finite() {
        return Err(AppError::BadRequest(format!(
            "{name}: the value must be a finite number."
        )));
    }
    if lower.is_some_and(|v| !v.is_finite()) || upper.is_some_and(|v| !v.is_finite()) {
        return Err(AppError::BadRequest(format!(
            "{name}: the limits must be finite numbers."
        )));
    }
    if let (Some(l), Some(u)) = (lower, upper) {
        if l > u {
            return Err(AppError::BadRequest(format!(
                "{name}: the lower limit {l} is above the upper limit {u}."
            )));
        }
    }
    Ok(())
}
