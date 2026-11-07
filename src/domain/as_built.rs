//! As-built records: what the deviation of a built unit from its design concerns and how it was decided.

use serde::{Deserialize, Serialize};

use super::ItemType;

impl ItemType {
    /// Whether a physical unit (an as-built item) can be built to a revision
    /// of this type.
    pub fn can_be_built_to(self) -> bool {
        matches!(
            self,
            ItemType::Part | ItemType::Assembly | ItemType::Software
        )
    }
}

/// What a deviation of the built unit from its design concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviationKind {
    /// A measured dimension or tolerance (a bore too large, a flatness).
    Dimension,
    /// Another material, heat treatment or surface finish.
    Material,
    /// Another, missing or additional component (also visible in the
    /// as-built BOM).
    Component,
    /// A manufacturing step done differently (welding, torque, bonding).
    Process,
    Other,
}

impl DeviationKind {
    pub const ALL: [DeviationKind; 5] = [
        DeviationKind::Dimension,
        DeviationKind::Material,
        DeviationKind::Component,
        DeviationKind::Process,
        DeviationKind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DeviationKind::Dimension => "dimension",
            DeviationKind::Material => "material",
            DeviationKind::Component => "component",
            DeviationKind::Process => "process",
            DeviationKind::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeviationKind::Dimension => "Dimension",
            DeviationKind::Material => "Material",
            DeviationKind::Component => "Component",
            DeviationKind::Process => "Process",
            DeviationKind::Other => "Other",
        }
    }

    pub fn parse(s: &str) -> Option<DeviationKind> {
        DeviationKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s.trim().to_ascii_lowercase())
    }
}

/// The decision about a deviation (the disposition of a nonconformance).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Not decided yet: the as-built revision cannot be submitted for release.
    Open,
    /// Accepted as it is (a concession).
    UseAsIs,
    /// Reworked to the design.
    Rework,
    /// Repaired: usable, but still not to the design.
    Repair,
    /// The unit or the component is scrapped.
    Scrap,
}

impl Disposition {
    pub const ALL: [Disposition; 5] = [
        Disposition::Open,
        Disposition::UseAsIs,
        Disposition::Rework,
        Disposition::Repair,
        Disposition::Scrap,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Open => "open",
            Disposition::UseAsIs => "use_as_is",
            Disposition::Rework => "rework",
            Disposition::Repair => "repair",
            Disposition::Scrap => "scrap",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Disposition::Open => "Open",
            Disposition::UseAsIs => "Use as is",
            Disposition::Rework => "Rework",
            Disposition::Repair => "Repair",
            Disposition::Scrap => "Scrap",
        }
    }

    /// Also accepts `use-as-is` and `use as is`; empty means open.
    pub fn parse(s: &str) -> Option<Disposition> {
        let s = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        if s.is_empty() {
            return Some(Disposition::Open);
        }
        Disposition::ALL.into_iter().find(|d| d.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NumberContext, NumberScheme};

    #[test]
    fn as_built_type_and_enums() {
        let ctx = NumberContext {
            project: "ROVER",
            item_type: ItemType::AsBuilt,
            year: 2026,
            month: 11,
        };
        assert_eq!(NumberScheme::default_scheme().format(&ctx, 7), "AB-000007");
        assert_eq!(ItemType::parse("As-Built"), Some(ItemType::AsBuilt));
        assert_eq!(ItemType::parse("as built"), Some(ItemType::AsBuilt));
        assert!(ItemType::Assembly.can_be_built_to());
        assert!(!ItemType::AsBuilt.can_be_built_to());
        assert!(!ItemType::Simulation.can_be_built_to());
        assert!(!ItemType::Document.can_be_built_to());
        for k in DeviationKind::ALL {
            assert_eq!(DeviationKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(
            DeviationKind::parse(" Dimension "),
            Some(DeviationKind::Dimension)
        );
        assert_eq!(DeviationKind::parse("colour"), None);
        for d in Disposition::ALL {
            assert_eq!(Disposition::parse(d.as_str()), Some(d));
        }
        assert_eq!(Disposition::parse("Use-as-is"), Some(Disposition::UseAsIs));
        assert_eq!(Disposition::parse("use as is"), Some(Disposition::UseAsIs));
        assert_eq!(Disposition::parse(""), Some(Disposition::Open));
        assert_eq!(Disposition::parse("ship it"), None);
    }
}
