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
fn a_fresh_sign_in_needs_a_recent_auth_time() {
    let now = 10_000;
    assert!(fresh_sign_in(&json!({ "auth_time": now - 30 }), now).is_ok());
    assert!(fresh_sign_in(&json!({ "auth_time": now - 300 }), now).is_ok());
    // too old, missing, not a number, far in the future
    assert!(fresh_sign_in(&json!({ "auth_time": now - 301 }), now).is_err());
    assert!(fresh_sign_in(&json!({}), now).is_err());
    assert!(fresh_sign_in(&json!({ "auth_time": "now" }), now).is_err());
    assert!(fresh_sign_in(&json!({ "auth_time": now + 3_600 }), now).is_err());
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

/// An ES256 key pair as the provider would hold it, and its public JWK.
fn es256_key(kid: &str) -> (ring::signature::EcdsaKeyPair, Jwk) {
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    let rng = ring::rand::SystemRandom::new();
    let alg = &ECDSA_P256_SHA256_FIXED_SIGNING;
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
    let pair = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
    let point = pair.public_key().as_ref().to_vec();
    let jwk = Jwk {
        kty: "EC".into(),
        kid: Some(kid.into()),
        alg: Some("ES256".into()),
        crv: Some("P-256".into()),
        x: Some(URL_SAFE_NO_PAD.encode(&point[1..33])),
        y: Some(URL_SAFE_NO_PAD.encode(&point[33..65])),
        ..Jwk::default()
    };
    (pair, jwk)
}

fn sign(pair: &ring::signature::EcdsaKeyPair, header: &Value, claims: &Value) -> String {
    let enc = |v: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
    let message = format!("{}.{}", enc(header), enc(claims));
    let rng = ring::rand::SystemRandom::new();
    let sig = pair.sign(&rng, message.as_bytes()).unwrap();
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
}

const ISS: &str = "https://auth.example.org/application/o/teamcraft/";

fn logout_claims(now: i64) -> Value {
    json!({
        "iss": ISS, "aud": "teamcraft", "iat": now, "jti": "j-1", "sub": "u-4711",
        "events": { BACKCHANNEL_LOGOUT_EVENT: {} },
    })
}

fn check(token: &str, keys: &[Jwk], now: i64) -> Result<String, LogoutTokenError> {
    verify_logout_token(token, keys, ISS, "teamcraft", now)
}

#[test]
fn a_signed_logout_token_names_its_subject() {
    let (pair, jwk) = es256_key("k1");
    let now = 1_800_000_000;
    let header = json!({ "alg": "ES256", "kid": "k1", "typ": "logout+jwt" });
    let token = sign(&pair, &header, &logout_claims(now));
    let keys = [jwk];
    assert_eq!(check(&token, &keys, now), Ok("u-4711".to_string()));

    // another key with the same id, an unknown key id
    let (other, mut other_jwk) = es256_key("k1");
    let foreign = sign(&other, &header, &logout_claims(now));
    assert_eq!(
        check(&foreign, &keys, now),
        Err(LogoutTokenError::BadSignature)
    );
    other_jwk.kid = Some("k2".into());
    assert_eq!(
        check(&token, &[other_jwk], now),
        Err(LogoutTokenError::UnknownKey)
    );
    // a changed payload under the old signature, `alg: none`, garbage
    let parts: Vec<&str> = token.split('.').collect();
    let mut claims = logout_claims(now);
    claims["sub"] = json!("admin");
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let forged = format!("{}.{payload}.{}", parts[0], parts[2]);
    assert_eq!(
        check(&forged, &keys, now),
        Err(LogoutTokenError::BadSignature)
    );
    let none = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"k1"}"#);
    let unsigned = format!("{none}.{}.", parts[1]);
    assert!(check(&unsigned, &keys, now).is_err());
    assert_eq!(check("a.b", &keys, now), Err(LogoutTokenError::Malformed));
}

#[test]
fn logout_token_claims_are_checked() {
    let (pair, jwk) = es256_key("k1");
    let now = 1_800_000_000;
    let header = json!({ "alg": "ES256", "kid": "k1" });
    let keys = [jwk];
    let refused = |change: &dyn Fn(&mut Value)| {
        let mut claims = logout_claims(now);
        change(&mut claims);
        let token = sign(&pair, &header, &claims);
        matches!(check(&token, &keys, now), Err(LogoutTokenError::Claim(_)))
    };
    assert!(refused(&|c| c["iss"] = json!("https://evil.example.org")));
    assert!(refused(&|c| c["aud"] = json!("another-app")));
    assert!(refused(&|c| c["iat"] = json!(now - 3600)));
    assert!(refused(&|c| c["iat"] = json!(now + 3600)));
    assert!(refused(&|c| c["exp"] = json!(now - 3600)));
    assert!(refused(&|c| c["events"] = json!({})));
    assert!(refused(&|c| c["nonce"] = json!("n")));
    assert!(refused(&|c| c["sub"] = json!("")));
    // a list audience is fine
    let mut claims = logout_claims(now);
    claims["aud"] = json!(["other", "teamcraft"]);
    let token = sign(&pair, &header, &claims);
    assert!(check(&token, &keys, now).is_ok());
}
