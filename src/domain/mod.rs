//! Pure domain rules: roles, lifecycle states, numbering and validation.
//!
//! Nothing in here touches the database, which keeps the rules easy to unit test.
//!
//! The rules are split by area; everything is re-exported here, so callers
//! keep using `crate::domain::Name`.

mod as_built;
mod attributes;
mod changes;
mod concurrency;
mod files_hint;
mod items;
mod lifecycle;
mod notifications;
mod references;
mod roles;
mod simulations;
mod skins;
mod tokens;
mod validation;

pub use as_built::*;
pub use attributes::*;
pub use changes::*;
pub use concurrency::*;
pub use files_hint::*;
pub use items::*;
pub use lifecycle::*;
pub use notifications::*;
pub use references::*;
pub use roles::*;
pub use simulations::*;
pub use skins::*;
pub use tokens::*;
pub use validation::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_attributes() {
        use serde_json::json;
        assert_eq!(normalize_attribute_key(" Order No. ").unwrap(), "order_no");
        assert_eq!(normalize_attribute_key("length_mm").unwrap(), "length_mm");
        // separators at the start and in a row fold into nothing or one '_'
        // (found by cargo mutants)
        assert_eq!(
            normalize_attribute_key("--Order -- No").unwrap(),
            "order_no"
        );
        assert_eq!(normalize_attribute_key("/vendor").unwrap(), "vendor");
        assert_eq!(normalize_attribute_key("a._/b").unwrap(), "a_b");
        assert_eq!(normalize_attribute_key(&"a".repeat(40)).unwrap().len(), 40);
        for bad in ["", "1abc", "ä", "a/b?", &"a".repeat(41)] {
            assert!(normalize_attribute_key(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse_attribute_choices("Steel; steel\n Aluminium \n\n").unwrap(),
            ["Steel", "Aluminium"]
        );
        // both limits are inclusive (found by cargo mutants)
        assert!(parse_attribute_choices(&"x".repeat(100)).is_ok());
        for n in [101, 102] {
            assert!(parse_attribute_choices(&"x".repeat(n)).is_err(), "{n}");
        }
        let choices = |n: usize| {
            (0..n)
                .map(|i| format!("c{i}"))
                .collect::<Vec<_>>()
                .join(";")
        };
        assert_eq!(parse_attribute_choices(&choices(200)).unwrap().len(), 200);
        for n in [201, 202] {
            assert!(parse_attribute_choices(&choices(n)).is_err(), "{n}");
        }

        let none: [String; 0] = [];
        let v = |t, raw| parse_attribute_value("X", t, &none, raw);
        assert_eq!(
            v(AttributeType::Text, "  Bosch ").unwrap(),
            Some(json!("Bosch"))
        );
        assert_eq!(
            v(AttributeType::Text, "   ").unwrap(),
            None,
            "empty removes"
        );
        assert_eq!(v(AttributeType::Number, "12,5").unwrap(), Some(json!(12.5)));
        assert!(v(AttributeType::Number, "12 mm").is_err());
        assert!(v(AttributeType::Number, "NaN").is_err());
        assert_eq!(v(AttributeType::Boolean, "Ja").unwrap(), Some(json!(true)));
        assert_eq!(
            v(AttributeType::Boolean, "false").unwrap(),
            Some(json!(false))
        );
        assert!(v(AttributeType::Boolean, "maybe").is_err());
        assert!(v(AttributeType::Text, &"x".repeat(501)).is_err());
        let choices = ["S235".to_string(), "S355".to_string()];
        assert_eq!(
            parse_attribute_value("Steel", AttributeType::Choice, &choices, "s355").unwrap(),
            Some(json!("S355")),
            "choices keep their spelling"
        );
        assert!(parse_attribute_value("Steel", AttributeType::Choice, &choices, "S275").is_err());

        assert_eq!(attribute_input_text(&json!(3)).unwrap(), "3");
        assert_eq!(attribute_input_text(&json!(true)).unwrap(), "true");
        assert_eq!(attribute_input_text(&json!(null)).unwrap(), "");
        assert_eq!(attribute_input_text(&json!("Bosch")).unwrap(), "Bosch");
        assert!(attribute_input_text(&json!([1])).is_err());

        let c = |t, raw| parse_attribute_condition("X", t, raw).unwrap();
        use AttributeCondition::*;
        assert_eq!(c(AttributeType::Text, " "), Present);
        assert_eq!(c(AttributeType::Choice, "bos"), Contains("bos".into()));
        assert_eq!(c(AttributeType::Boolean, "no"), Bool(false));
        assert_eq!(
            c(AttributeType::Number, "12"),
            Range(Some(12.0), Some(12.0))
        );
        assert_eq!(c(AttributeType::Number, ">=1,5"), Range(Some(1.5), None));
        assert_eq!(c(AttributeType::Number, "<=-2"), Range(None, Some(-2.0)));
        assert_eq!(
            c(AttributeType::Number, "10..20"),
            Range(Some(10.0), Some(20.0))
        );
        assert_eq!(c(AttributeType::Number, "..20"), Range(None, Some(20.0)));
        assert!(parse_attribute_condition("X", AttributeType::Number, "10-20").is_err());
        // a number that overflows to infinity is no bound (found by cargo mutants)
        for raw in ["1e999", "1e999..", "..-1e999"] {
            assert!(
                parse_attribute_condition("X", AttributeType::Number, raw).is_err(),
                "{raw}"
            );
        }

        for k in Classification::ALL {
            assert_eq!(Classification::parse(k.as_str()), Some(k));
        }
        assert_eq!(
            Classification::parse("Purchased"),
            Some(Classification::Purchased)
        );
        assert_eq!(Classification::parse_field(Some(" ")).unwrap(), None);
        for (raw, expected) in [
            ("Normal", Some(Classification::Make)),
            ("Phantom", Some(Classification::Make)),
            ("Purchased", Some(Classification::Purchased)),
            ("Kaufteil", Some(Classification::Purchased)),
            ("Zukaufteil", Some(Classification::Purchased)),
            ("Content Center", Some(Classification::Standard)),
            ("Normteil", Some(Classification::Standard)),
            ("", None),
            ("Superpart", None),
        ] {
            assert_eq!(Classification::from_bom_structure(raw), expected, "{raw:?}");
        }
        assert!(Classification::parse_field(Some("phantom")).is_err());
        for t in AttributeType::ALL {
            assert_eq!(AttributeType::parse(t.as_str()), Some(t));
        }
        // the labels on the item and attribute pages (found by cargo mutants)
        let labels: Vec<&str> = Classification::ALL.iter().map(|k| k.label()).collect();
        assert_eq!(labels, ["In-house", "Purchased", "Standard part"]);
        let labels: Vec<&str> = AttributeType::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels, ["Text", "Number", "Yes/no", "Choice list"]);
    }

    #[test]
    fn project_visibility_parse() {
        for v in [ProjectVisibility::Internal, ProjectVisibility::Restricted] {
            assert_eq!(ProjectVisibility::parse(v.as_str()), Some(v));
        }
        assert_eq!(
            ProjectVisibility::parse(" Restricted "),
            Some(ProjectVisibility::Restricted)
        );
        assert_eq!(ProjectVisibility::parse("secret"), None);
    }

    #[test]
    fn admin_read_parse() {
        for m in [AdminRead::Implicit, AdminRead::WithReason, AdminRead::None] {
            assert_eq!(AdminRead::parse(m.as_str()), Some(m));
            assert!(!m.label().is_empty());
        }
        assert_eq!(
            AdminRead::parse(" With_Reason "),
            Some(AdminRead::WithReason)
        );
        assert_eq!(AdminRead::parse("always"), None);
    }

    #[test]
    fn base_versions() {
        let sha = "AB".repeat(32);
        assert_eq!(
            BaseVersion::parse(&sha),
            Some(BaseVersion::Sha256("ab".repeat(32)))
        );
        assert_eq!(BaseVersion::parse(" 3 "), Some(BaseVersion::Version(3)));
        assert_eq!(BaseVersion::parse("v12"), Some(BaseVersion::Version(12)));
        assert_eq!(BaseVersion::parse(""), None);
        assert_eq!(BaseVersion::parse("-1"), None);
        assert_eq!(BaseVersion::parse("abc"), None);
        assert_eq!(
            BaseVersion::from_json(&serde_json::json!(2)).unwrap(),
            BaseVersion::Version(2)
        );
        assert!(BaseVersion::from_json(&serde_json::json!(-2)).is_err());
        assert!(BaseVersion::from_json(&serde_json::json!(null)).is_err());

        let cur = Some(ServerFile {
            version: 4,
            sha256: "c4",
        });
        assert_eq!(check_base(&BaseVersion::Version(4), cur, "new"), Ok(()));
        assert_eq!(
            check_base(&BaseVersion::Version(3), cur, "new"),
            Err(BaseConflict::Moved)
        );
        assert_eq!(
            check_base(&BaseVersion::Version(3), cur, "c4"),
            Ok(()),
            "identical content overwrites nothing"
        );
        assert_eq!(
            check_base(&BaseVersion::Sha256("C4".into()), cur, "new"),
            Ok(())
        );
        assert_eq!(
            check_base(&BaseVersion::Sha256("c3".into()), cur, "new"),
            Err(BaseConflict::Moved)
        );
        assert_eq!(
            check_base(&BaseVersion::Version(0), cur, "new"),
            Err(BaseConflict::AddedMeanwhile)
        );
        assert_eq!(check_base(&BaseVersion::Version(0), None, "new"), Ok(()));
        assert_eq!(
            check_base(&BaseVersion::Version(2), None, "new"),
            Err(BaseConflict::Removed)
        );
    }

    #[test]
    fn sync_states() {
        use FileSyncState::*;
        let history = [(1, "a1"), (2, "a2"), (3, "a3")];
        let cur = Some(ServerFile {
            version: 3,
            sha256: "a3",
        });
        let v = |n| BaseVersion::Version(n);
        assert_eq!(sync_state(cur, &history, Some("a3"), None), UpToDate);
        assert_eq!(sync_state(cur, &history, Some("A3"), Some(&v(1))), UpToDate);
        assert_eq!(sync_state(cur, &history, Some("a1"), None), Outdated);
        assert_eq!(sync_state(cur, &history, Some("zz"), None), Modified);
        assert_eq!(sync_state(cur, &history, Some("zz"), Some(&v(3))), Modified);
        assert_eq!(sync_state(cur, &history, Some("a2"), Some(&v(2))), Outdated);
        assert_eq!(sync_state(cur, &history, Some("zz"), Some(&v(2))), Conflict);
        assert_eq!(
            sync_state(
                cur,
                &history,
                Some("zz"),
                Some(&BaseVersion::Sha256("a1".into()))
            ),
            Conflict
        );
        assert_eq!(sync_state(cur, &history, Some("zz"), Some(&v(0))), Conflict);
        assert_eq!(sync_state(None, &[], Some("zz"), None), New);
        assert_eq!(sync_state(cur, &history, None, None), Missing);
        assert_eq!(FileSyncState::UpToDate.as_str(), "up_to_date");
    }

    #[test]
    fn ages() {
        assert_eq!(format_age(-5), "just now");
        assert_eq!(format_age(59), "just now");
        assert_eq!(format_age(60), "1 min");
        assert_eq!(format_age(3_600 * 5 + 7), "5 h");
        assert_eq!(format_age(86_400 + 1), "1 day");
        assert_eq!(format_age(86_400 * 9), "9 days");
    }

    #[test]
    fn skins() {
        assert_eq!(skin_or_default("arch"), "arch");
        assert_eq!(skin_or_default("nonsense"), DEFAULT_SKIN);
        assert!(is_skin("teamcraft-day"));
        assert!(!is_skin("TeamCraft"));
        let mut ids: Vec<_> = SKINS.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SKINS.len(), "skin ids are unique");
        // every skin has a CSS block
        let css = include_str!("../../shared/css/skins.css");
        for skin in SKINS.iter().filter(|s| s.id != DEFAULT_SKIN) {
            assert!(
                css.contains(&format!("[data-skin=\"{}\"] {{", skin.id)),
                "CSS for {}",
                skin.id
            );
        }
    }

    /// The desktop app has its own copy of the skin list (it runs without the
    /// server's templates); it must name the same skins and logos.
    #[test]
    fn desktop_app_knows_every_skin() {
        let app = include_str!("../../desktop/ui/js/core.js");
        for skin in SKINS {
            assert!(
                app.contains(&format!(
                    "id: \"{}\", name: \"{}\", logo: \"{}\"",
                    skin.id, skin.name, skin.logo
                )),
                "desktop/ui/js/core.js SKINS: {} ({}, {})",
                skin.id,
                skin.name,
                skin.logo
            );
        }
    }

    #[test]
    fn light_and_dark_skins() {
        assert_eq!(skin_scheme("teamcraft"), Scheme::System);
        assert_eq!(skin_scheme("teamcraft-night"), Scheme::Dark);
        assert_eq!(skin_scheme("nonsense"), Scheme::System, "unknown → default");
        assert!(skin_is_light("teamcraft-day"));
        assert!(!skin_is_light("teamcraft-night"));
        assert!(!skin_is_light("teamcraft"));
        assert!(!skin_is_light("nonsense"));
        assert_eq!(
            SKINS.iter().filter(|s| s.scheme == Scheme::Light).count(),
            1
        );
        assert_eq!(Scheme::System.as_str(), "system");
    }

    #[test]
    fn avatar_initials_and_tints() {
        assert_eq!(initials("Alex Morgan", "admin"), "AM");
        assert_eq!(initials("Erika Maria Mustermann", "erika"), "EM");
        assert_eq!(initials("", "max"), "M");
        assert_eq!(initials(" ", ""), "?");
        assert_eq!(initials("łukasz", "l"), "Ł");
        assert_eq!(avatar_tint("admin"), avatar_tint("admin"));
        assert!((1..=6).contains(&avatar_tint("")));
        assert!((1..=6).contains(&avatar_tint("someone else")));
        // FNV-1a is stable across builds: the tint of a user never changes,
        // and all six tints are used (found by cargo mutants)
        assert_eq!(
            [
                avatar_tint("admin"),
                avatar_tint("erika"),
                avatar_tint("max"),
                avatar_tint("")
            ],
            [3, 2, 6, 2]
        );
        let mut tints: Vec<u32> = (0..60).map(|i| avatar_tint(&format!("user{i}"))).collect();
        tints.sort_unstable();
        tints.dedup();
        assert_eq!(tints, [1, 2, 3, 4, 5, 6]);
    }

    // --- skins.css: token blocks and contrast --------------------------------

    const SKINS_CSS: &str = include_str!("../../shared/css/skins.css");

    /// The text between two marker comments.
    fn css_between<'a>(css: &'a str, begin: &str, end: &str) -> &'a str {
        let start = css
            .find(begin)
            .unwrap_or_else(|| panic!("{begin} in skins.css"))
            + begin.len();
        let stop = css[start..]
            .find(end)
            .unwrap_or_else(|| panic!("{end} in skins.css"))
            + start;
        &css[start..stop]
    }

    /// `--name: value` declarations of the first `{ … }` block in `text`.
    fn css_decls(text: &str) -> Vec<(String, String)> {
        let open = text.find('{').expect("a CSS block") + 1;
        let close = text[open..].find('}').expect("end of the CSS block") + open;
        let mut body = text[open..close].to_string();
        while let Some(a) = body.find("/*") {
            let b = body[a..]
                .find("*/")
                .map(|b| a + b + 2)
                .unwrap_or(body.len());
            body.replace_range(a..b, "");
        }
        body.split(';')
            .filter_map(|d| d.split_once(':'))
            .map(|(k, v)| {
                (
                    k.trim().to_string(),
                    v.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .filter(|(k, _)| !k.is_empty())
            .collect()
    }

    fn css_map(decls: &[(String, String)]) -> std::collections::HashMap<String, String> {
        decls.iter().cloned().collect()
    }

    #[test]
    fn dark_token_blocks_identical() {
        let media = css_decls(css_between(
            SKINS_CSS,
            "/* DARK-TOKENS:BEGIN media */",
            "/* DARK-TOKENS:END media */",
        ));
        let forced = css_decls(css_between(
            SKINS_CSS,
            "/* DARK-TOKENS:BEGIN forced */",
            "/* DARK-TOKENS:END forced */",
        ));
        assert!(media.len() > 40, "{} dark tokens", media.len());
        assert_eq!(
            media, forced,
            "the System dark block and the forced dark block differ"
        );
    }

    /// WCAG 2 relative luminance of `#rrggbb`.
    fn luminance(hex: &str) -> f64 {
        let hex = hex
            .strip_prefix('#')
            .filter(|h| h.len() == 6)
            .unwrap_or_else(|| panic!("not #rrggbb: {hex}"));
        let channel = |i: usize| {
            let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit")) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn skins_meet_contrast() {
        let light = css_map(&css_decls(css_between(
            SKINS_CSS,
            "/* LIGHT-TOKENS:BEGIN */",
            "/* LIGHT-TOKENS:END */",
        )));
        let dark = css_map(&css_decls(css_between(
            SKINS_CSS,
            "/* DARK-TOKENS:BEGIN forced */",
            "/* DARK-TOKENS:END forced */",
        )));
        let mut skins = vec![
            ("light base".to_string(), light),
            ("dark base".to_string(), dark.clone()),
        ];
        for skin in SKINS.iter().filter(|s| !s.id.starts_with("teamcraft")) {
            let selector = format!("[data-skin=\"{}\"] {{", skin.id);
            let at = SKINS_CSS
                .find(&selector)
                .unwrap_or_else(|| panic!("{selector}"));
            let mut tokens = dark.clone();
            tokens.extend(css_map(&css_decls(&SKINS_CSS[at..])));
            skins.push((skin.id.to_string(), tokens));
        }
        let mut failures = Vec::new();
        for (name, t) in &skins {
            // A token may name another one (`--count-bg: var(--primary)`);
            // it resolves against the same skin, as in the browser.
            let get = |k: &str| {
                let mut key = k;
                loop {
                    let v = t
                        .get(key)
                        .unwrap_or_else(|| panic!("{name}: {key}"))
                        .as_str();
                    match v.strip_prefix("var(").and_then(|r| r.strip_suffix(')')) {
                        Some(inner) => key = inner.trim(),
                        None => break v,
                    }
                }
            };
            let mut check = |fg: &str, bg: &str, min: f64| {
                let ratio = contrast(get(fg), get(bg));
                if ratio < min {
                    failures.push(format!("{name}: {fg} on {bg} = {ratio:.2} < {min}"));
                }
            };
            for fg in ["--text", "--text-muted", "--text-dim"] {
                for bg in ["--bg", "--surface", "--surface-2"] {
                    check(fg, bg, 4.5);
                }
            }
            check("--text", "--selected", 4.5);
            check("--text-muted", "--selected", 4.5);
            check("--text-dim", "--selected", 4.5);
            for bg in ["--bg", "--surface", "--surface-2", "--selected"] {
                check("--link", bg, 4.5);
            }
            check("--on-primary", "--primary", 4.5);
            // a hovered or pressed primary button keeps its edge and its label
            for state in ["--primary-hover", "--primary-active"] {
                check(state, "--surface", 3.0);
                check("--on-primary", state, 4.5);
            }
            for bg in ["--bg", "--surface", "--surface-2"] {
                check("--focus", bg, 3.0);
            }
            for bg in ["--surface", "--surface-2", "--input-bg", "--sidebar"] {
                check("--control-border", bg, 3.0);
            }
            for state in ["wip", "review", "ok", "danger", "impact"] {
                let fg = format!("--st-{state}-fg");
                check(&fg, &format!("--st-{state}-bg"), 4.5);
                check(&fg, "--surface", 4.5);
            }
            check("--st-lock-other", "--surface", 4.5);
            check("--count-fg", "--count-bg", 4.5);
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn revision_codes_follow_asme_alphabet() {
        assert_eq!(revision_code(1), "A");
        assert_eq!(revision_code(2), "B");
        assert_eq!(revision_code(8), "H");
        // I is skipped
        assert_eq!(revision_code(9), "J");
        assert_eq!(revision_code(20), "Y");
        assert_eq!(revision_code(21), "AA");
        assert_eq!(revision_code(22), "AB");
        assert_eq!(revision_code(40), "AY");
        assert_eq!(revision_code(41), "BA");
        assert_eq!(revision_code(420), "YY");
        assert_eq!(revision_code(421), "AAA");
    }

    #[test]
    fn revision_codes_round_trip() {
        for seq in 1..2000 {
            let code = revision_code(seq);
            assert_eq!(revision_seq(&code), Some(seq), "code {code}");
            for forbidden in ['I', 'O', 'Q', 'S', 'X', 'Z'] {
                assert!(!code.contains(forbidden), "{code} contains {forbidden}");
            }
        }
        assert_eq!(revision_seq("b"), Some(2));
        assert_eq!(revision_seq(""), None);
        assert_eq!(revision_seq("I"), None);
        // at most six letters, although seven would still fit an i32 (found
        // by cargo mutants)
        assert!(revision_seq("AAAAAA").is_some());
        assert_eq!(revision_seq("AAAAAAA"), None);
        assert_eq!(revision_seq("A1"), None);
    }

    #[test]
    fn lifecycle_transitions() {
        use LifecycleState::*;
        assert!(InWork.can_transition_to(InReview));
        assert!(InReview.can_transition_to(Released));
        assert!(InReview.can_transition_to(InWork));
        assert!(Released.can_transition_to(Obsolete));
        assert!(!InWork.can_transition_to(Released));
        assert!(!Released.can_transition_to(InWork));
        assert!(!Obsolete.can_transition_to(Released));
        assert!(InWork.is_editable());
        assert!(!InReview.is_editable());
        assert!(!Released.is_editable());
        assert!(InReview.is_open());
        assert!(!Released.is_open());
        for s in LifecycleState::ALL {
            assert_eq!(LifecycleState::parse(s.as_str()), Some(s));
        }
        // stored values, labels and the two flags of every state; file
        // retention values (found by cargo mutants)
        let states: Vec<(&str, &str, bool, bool)> = LifecycleState::ALL
            .iter()
            .map(|s| (s.as_str(), s.label(), s.is_editable(), s.is_open()))
            .collect();
        assert_eq!(
            states,
            [
                ("in_work", "WIP", true, true),
                ("in_review", "In Review", false, true),
                ("released", "Released", false, false),
                ("obsolete", "Obsolete", false, false),
            ]
        );
        assert_eq!(LifecycleState::parse(" Released "), Some(Released));
        assert_eq!(LifecycleState::parse("wip"), None);
        for r in [FileRetention::All, FileRetention::Latest] {
            assert_eq!(FileRetention::parse(r.as_str()), Some(r));
        }
        assert_eq!(
            (FileRetention::All.as_str(), FileRetention::Latest.as_str()),
            ("all", "latest")
        );
        assert_eq!(
            FileRetention::parse(" LATEST "),
            Some(FileRetention::Latest)
        );
        assert_eq!(FileRetention::parse("newest"), None);
    }

    #[test]
    fn lifecycle_rules_document_every_transition() {
        // every documented rule is allowed …
        for rule in LIFECYCLE_RULES {
            assert!(rule.from.can_transition_to(rule.to), "{rule:?}");
            assert!(!rule.how.is_empty() && !rule.who.is_empty());
        }
        // … and every allowed transition is documented exactly once
        for from in LifecycleState::ALL {
            for to in LifecycleState::ALL {
                let documented = LIFECYCLE_RULES
                    .iter()
                    .filter(|r| r.from == from && r.to == to)
                    .count();
                assert_eq!(
                    documented,
                    usize::from(from.can_transition_to(to)),
                    "{from:?} → {to:?}"
                );
            }
        }
        assert_eq!(rules_from(LifecycleState::InReview).len(), 2);
        assert!(
            rules_from(LifecycleState::Obsolete).is_empty(),
            "final state"
        );
        let json = serde_json::to_value(LIFECYCLE_RULES[0]).unwrap();
        assert_eq!(json["from"], "in_work");
        assert_eq!(json["to"], "in_review");
    }

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Viewer < Role::Editor);
        assert!(Role::Editor < Role::Admin);
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse(" ADMIN "), Some(Role::Admin));
        assert_eq!(Role::parse("CAD editor"), Some(Role::Editor));
        assert_eq!(Role::parse("engineer"), Some(Role::Editor));
        assert_eq!(Role::Editor.label(), "CAD editor");
        assert_eq!(Role::parse("root"), None);
    }

    #[test]
    fn separation_of_duties() {
        use uuid::Uuid;
        let (author, admin, editor, other) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let editors = [editor];
        let check = |who, submitter| conflicting_reviewer(who, author, submitter, &editors);
        assert_eq!(check(author, None), Some(ReviewConflict::Author));
        assert_eq!(check(editor, None), Some(ReviewConflict::Editor));
        assert_eq!(check(admin, Some(admin)), Some(ReviewConflict::Submitter));
        assert_eq!(check(admin, None), None);
        assert_eq!(check(other, Some(admin)), None);
        // the author wins over the other reasons
        assert_eq!(
            conflicting_reviewer(author, author, Some(author), &[author]),
            Some(ReviewConflict::Author)
        );
    }

    #[test]
    fn review_outcomes() {
        use Decision::*;
        assert_eq!(review_outcome(&[]), ReviewOutcome::Pending);
        assert_eq!(review_outcome(&[Pending, Approved]), ReviewOutcome::Pending);
        assert_eq!(
            review_outcome(&[Approved, Approved]),
            ReviewOutcome::Approved
        );
        assert_eq!(
            review_outcome(&[Approved, Rejected]),
            ReviewOutcome::Rejected
        );
        assert_eq!(
            review_outcome(&[Pending, Rejected]),
            ReviewOutcome::Rejected
        );
    }

    #[test]
    fn review_policy_outcomes() {
        use uuid::Uuid;
        use Decision::*;
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let none = ReviewRequirement::default();
        // the default equals the old rule
        assert_eq!(
            review_outcome_with(&[(a, Approved), (b, Pending)], &none),
            ReviewOutcome::Pending
        );
        assert_eq!(
            review_outcome_with(&[(a, Approved), (b, Approved)], &none),
            ReviewOutcome::Approved
        );
        assert_eq!(review_outcome_with(&[], &none), ReviewOutcome::Pending);
        // two of three
        let two = ReviewRequirement {
            min_approvals: Some(2),
            ..Default::default()
        };
        let votes = [(a, Approved), (b, Approved), (c, Pending)];
        assert_eq!(review_outcome_with(&votes, &two), ReviewOutcome::Approved);
        assert_eq!(
            review_outcome_with(&[(a, Approved), (b, Pending), (c, Pending)], &two),
            ReviewOutcome::Pending
        );
        // a rejection still rejects
        assert_eq!(
            review_outcome_with(&[(a, Approved), (b, Approved), (c, Rejected)], &two),
            ReviewOutcome::Rejected
        );
        // a required person
        let need_c = ReviewRequirement {
            min_approvals: Some(1),
            required_users: vec![(c, "Carl".into())],
            ..Default::default()
        };
        assert_eq!(
            review_outcome_with(&[(a, Approved), (c, Pending)], &need_c),
            ReviewOutcome::Pending
        );
        assert_eq!(
            review_outcome_with(&[(a, Pending), (c, Approved)], &need_c),
            ReviewOutcome::Approved
        );
        // a role group
        let qa = ReviewRequirement {
            min_approvals: Some(1),
            role_groups: vec![RoleRequirement {
                label: "QA".into(),
                members: vec![b],
            }],
            ..Default::default()
        };
        assert_eq!(
            review_outcome_with(&[(a, Approved), (b, Pending)], &qa),
            ReviewOutcome::Pending
        );
        assert_eq!(
            review_outcome_with(&[(a, Approved), (b, Approved)], &qa),
            ReviewOutcome::Approved
        );
        // min larger than the reviewers is capped (submit refuses it earlier)
        let ten = ReviewRequirement {
            min_approvals: Some(10),
            ..Default::default()
        };
        assert_eq!(
            review_outcome_with(&[(a, Approved)], &ten),
            ReviewOutcome::Approved
        );
    }

    #[test]
    fn review_policy_combination_and_shortfalls() {
        use uuid::Uuid;
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(combine_min_approvals(&[]), None);
        assert_eq!(combine_min_approvals(&[Some(2), None]), None);
        assert_eq!(combine_min_approvals(&[Some(2), Some(3)]), Some(3));
        let req = ReviewRequirement {
            min_approvals: Some(3),
            required_users: vec![(b, "Bea".into())],
            role_groups: vec![RoleRequirement {
                label: "QA".into(),
                members: vec![b],
            }],
        };
        let s = policy_shortfalls(&[a], &req);
        assert_eq!(s.len(), 3, "{s:?}");
        assert!(s[0].contains("3 approvals") && s[0].contains("only 1 reviewer is"));
        assert!(s[1].contains("Bea"));
        assert!(s[2].contains("QA"));
        assert!(policy_shortfalls(&[a, b, Uuid::from_u128(3)], &req).is_empty());
        assert!(policy_shortfalls(&[], &ReviewRequirement::default()).is_empty());
        let rest = attainable(&req, &[a]);
        assert!(rest.required_users.is_empty() && rest.role_groups.is_empty());
        assert_eq!(attainable(&req, &[a, b]), req);
    }

    #[test]
    fn item_numbers() {
        let ctx = |t| NumberContext {
            project: "ROVER",
            item_type: t,
            year: 2026,
            month: 3,
        };
        let default = NumberScheme::default_scheme();
        assert_eq!(default.format(&ctx(ItemType::Part), 42), "PRT-000042");
        assert_eq!(default.format(&ctx(ItemType::Software), 1), "SW-000001");
        assert_eq!(default.counter_key(&ctx(ItemType::Part)), "num:PRT-{SEQ}");
        assert_eq!(format_change_number(7), "CHG-000007");
        assert_eq!(
            normalize_item_number(" exp-prop-0012 ").unwrap(),
            "EXP-PROP-0012"
        );
        assert!(normalize_item_number("").is_err());
        assert!(normalize_item_number("-ABC").is_err());
        assert!(normalize_item_number("A/B").is_err());
        assert!(normalize_item_number(&"A".repeat(65)).is_err());
    }

    #[test]
    fn numbering_schemes() {
        let ctx = NumberContext {
            project: "ROVER",
            item_type: ItemType::Assembly,
            year: 2026,
            month: 3,
        };
        let s = NumberScheme::parse(" {project}-{Type}-{YYYY}-{SEQ:4} ").unwrap();
        assert_eq!(s.as_string(), "{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}");
        assert_eq!(s.format(&ctx, 7), "ROVER-ASM-2026-0007");
        assert_eq!(s.format(&ctx, 123_456), "ROVER-ASM-2026-123456");
        assert_eq!(s.counter_key(&ctx), "num:ROVER-ASM-2026-{SEQ}");
        // The padding width does not change the counter.
        let wider = NumberScheme::parse("{PROJECT}-{TYPE}-{YYYY}-{SEQ:6}").unwrap();
        assert_eq!(wider.counter_key(&ctx), s.counter_key(&ctx));

        // Numbers the expansion can produce, at any width.
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-0012"), Some(12));
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-123456"), Some(123_456));
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2025-0012"), None);
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-12A"), None);
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-"), None);
        // digits only and at most 18 of them (found by cargo mutants: `+12`
        // and 19 digits would parse as i64)
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-+12"), None);
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-1000000000000000000"), None);
        assert_eq!(
            s.seq_of(&ctx, "ROVER-ASM-2026-100000000000000000"),
            Some(100_000_000_000_000_000),
            "18 digits"
        );

        let s = NumberScheme::parse("x{YY}{MM}.{SEQ}").unwrap();
        assert_eq!(s.format(&ctx, 12), "X2603.12");
        assert_eq!(s.around_seq(&ctx), ("X2603.".to_string(), String::new()));
        let leading = NumberScheme::parse("{SEQ:3}-{PROJECT}").unwrap();
        assert_eq!(leading.seq_of(&ctx, "042-ROVER"), Some(42));
        assert_eq!(leading.seq_of(&ctx, "042-ROVERX"), None);
        assert_eq!(s.as_string(), "X{YY}{MM}.{SEQ}");
        assert_eq!(
            NumberScheme::of_project(Some("  ")).unwrap(),
            NumberScheme::default_scheme()
        );
        assert_eq!(
            NumberScheme::of_project(None).unwrap().as_string(),
            DEFAULT_NUMBER_SCHEME
        );

        for bad in [
            "",
            "PRT-0001",                             // no sequence
            "{SEQ}-{SEQ:3}",                        // two sequences
            "{SEQ:0}",                              // width out of range
            "{SEQ:13}",                             // width out of range
            "{SEQ:x}",                              // width not a number
            "{DATE}-{SEQ}",                         // unknown placeholder
            "{TYPE-{SEQ}",                          // unclosed brace
            "{TYPE} {SEQ}",                         // space
            "A/{SEQ}",                              // slash
            "-{SEQ}",                               // starts with '-'
            "{PROJECT}-{PROJECT}-{SEQ:4}",          // up to 69 characters
            &format!("{}{{SEQ}}", "A".repeat(100)), // too long
        ] {
            assert!(
                NumberScheme::parse(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        // the reason is named (found by cargo mutants)
        let msg = |raw: &str| NumberScheme::parse(raw).unwrap_err().public_message();
        assert!(msg("PRT-{YYYY}").contains("needs a sequence"));
        assert!(msg("{SEQ}-{SEQ:3}").contains("only once"));
        // generated numbers of exactly 64 characters are fine, 65 are not
        // (project code 32 + text + sequence 12)
        let numbers = |text: usize| format!("{{PROJECT}}{}{{SEQ:12}}", "A".repeat(text));
        assert!(NumberScheme::parse(&numbers(20)).is_ok(), "64 characters");
        assert!(msg(&numbers(21)).contains("at most 64"));
        // the scheme text itself has at most 100 characters; `{YY}` makes a
        // long text with short numbers (found by cargo mutants)
        let scheme = |extra: usize| format!("{}{{SEQ:1}}A{}", "{YY}".repeat(23), "B".repeat(extra));
        assert_eq!(scheme(0).len(), 100);
        assert!(
            NumberScheme::parse(&scheme(0)).is_ok(),
            "exactly 100 characters"
        );
        for extra in [1, 2] {
            let err = NumberScheme::parse(&scheme(extra))
                .unwrap_err()
                .public_message();
            assert!(err.contains("at most 100"), "{err}");
        }
    }

    #[test]
    fn usernames_and_codes() {
        assert_eq!(
            normalize_username(" Max.Mustermann ").unwrap(),
            "max.mustermann"
        );
        assert!(normalize_username("a").is_err());
        assert!(normalize_username("with space").is_err());
        assert_eq!(normalize_project_code("rover").unwrap(), "ROVER");
        assert!(normalize_project_code("NI XUS").is_err());
    }

    #[test]
    fn decimals_accept_comma() {
        assert_eq!(parse_decimal("Mass", Some("1,5")).unwrap(), Some(1.5));
        assert_eq!(parse_decimal("Mass", Some(" 2.25 ")).unwrap(), Some(2.25));
        assert_eq!(parse_decimal("Mass", Some("")).unwrap(), None);
        assert_eq!(parse_decimal("Mass", None).unwrap(), None);
        assert!(parse_decimal("Mass", Some("abc")).is_err());
        assert!(validate_non_negative("Mass", Some(-1.0)).is_err());
        assert!(validate_non_negative("Mass", Some(f64::NAN)).is_err());
        assert!(validate_quantity(0.0).is_err());
        assert_eq!(validate_quantity(2.0).unwrap(), 2.0);
    }

    #[test]
    fn simulation_items_and_basis_roles() {
        let ctx = NumberContext {
            project: "ROVER",
            item_type: ItemType::Simulation,
            year: 2026,
            month: 3,
        };
        assert_eq!(NumberScheme::default_scheme().format(&ctx, 3), "SIM-000003");
        assert_eq!(ItemType::parse(" Simulation "), Some(ItemType::Simulation));
        for t in ItemType::ALL {
            assert_eq!(ItemType::parse(t.as_str()), Some(t));
        }
        // stored value, label and number prefix of each type (found by cargo
        // mutants)
        let types: Vec<(&str, &str, &str)> = ItemType::ALL
            .iter()
            .map(|t| (t.as_str(), t.label(), t.number_prefix()))
            .collect();
        assert_eq!(
            types,
            [
                ("part", "Part", "PRT"),
                ("assembly", "Assembly", "ASM"),
                ("document", "Document", "DOC"),
                ("software", "Software", "SW"),
                ("simulation", "Simulation", "SIM"),
                ("as_built", "As-built", "AB"),
            ]
        );
        assert_eq!(ItemType::parse("As-Built"), Some(ItemType::AsBuilt));
        assert_eq!(ItemType::parse("as built"), Some(ItemType::AsBuilt));
        assert_eq!(ItemType::parse("gadget"), None);
        for r in BasisRole::ALL {
            assert_eq!(BasisRole::parse(r.as_str()), Some(r));
        }
        assert_eq!(BasisRole::parse("Load-Case"), Some(BasisRole::LoadCase));
        assert_eq!(BasisRole::parse("load case"), Some(BasisRole::LoadCase));
        assert_eq!(BasisRole::parse("sketch"), None);
    }

    #[test]
    fn result_filter_names_round_trip() {
        for f in ResultFilter::ALL {
            assert_eq!(ResultFilter::parse(f.as_str()), Some(f));
            assert!(!f.label().is_empty());
        }
        assert_eq!(ResultFilter::parse(" failed "), Some(ResultFilter::Failed));
        assert_eq!(ResultFilter::parse("fail"), None);
        assert_eq!(ResultFilter::parse_field(None).unwrap(), None);
        assert_eq!(ResultFilter::parse_field(Some("  ")).unwrap(), None);
        assert!(ResultFilter::parse_field(Some("nope")).is_err());
    }

    #[test]
    fn result_pass_fail() {
        use ResultStatus::{Fail, Pass};
        assert_eq!(result_status(5.0, None, None), ResultStatus::NoLimits);
        assert_eq!(result_status(5.0, Some(1.0), Some(10.0)), Pass);
        assert_eq!(result_status(1.0, Some(1.0), Some(10.0)), Pass, "inclusive");
        assert_eq!(
            result_status(10.0, Some(1.0), Some(10.0)),
            Pass,
            "inclusive"
        );
        assert_eq!(result_status(0.5, Some(1.0), Some(10.0)), Fail);
        assert_eq!(result_status(11.0, Some(1.0), Some(10.0)), Fail);
        // only an upper limit (max. stress), only a lower one (safety factor)
        assert_eq!(result_status(180e6, None, Some(200e6)), Pass);
        assert_eq!(result_status(210e6, None, Some(200e6)), Fail);
        assert_eq!(result_status(1.4, Some(1.5), None), Fail);
        assert_eq!(result_status(-3.0, Some(-5.0), None), Pass);
        assert_eq!(result_status(f64::NAN, Some(0.0), None), Fail);
        assert_eq!(Pass.as_str(), "pass");
        assert!(validate_result_numbers("Stress", 1.0, Some(2.0), Some(1.0)).is_err());
        assert!(validate_result_numbers("Stress", f64::INFINITY, None, None).is_err());
        assert!(validate_result_numbers("Stress", 1.0, Some(f64::NAN), None).is_err());
        assert!(validate_result_numbers("Stress", 1.0, Some(1.0), Some(1.0)).is_ok());
    }

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(like_pattern("valve"), "%valve%");
        assert_eq!(like_pattern("50%_x\\"), "%50\\%\\_x\\\\%");
    }

    #[test]
    fn password_policy() {
        // generated at the boundary, not literal phrases: a password literal
        // reads as a hard-coded credential (CodeQL) and says less about the rule
        assert!(validate_password(&"x".repeat(MIN_PASSWORD_LEN - 1)).is_err());
        assert!(validate_password(&"x".repeat(MIN_PASSWORD_LEN)).is_ok());
        // at most 1024 bytes (found by cargo mutants)
        assert!(validate_password(&"x".repeat(1024)).is_ok());
        for n in [1025, 1026] {
            assert!(validate_password(&"x".repeat(n)).is_err(), "{n}");
        }
    }

    #[test]
    fn change_request_transitions() {
        use RequestStatus::*;
        // the regular path: raised → triage → accepted → implemented
        assert!(New.can_transition_to(InTriage));
        assert!(InTriage.can_transition_to(Accepted));
        assert!(Accepted.can_transition_to(Implemented));
        // decisions straight from new, and out of deferred
        assert!(New.can_transition_to(Accepted));
        assert!(New.can_transition_to(Rejected));
        assert!(InTriage.can_transition_to(Deferred));
        assert!(Deferred.can_transition_to(InTriage));
        assert!(Deferred.can_transition_to(Accepted));
        assert!(Deferred.can_transition_to(Rejected));
        // the requester withdraws only before a decision
        assert!(New.can_transition_to(Withdrawn));
        assert!(InTriage.can_transition_to(Withdrawn));
        assert!(!Accepted.can_transition_to(Withdrawn));
        assert!(!Deferred.can_transition_to(Withdrawn));
        // implemented only through a released change order
        assert!(!New.can_transition_to(Implemented));
        assert!(!InTriage.can_transition_to(Implemented));
        assert!(!Accepted.can_transition_to(Rejected));
        // final states
        for end in [Rejected, Implemented, Withdrawn] {
            assert!(!end.is_open());
            for to in RequestStatus::ALL {
                assert!(!end.can_transition_to(to), "{end:?} → {to:?}");
            }
        }
        for s in RequestStatus::ALL {
            assert!(!s.can_transition_to(s), "no self transition for {s:?}");
            assert_eq!(RequestStatus::parse(s.as_str()), Some(s));
        }
        assert!(New.is_open() && Accepted.is_open() && Deferred.is_open());
        assert_eq!(RequestStatus::parse(" IN_TRIAGE "), Some(InTriage));
        assert_eq!(RequestStatus::parse("open"), None);
    }

    #[test]
    fn change_request_reasons_and_decisions() {
        for r in RequestReason::ALL {
            assert_eq!(RequestReason::parse(r.as_str()), Some(r));
        }
        assert_eq!(RequestReason::parse(""), Some(RequestReason::Other));
        assert_eq!(RequestReason::parse("bogus"), None);
        assert_eq!(
            RequestDecision::parse("accept").map(RequestDecision::status),
            Some(RequestStatus::Accepted)
        );
        assert_eq!(
            RequestDecision::parse("Defer").map(RequestDecision::status),
            Some(RequestStatus::Deferred)
        );
        assert_eq!(RequestDecision::parse("approve"), None);
        // the desktop app has its own copy of the reasons
        let app = include_str!("../../desktop/ui/js/core.js");
        for r in RequestReason::ALL {
            assert!(
                app.contains(&format!(
                    "{{ value: \"{}\", label: \"{}\" }}",
                    r.as_str(),
                    r.label()
                )),
                "desktop/ui/js/core.js lists the reason {}",
                r.as_str()
            );
        }
        assert_eq!(format_request_number(7), "ECR-000007");
        assert_eq!(
            NotificationKind::parse("change_request"),
            Some(NotificationKind::ChangeRequest)
        );
    }

    #[test]
    fn folders_are_normalised() {
        let ok = |raw: &str| normalize_folder(raw).unwrap();
        assert_eq!(ok(""), "");
        assert_eq!(ok("  "), "");
        assert_eq!(ok("/"), "");
        assert_eq!(ok("chassis"), "chassis");
        assert_eq!(ok("chassis/brackets"), "chassis/brackets");
        assert_eq!(ok("chassis\\brackets\\"), "chassis/brackets");
        assert_eq!(ok("/chassis//./brackets/"), "chassis/brackets");
        assert_eq!(ok("Rover 2 (old)/Parts"), "Rover 2 (old)/Parts");
        assert_eq!(ok("a..b/v1.2"), "a..b/v1.2", "dots inside names are fine");
        assert_eq!(ok("Ölwanne/Gehäuse"), "Ölwanne/Gehäuse");
        assert_eq!(ok("CONSOLE/COM10"), "CONSOLE/COM10");
        for bad in [
            "..",
            "chassis/../etc",
            "../outside",
            "a\\..\\b",
            "C:/CAD",
            "a|b",
            "what?",
            "star*",
            "quote\"",
            "<x>",
            "tab\there",
            "trailing./x",
            "space /x",
            "CON",
            "parts/nul.txt",
            "Com1",
            "LPT9/x",
        ] {
            assert!(normalize_folder(bad).is_err(), "{bad:?} must be refused");
        }
        let deep = vec!["a"; MAX_FOLDER_DEPTH + 1].join("/");
        assert!(normalize_folder(&deep).is_err());
        assert!(normalize_folder(&vec!["a"; MAX_FOLDER_DEPTH].join("/")).is_ok());
        let long = vec!["x".repeat(100); 5].join("/");
        assert!(normalize_folder(&long).is_err(), "longer than 400");
        assert!(normalize_folder(&"y".repeat(MAX_FOLDER_SEGMENT + 1)).is_err());
        // both length limits are inclusive (found by cargo mutants)
        assert!(normalize_folder(&"y".repeat(MAX_FOLDER_SEGMENT)).is_ok());
        let path = |len: usize| {
            // folders of 99 characters plus '/', the last one non-empty
            let mut parts = vec!["x".repeat(99); (len - 1) / 100];
            parts.push("z".repeat(len - parts.len() * 100));
            parts.join("/")
        };
        assert_eq!(path(MAX_FOLDER_LEN).chars().count(), MAX_FOLDER_LEN);
        assert_eq!(
            normalize_folder(&path(MAX_FOLDER_LEN))
                .unwrap()
                .chars()
                .count(),
            MAX_FOLDER_LEN
        );
        assert!(normalize_folder(&path(MAX_FOLDER_LEN + 1)).is_err());
        // reserved names need a digit after COM/LPT, and only these four
        for ok in ["COMX", "LPTA", "CONSOLE", "nullable"] {
            assert!(normalize_folder(ok).is_ok(), "{ok}");
        }
    }
}

/// Property tests for the pure rules: they must hold for every input, not
/// only for the examples of `tests`.
#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// Largest sequence whose code `revision_seq` still reads back (six
    /// letters: 20 + 20² + … + 20⁶).
    const MAX_READABLE_SEQ: i32 = 67_368_420;

    fn lifecycle_states() -> impl Strategy<Value = LifecycleState> {
        prop_oneof![
            Just(LifecycleState::InWork),
            Just(LifecycleState::InReview),
            Just(LifecycleState::Released),
            Just(LifecycleState::Obsolete),
        ]
    }

    fn item_types() -> impl Strategy<Value = ItemType> {
        prop::sample::select(ItemType::ALL.to_vec())
    }

    /// A valid scheme: literal text and placeholders with exactly one `{SEQ}`.
    fn schemes() -> impl Strategy<Value = String> {
        let token = prop_oneof![
            "[A-Za-z0-9][A-Za-z0-9._-]{0,5}",
            Just("{PROJECT}".to_string()),
            Just("{type}".to_string()),
            Just("{YYYY}".to_string()),
            Just("{YY}".to_string()),
            Just("{MM}".to_string()),
            Just("-".to_string()),
        ];
        (
            prop::collection::vec(token.clone(), 0..3),
            1usize..=9,
            prop::collection::vec(token, 0..3),
        )
            .prop_map(|(before, width, after)| {
                format!("{}{{SEQ:{width}}}{}", before.concat(), after.concat())
            })
    }

    proptest! {
        #[test]
        fn revision_codes_round_trip(seq in 1..=MAX_READABLE_SEQ) {
            let code = revision_code(seq);
            prop_assert!(code.len() <= 6);
            prop_assert!(code.bytes().all(|b| REVISION_ALPHABET.contains(&b)));
            prop_assert_eq!(revision_seq(&code), Some(seq));
        }

        #[test]
        fn revision_codes_sort_like_their_sequence(a in 1..=MAX_READABLE_SEQ, b in 1..=MAX_READABLE_SEQ) {
            let (ca, cb) = (revision_code(a), revision_code(b));
            // shorter codes come first, equal lengths sort alphabetically
            prop_assert_eq!(a.cmp(&b), (ca.len(), &ca).cmp(&(cb.len(), &cb)));
        }

        #[test]
        fn revision_seq_never_panics_and_only_accepts_canonical_codes(code in "\\PC{0,12}") {
            if let Some(seq) = revision_seq(&code) {
                prop_assert!(seq >= 1);
                prop_assert_eq!(revision_code(seq), code.trim().to_ascii_uppercase());
            }
        }

        #[test]
        fn number_schemes_never_panic(raw in "\\PC{0,120}") {
            let _ = NumberScheme::parse(&raw);
        }

        #[test]
        fn number_schemes_with_braces_never_panic(raw in "[A-Za-z0-9{}:._ -]{0,120}") {
            let _ = NumberScheme::parse(&raw);
        }

        #[test]
        fn valid_schemes_have_one_canonical_spelling(raw in schemes()) {
            // a generated scheme can still be too long; only valid ones count
            if let Ok(scheme) = NumberScheme::parse(&raw) {
                let canonical = scheme.as_string();
                let again = NumberScheme::parse(&canonical).expect("the canonical spelling parses");
                prop_assert_eq!(&again, &scheme);
                prop_assert_eq!(again.as_string(), canonical);
            }
        }

        #[test]
        fn generated_numbers_are_read_back_and_fit(
            raw in schemes(),
            ty in item_types(),
            project in "[A-Z][A-Z0-9]{0,31}",
            year in 1990i32..2200,
            month in 1u32..=12,
            seq in 0i64..1_000_000_000,
        ) {
            if let Ok(scheme) = NumberScheme::parse(&raw) {
                let ctx = NumberContext { project: &project, item_type: ty, year, month };
                let number = scheme.format(&ctx, seq);
                prop_assert_eq!(scheme.seq_of(&ctx, &number), Some(seq), "{} -> {}", raw, number);
                // the counter does not depend on the sequence or its width
                prop_assert!(scheme.counter_key(&ctx).starts_with("num:"));
                // the item-number alphabet and length rule of the items table
                prop_assert!(number.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')), "{number}");
                prop_assert!(number.starts_with(|c: char| c.is_ascii_alphanumeric()));
                let width = scheme.as_string().split("{SEQ").nth(1).map_or(1, |s| {
                    s.trim_start_matches(':').split('}').next().and_then(|n| n.parse().ok()).unwrap_or(1)
                });
                if seq < 10i64.pow(width as u32) {
                    prop_assert!(number.len() <= 64, "{} is {} characters", number, number.len());
                }
            }
        }

        #[test]
        fn numbers_of_other_prefixes_are_not_read(
            raw in schemes(),
            project in "[A-Z][A-Z0-9]{0,31}",
            other in "[a-z]{1,4}",
        ) {
            if let Ok(scheme) = NumberScheme::parse(&raw) {
                let ctx = NumberContext { project: &project, item_type: ItemType::Part, year: 2026, month: 11 };
                let number = format!("{other}{}", scheme.format(&ctx, 7));
                // lower case never occurs in generated numbers
                prop_assert_eq!(scheme.seq_of(&ctx, &number), None);
            }
        }

        #[test]
        fn lifecycle_states_only_move_forward_or_back_to_work(a in lifecycle_states(), b in lifecycle_states()) {
            if a.can_transition_to(b) {
                prop_assert!(a != b);
                // nothing leaves Obsolete, nothing re-enters Released from outside review
                prop_assert!(a != LifecycleState::Obsolete);
                if b == LifecycleState::Released {
                    prop_assert_eq!(a, LifecycleState::InReview);
                }
                // editable states are open, and only editable states can be left to review
                if a.is_editable() {
                    prop_assert!(a.is_open());
                }
            }
        }

        #[test]
        fn lifecycle_has_no_path_back_from_released(start in lifecycle_states(), steps in prop::collection::vec(lifecycle_states(), 0..12)) {
            let mut at = start;
            let mut released = start == LifecycleState::Released;
            for next in steps {
                if at.can_transition_to(next) {
                    at = next;
                    released |= at == LifecycleState::Released;
                }
                if released {
                    prop_assert!(matches!(at, LifecycleState::Released | LifecycleState::Obsolete));
                }
            }
        }
    }
}
