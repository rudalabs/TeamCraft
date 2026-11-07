//! Boundary values of the pure rules in `domain` and the integrity checks of
//! the file vault. No database needed.

use teamcraft::{
    domain::{
        self, normalize_item_number, normalize_project_code, normalize_username, revision_code,
        revision_seq, validate_password, LifecycleState,
    },
    error::AppError,
    vault::Vault,
};

#[test]
fn revision_letters_skip_the_confusable_letters_and_round_trip() {
    let letters = |n: i32| revision_code(n);
    assert_eq!(letters(1), "A");
    assert_eq!(letters(8), "H");
    assert_eq!(letters(9), "J", "I is skipped");
    assert_eq!(letters(20), "Y", "the last single letter");
    assert_eq!(letters(21), "AA");
    assert_eq!(letters(40), "AY");
    assert_eq!(letters(41), "BA");
    assert_eq!(letters(420), "YY");
    assert_eq!(letters(421), "AAA");
    for seq in 1..=9000 {
        let code = letters(seq);
        assert!(code.chars().all(|c| !"IOQSXZ".contains(c)), "{code}");
        assert_eq!(revision_seq(&code), Some(seq), "{code}");
    }
    // codes are strictly increasing in (length, text): every letter sequence
    // sorts after the previous one
    let mut previous = letters(1);
    for seq in 2..=2000 {
        let code = letters(seq);
        assert!((code.len(), &code) > (previous.len(), &previous), "{seq}");
        previous = code;
    }
}

#[test]
fn invalid_revision_codes_have_no_sequence() {
    for bad in [
        "", "I", "O", "Q", "S", "X", "Z", "A1", "A-", "ÄA", "ABCDEFG", "AAAAAAA",
    ] {
        assert_eq!(revision_seq(bad), None, "{bad:?}");
    }
    assert_eq!(revision_seq("a"), Some(1), "lower case is accepted");
    assert_eq!(revision_seq("yy"), Some(420));
    // the longest accepted code does not overflow
    let _ = revision_seq("YYYYYY");
}

#[test]
fn the_lifecycle_allows_exactly_the_documented_transitions() {
    use LifecycleState::*;
    let all = [InWork, InReview, Released, Obsolete];
    let allowed = [
        (InWork, InReview),
        (InReview, Released),
        (InReview, InWork),
        (Released, Obsolete),
    ];
    for from in all {
        for to in all {
            assert_eq!(
                from.can_transition_to(to),
                allowed.contains(&(from, to)),
                "{from:?} -> {to:?}"
            );
        }
        // the table shown in the UI says the same
        let ruled: Vec<_> = domain::rules_from(from).iter().map(|r| r.to).collect();
        let expected: Vec<_> = allowed
            .iter()
            .filter(|(f, _)| *f == from)
            .map(|(_, t)| *t)
            .collect();
        assert_eq!(ruled, expected, "{from:?}");
    }
    assert!(!Obsolete.is_open() && !Released.is_open());
    assert!(InWork.is_open() && InReview.is_open());
    assert!(InWork.is_editable() && !Released.is_editable() && !Obsolete.is_editable());
}

fn bad_request<T: std::fmt::Debug>(r: Result<T, AppError>) {
    assert!(matches!(r, Err(AppError::BadRequest(_))), "{r:?}");
}

#[test]
fn item_numbers_are_normalised_and_bounded() {
    assert_eq!(normalize_item_number("  abc-1.2_x ").unwrap(), "ABC-1.2_X");
    assert_eq!(normalize_item_number("0").unwrap(), "0");
    assert!(normalize_item_number(&"A".repeat(64)).is_ok());
    for bad in [
        "", "   ", "-A", ".A", "_A", "A B", "Ä1", "A/B", "A\\B", "A\nB", "A;B", "a\u{0}",
    ] {
        bad_request(normalize_item_number(bad));
    }
    bad_request(normalize_item_number(&"A".repeat(65)));
}

