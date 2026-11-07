//! The desktop app's version handshake (`desktop/src/compat.rs`, a file without
//! dependencies, compiled here because the desktop crate needs GUI libraries).

#[path = "../desktop/src/compat.rs"]
mod compat;

#[test]
fn parses_versions() {
    assert_eq!(compat::parse("0.2.0"), Some((0, 2)));
    assert_eq!(compat::parse("v1.4.2-rc.1"), Some((1, 4)));
    assert_eq!(compat::parse("1.10"), Some((1, 10)));
    assert_eq!(compat::parse("1.3-beta"), Some((1, 3)));
    assert_eq!(compat::parse(""), None);
    assert_eq!(compat::parse("abc"), None);
    assert_eq!(compat::parse("2"), None);
}

#[test]
fn before_1_0_major_and_minor_must_match() {
    assert!(compat::notice("0.2.0", "0.2.7").is_none());
    assert!(compat::notice("0.2.5", "0.2.0").is_none());
    let newer = compat::notice("0.2.0", "0.3.0").expect("notice");
    assert!(
        newer.contains("0.2.0") && newer.contains("0.3.0") && newer.contains("matching desktop")
    );
    let older = compat::notice("0.3.0", "0.2.0").expect("notice");
    assert!(older.contains("upgrade the server"));
}

#[test]
fn from_1_0_a_server_with_the_same_major_and_a_newer_minor_fits() {
    assert!(compat::notice("1.2.0", "1.2.0").is_none());
    assert!(compat::notice("1.2.0", "1.5.1").is_none());
    assert!(compat::notice("1.2.0", "1.1.9").is_some());
    assert!(compat::notice("1.2.0", "2.0.0").is_some());
    assert!(compat::notice("2.0.0", "1.9.0").is_some());
    assert!(compat::notice("1.0.0", "0.9.0").is_some());
}

#[test]
fn an_unreadable_server_version_is_not_blocked() {
    assert!(compat::notice("0.2.0", "").is_none());
    assert!(compat::notice("0.2.0", "unknown").is_none());
}

#[test]
fn local_agent_header_matches_the_server() {
    // a rename on one side would silently turn the server's hint off
    assert!(compat::LOCAL_AGENT_HEADER.eq_ignore_ascii_case(teamcraft::config::LOCAL_AGENT_HEADER));
    assert_eq!(
        compat::LOCAL_AGENT_HEADER_VALUE,
        teamcraft::config::LOCAL_AGENT_HEADER_VALUE
    );
    // a valid header name, as the HTTP stack of the agent needs it
    assert!(axum::http::HeaderName::from_bytes(compat::LOCAL_AGENT_HEADER.as_bytes()).is_ok());
}

#[test]
fn agent_backend_follows_the_policy_then_the_users_pick() {
    use compat::AgentBackend::{Company, Local, Off};
    // off + company AI: the company AI only, whatever was picked before
    for stored in [None, Some(Local), Some(Company)] {
        assert_eq!(
            compat::choose_backend(false, true, true, stored),
            (Company, vec![Company])
        );
    }
    // off + no company AI: no AI at all
    assert_eq!(
        compat::choose_backend(false, false, true, Some(Local)),
        (Off, vec![])
    );
    // both: the stored pick, else a set-up local agent, else the company AI
    assert_eq!(
        compat::choose_backend(true, true, true, Some(Company)),
        (Company, vec![Company, Local])
    );
    assert_eq!(
        compat::choose_backend(true, true, false, Some(Local)),
        (Local, vec![Company, Local])
    );
    assert_eq!(compat::choose_backend(true, true, true, None).0, Local);
    assert_eq!(compat::choose_backend(true, true, false, None).0, Company);
    // allowed, no company AI: the local agent (today's behaviour), even
    // with a stale pick of the company AI
    assert_eq!(
        compat::choose_backend(true, false, false, Some(Company)),
        (Local, vec![Local])
    );
}

#[test]
fn agent_backend_names_round_trip() {
    use compat::AgentBackend;
    for b in [AgentBackend::Company, AgentBackend::Local] {
        assert_eq!(AgentBackend::parse(b.as_str()), Some(b));
    }
    // "off" is a state, never a pick
    assert_eq!(AgentBackend::Off.as_str(), "off");
    assert_eq!(AgentBackend::parse("off"), None);
    assert_eq!(AgentBackend::parse("Company"), None);
    assert_eq!(AgentBackend::parse(""), None);
}
