//! Unit tests of the OIDC claim mapping and id_token validation. They live in
//! their own file so the CodeQL configuration can leave test-only code out of
//! the analysis (`.github/codeql/codeql-config.yml`).

use super::*;
use serde_json::json;

fn config(viewer_group: Option<&str>) -> OidcConfig {
    OidcConfig {
        issuer_url: "https://auth.example.org/application/o/teamcraft/".into(),
        client_id: "teamcraft".into(),
        client_secret: "secret".into(),
        scopes: "openid profile email".into(),
        display_name: "SSO".into(),
        groups_claim: "groups".into(),
        username_claim: "preferred_username".into(),
        admin_group: "teamcraft-admins".into(),
        editor_group: "teamcraft-editors".into(),
        viewer_group: viewer_group.map(str::to_string),
    }
}

fn groups(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn group_mapping() {
    let open = config(None);
    assert_eq!(
        role_for_groups(&groups(&["teamcraft-admins", "x"]), &open),
        Some(Role::Admin)
    );
    assert_eq!(
        role_for_groups(&groups(&["TeamCraft-Editors"]), &open),
        Some(Role::Editor)
    );
    assert_eq!(
        role_for_groups(&groups(&["other"]), &open),
        Some(Role::Viewer)
    );
    assert_eq!(role_for_groups(&[], &open), Some(Role::Viewer));

    let restricted = config(Some("teamcraft-users"));
    assert_eq!(role_for_groups(&groups(&["other"]), &restricted), None);
    assert_eq!(
        role_for_groups(&groups(&["teamcraft-users"]), &restricted),
        Some(Role::Viewer)
    );
    assert_eq!(
        role_for_groups(&groups(&["teamcraft-editors"]), &restricted),
        Some(Role::Editor)
    );
}

#[test]
fn jwt_payload_decoding() {
    let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"abc","iss":"x"}"#);
    let jwt = format!("eyJhbGciOiJSUzI1NiJ9.{payload}.signature");
    let claims = decode_jwt_claims(&jwt).unwrap();
    assert_eq!(claims["sub"], "abc");
    assert!(decode_jwt_claims("no-dots").is_err());
}

#[test]
fn id_token_validation() {
    let issuer = "https://auth.example.org/application/o/teamcraft/";
    let good = json!({
        "iss": issuer, "aud": "teamcraft", "exp": 1_000, "nonce": "n1", "sub": "user-1"
    });
    assert!(validate_id_token_claims(&good, issuer, "teamcraft", "n1", 900).is_ok());
    // issuer without trailing slash is equivalent
    assert!(
        validate_id_token_claims(&good, issuer.trim_end_matches('/'), "teamcraft", "n1", 900)
            .is_ok()
    );
    assert!(
        validate_id_token_claims(&good, "https://evil.example/", "teamcraft", "n1", 900).is_err()
    );
    assert!(validate_id_token_claims(&good, issuer, "other-client", "n1", 900).is_err());
    assert!(validate_id_token_claims(&good, issuer, "teamcraft", "wrong", 900).is_err());
    assert!(validate_id_token_claims(&good, issuer, "teamcraft", "n1", 2_000).is_err());
    let multi_aud = json!({
        "iss": issuer, "aud": ["a", "teamcraft"], "exp": 1_000, "nonce": "n1", "sub": "user-1"
    });
    assert!(validate_id_token_claims(&multi_aud, issuer, "teamcraft", "n1", 900).is_ok());
}

#[test]
fn identity_extraction() {
    let cfg = config(None);
    let claims = json!({
        "sub": "1234",
        "preferred_username": "max.m",
        "name": "Max Mustermann",
        "email": "max@example.org",
        "groups": ["teamcraft-editors", "teamcraft-rocket"]
    });
    let id = identity_from_claims(&claims, &cfg).unwrap();
    assert_eq!(id.subject, "1234");
    assert_eq!(id.username_hint, "max.m");
    assert_eq!(id.display_name, "Max Mustermann");
    assert_eq!(id.email.as_deref(), Some("max@example.org"));
    assert_eq!(id.role, Role::Editor);
    assert_eq!(id.groups, vec!["teamcraft-editors", "teamcraft-rocket"]);

    let restricted = config(Some("teamcraft-users"));
    let outsider = json!({ "sub": "1", "groups": "other" });
    assert!(matches!(
        identity_from_claims(&outsider, &restricted),
        Err(AppError::Forbidden(_))
    ));
}