#[test]
fn usernames_and_project_codes_have_their_limits() {
    assert_eq!(
        normalize_username(" Max.Mustermann ").unwrap(),
        "max.mustermann"
    );
    assert!(normalize_username("ab").is_ok());
    assert!(normalize_username(&"a".repeat(64)).is_ok());
    for bad in ["", "a", "a b", "ä1", "x@y"] {
        bad_request(normalize_username(bad));
    }
    bad_request(normalize_username(&"a".repeat(65)));

    assert_eq!(normalize_project_code(" rover-2 ").unwrap(), "ROVER-2");
    assert!(normalize_project_code(&"P".repeat(32)).is_ok());
    for bad in ["", " ", "A.B", "A B", "Ä"] {
        bad_request(normalize_project_code(bad));
    }
    bad_request(normalize_project_code(&"P".repeat(33)));
}

#[test]
fn passwords_are_counted_in_characters_and_bounded_in_bytes() {
    bad_request(validate_password(&"x".repeat(domain::MIN_PASSWORD_LEN - 1)));
    assert!(validate_password(&"x".repeat(domain::MIN_PASSWORD_LEN)).is_ok());
    // ten two-byte characters are ten characters, not twenty
    assert!(validate_password(&"ä".repeat(10)).is_ok());
    bad_request(validate_password(&"ä".repeat(9)));
    assert!(validate_password(&"x".repeat(1024)).is_ok());
    bad_request(validate_password(&"x".repeat(1025)));
}

#[test]
fn required_text_is_trimmed_and_bounded() {
    assert_eq!(
        domain::require_text("Name", "  Wheel ", 10).unwrap(),
        "Wheel"
    );
    bad_request(domain::require_text("Name", "   ", 10));
    bad_request(domain::require_text("Name", &"x".repeat(11), 10));
    assert!(domain::require_text("Name", &"ä".repeat(10), 10).is_ok());
    // the mark counts towards the limit
    assert_eq!(domain::truncate_chars("äöü", 2), "ä…");
    assert_eq!(domain::truncate_chars("äöü", 3), "äöü");
    assert_eq!(domain::truncate_chars("abc", 10), "abc");
}

// ---------------------------------------------------------------------------
// Vault
// ---------------------------------------------------------------------------

async fn vault() -> (Vault, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("tc-vault-{}", uuid::Uuid::new_v4()));
    (Vault::open(&root).await.unwrap(), root)
}

#[tokio::test]
async fn a_blob_that_changed_on_disk_fails_verification() {
    let (vault, root) = vault().await;
    let stored = vault.store_bytes(b"original content").await.unwrap();
    assert!(vault.verify(&stored.sha256).await.unwrap());
    let path = vault.object_path(&stored.sha256).unwrap();
    std::fs::write(&path, b"original contenT").unwrap();
    assert!(
        !vault.verify(&stored.sha256).await.unwrap(),
        "bit rot is found"
    );
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        vault.verify(&stored.sha256).await,
        Err(AppError::NotFound(_))
    ));
    assert!(!vault.exists(&stored.sha256).await);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn hashes_cannot_escape_the_object_tree() {
    let (vault, root) = vault().await;
    for bad in [
        "",
        "../../etc/passwd",
        &"g".repeat(64),
        &"a".repeat(63),
        &"a".repeat(65),
        "..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f..%2f",
    ] {
        assert!(vault.object_path(bad).is_err(), "{bad:?}");
        assert!(!vault.exists(bad).await);
        assert!(vault.open_blob(bad).await.is_err());
    }
    // upper-case hashes address the same object as lower-case ones
    let stored = vault.store_bytes(b"x").await.unwrap();
    assert_eq!(
        vault
            .object_path(&stored.sha256.to_ascii_uppercase())
            .unwrap(),
        vault.object_path(&stored.sha256).unwrap()
    );
    let path = vault.object_path(&stored.sha256).unwrap();
    assert!(path.starts_with(vault.root()));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_upload_over_the_limit_is_refused_and_leaves_no_blob() {
    let (vault, root) = vault().await;
    let mut upload = vault.begin_upload(10).await.unwrap();
    upload.write(b"0123456789").await.unwrap();
    assert!(upload.write(b"!").await.is_err(), "one byte too many");
    drop(upload);
    let empty = vault.store_bytes(b"").await.unwrap();
    assert_eq!(empty.size, 0);
    assert!(vault.verify(&empty.sha256).await.unwrap(), "the empty file");
    let objects: Vec<_> = walk(&vault.root().join("objects"));
    assert_eq!(objects.len(), 1, "only the empty file: {objects:?}");
    let _ = std::fs::remove_dir_all(root);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
