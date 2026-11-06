//! Unit tests of the pure rules (no database).

use super::*;

fn actor(role: Role, projects: Vec<Uuid>) -> Actor {
    Actor {
        id: Uuid::new_v4(),
        username: "u".into(),
        display_name: "U".into(),
        role,
        skin: String::new(),
        projects,
        hidden_projects: Vec::new(),
    }
}

fn request(status: &str, requested_by: Uuid) -> ChangeRequest {
    let now = chrono::Utc::now();
    ChangeRequest {
        id: Uuid::nil(),
        number: "ECR-000001".into(),
        title: "t".into(),
        description: String::new(),
        reason: "defect".into(),
        priority: "normal".into(),
        status: status.into(),
        requested_by,
        requested_by_name: "R".into(),
        assignee: None,
        assignee_name: None,
        assignee_username: None,
        decision_comment: String::new(),
        decided_by: None,
        decided_by_name: None,
        decided_at: None,
        due_date: None,
        created_at: now,
        updated_at: now,
        closed_at: None,
        item_count: 0,
    }
}

#[test]
fn who_may_triage() {
    let rocket = Uuid::new_v4();
    let rover = Uuid::new_v4();
    let viewer = actor(Role::Viewer, vec![rocket]);
    let member = actor(Role::Editor, vec![rocket]);
    let outsider = actor(Role::Editor, vec![rover]);
    let admin = actor(Role::Admin, vec![]);
    assert!(!may_triage(&viewer, &[Some(rocket)]));
    assert!(may_triage(&member, &[Some(rocket)]));
    assert!(may_triage(&member, &[Some(rover), Some(rocket)]));
    assert!(!may_triage(&outsider, &[Some(rocket)]));
    assert!(may_triage(&outsider, &[]), "no items yet: any CAD editor");
    assert!(may_triage(&admin, &[Some(rocket)]));
}

#[test]
fn abilities_follow_status_and_role() {
    let rocket = Uuid::new_v4();
    let viewer = actor(Role::Viewer, vec![]);
    let editor = actor(Role::Editor, vec![rocket]);
    let projects = [Some(rocket)];
    let mine = request("new", viewer.id);
    let a = abilities(&viewer, &mine, &projects);
    assert!(a.edit && a.withdraw && a.comment && a.attach);
    assert!(!a.decide && !a.start_triage && !a.implement && !a.assign);
    let e = abilities(&editor, &mine, &projects);
    assert!(e.start_triage && e.decide && e.assign && !e.withdraw && !e.implement);

    let accepted = request("accepted", viewer.id);
    let a = abilities(&viewer, &accepted, &projects);
    assert!(!a.edit && !a.withdraw && a.comment);
    let e = abilities(&editor, &accepted, &projects);
    assert!(e.implement && !e.decide && e.edit);

    let deferred = request("deferred", viewer.id);
    let e = abilities(&editor, &deferred, &projects);
    assert!(e.reopen && e.decide && !e.start_triage);

    let done = request("implemented", viewer.id);
    let e = abilities(&editor, &done, &projects);
    assert!(!e.edit && !e.decide && !e.assign && !e.attach && e.comment);
}

#[test]
fn attachments_are_sniffed() {
    assert_eq!(
        sniff_attachment(b"\x89PNG\r\n\x1a\n...."),
        Some("image/png")
    );
    assert_eq!(
        sniff_attachment(&[0xFF, 0xD8, 0xFF, 0xE0]),
        Some("image/jpeg")
    );
    assert_eq!(sniff_attachment(b"GIF89a.."), Some("image/gif"));
    assert_eq!(
        sniff_attachment(b"RIFF\0\0\0\0WEBPVP8 "),
        Some("image/webp")
    );
    assert_eq!(sniff_attachment(b"%PDF-1.7\n"), Some("application/pdf"));
    assert_eq!(
        sniff_attachment(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"),
        None
    );
    assert_eq!(sniff_attachment(b"<html>"), None);
    assert_eq!(sniff_attachment(b""), None);
}

#[test]
fn attachment_names_are_cleaned() {
    assert_eq!(clean_filename("C:\\shots\\crack.png"), "crack.png");
    assert_eq!(clean_filename("../../etc/passwd"), "passwd");
    assert_eq!(clean_filename("a\u{0}b\r\n<x>.pdf"), "abx.pdf");
    assert_eq!(clean_filename(""), "attachment");
    assert_eq!(clean_filename(".."), "attachment");
}

#[test]
fn due_dates() {
    assert_eq!(parse_due_date(None).unwrap(), None);
    assert_eq!(parse_due_date(Some(" ")).unwrap(), None);
    assert_eq!(
        parse_due_date(Some("2026-12-01")).unwrap(),
        NaiveDate::from_ymd_opt(2026, 12, 1)
    );
    assert!(parse_due_date(Some("01.12.2026")).is_err());
}
