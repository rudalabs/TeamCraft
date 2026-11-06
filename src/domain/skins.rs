//! UI skins and avatar helpers.

use serde::Serialize;

/// A selectable look of the UI. The colours live in `static/style.css`
/// (`[data-skin="<id>"]`), the logos in `media/`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Skin {
    pub id: &'static str,
    pub name: &'static str,
    /// Path below `/static/media/`.
    pub logo: &'static str,
    pub description: &'static str,
    /// Light, dark, or following the operating system (`prefers-color-scheme`).
    pub scheme: Scheme,
}

/// The colour scheme of a skin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    Light,
    Dark,
    /// Light or dark, as the operating system (browser) prefers.
    System,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Light => "light",
            Scheme::Dark => "dark",
            Scheme::System => "system",
        }
    }
}

pub const DEFAULT_SKIN: &str = "teamcraft";

/// The TeamCraft icon (shown in the header, the footer and as favicon).
pub const TEAMCRAFT_ICON: &str = "logos/TeamCraft_Icon.svg";

pub const SKINS: [Skin; 10] = [
    Skin {
        id: "teamcraft",
        name: "TeamCraft",
        logo: TEAMCRAFT_ICON,
        description: "Follows your system: light or dark",
        scheme: Scheme::System,
    },
    Skin {
        id: "teamcraft-day",
        name: "TeamCraft Light",
        logo: TEAMCRAFT_ICON,
        description: "House style, light",
        scheme: Scheme::Light,
    },
    Skin {
        id: "teamcraft-night",
        name: "TeamCraft Dark",
        logo: TEAMCRAFT_ICON,
        description: "House style, dark",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "arch",
        name: "Arch",
        logo: TEAMCRAFT_ICON,
        description: "Blueprint grid",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "cachyos",
        name: "CachyOS",
        logo: TEAMCRAFT_ICON,
        description: "Teal dot matrix",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "endeavouros",
        name: "EndeavourOS",
        logo: TEAMCRAFT_ICON,
        description: "Purple sunset",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "kde",
        name: "KDE",
        logo: TEAMCRAFT_ICON,
        description: "Breeze dark",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "kubuntu",
        name: "Kubuntu",
        logo: TEAMCRAFT_ICON,
        description: "Blue rings",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "opensuse",
        name: "openSUSE",
        logo: TEAMCRAFT_ICON,
        description: "Chameleon scales",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "ubuntu",
        name: "Ubuntu",
        logo: TEAMCRAFT_ICON,
        description: "Circle of friends",
        scheme: Scheme::Dark,
    },
];

/// Returns the skin id if it is known, otherwise the default skin.
pub fn skin_or_default(id: &str) -> &'static str {
    let id = id.trim();
    SKINS
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.id)
        .unwrap_or(DEFAULT_SKIN)
}

pub fn is_skin(id: &str) -> bool {
    SKINS.iter().any(|s| s.id == id)
}

/// The colour scheme of a skin (unknown ids: the default skin).
pub fn skin_scheme(id: &str) -> Scheme {
    let id = skin_or_default(id);
    SKINS
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.scheme)
        .unwrap_or(Scheme::System)
}

/// Up to two initials for the avatar: first letters of the first and last
/// word of the display name (the username when the name is empty).
pub fn initials(display_name: &str, username: &str) -> String {
    let source = if display_name.trim().is_empty() {
        username
    } else {
        display_name
    };
    let words: Vec<&str> = source.split_whitespace().collect();
    let first = |w: &str| {
        w.chars()
            .next()
            .map(|c| c.to_uppercase().collect::<String>())
    };
    match words.as_slice() {
        [] => "?".to_string(),
        [one] => first(one).unwrap_or_default(),
        [a, .., b] => format!(
            "{}{}",
            first(a).unwrap_or_default(),
            first(b).unwrap_or_default()
        ),
    }
}

/// A stable avatar tint 1..=6 (`--avatar-N` in skins.css) from the username.
pub fn avatar_tint(username: &str) -> u32 {
    // FNV-1a: stable across builds and platforms
    let hash = username.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    hash % 6 + 1
}

/// Whether a skin is always light (the System skin is not: it may be dark).
pub fn skin_is_light(id: &str) -> bool {
    skin_scheme(id) == Scheme::Light
}
