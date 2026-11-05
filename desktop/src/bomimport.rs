//! *Import BOM…*: a BOM table (CSV/Excel) or a STEP assembly exported by the
//! CAD program goes to the server's BOM import – below a checked-out
//! assembly revision (`POST /items/{number}/revisions/{code}/bom/import`) or
//! as a new assembly (`POST /bom-imports`). The first call sends
//! `dry_run=true` and returns the plan the dialog shows (counts, notes, the
//! first lines); the second call imports in one transaction on the server.
//! The column recognition, numbering and matching stay on the server.

use std::path::Path;

use serde::Deserialize;

use crate::api::{Api, ApiError};
use crate::workspace;

/// File types the server reads as a BOM (`bomtable::supported`, `stepbom::is_step`).
pub const EXTENSIONS: &[&str] = &[
    "csv", "tsv", "txt", "xlsx", "xlsm", "xlsb", "xls", "ods", "step", "stp", "p21",
];

/// The server's limits (`services::imports::MAX_TABLE_BYTES` / `MAX_STEP_BYTES`),
/// checked before the file is read into memory.
const MAX_TABLE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_STEP_BYTES: u64 = 200 * 1024 * 1024;

/// What the dialog asks for.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Request {
    /// Item number of the checked-out assembly to import into; `None` = a new assembly.
    pub item: Option<String>,
    /// Its revision code.
    pub code: Option<String>,
    /// Project group of the new items (required for a new assembly).
    pub project: Option<String>,
    /// Name of a new assembly (default: the assembly row or the file name).
    pub name: Option<String>,
    /// `file` (part numbers of the file) or `generate`.
    pub numbers: Option<String>,
    /// Use existing items with the same number instead of creating new ones.
    pub match_existing: Option<bool>,
}

fn is_step(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "step" | "stp" | "p21"))
}

/// Checks the file before it is sent: a known type, not larger than the
/// server takes. Returns the file name sent to the server.
pub fn check_file(path: &Path, size: u64) -> Result<String, ApiError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ApiError::local(format!("{} has no usable file name.", path.display())))?
        .to_string();
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !EXTENSIONS.contains(&ext.as_str()) {
        return Err(ApiError::local(format!(
            "{name}: BOMs are read from CSV, TSV, TXT, XLSX, XLSM, XLSB, XLS and ODS tables \
             and from STEP assemblies (STEP, STP, P21)."
        )));
    }
    let (limit, what) = if is_step(&name) {
        (MAX_STEP_BYTES, "STEP assemblies")
    } else {
        (MAX_TABLE_BYTES, "BOM tables")
    };
    if size > limit {
        return Err(ApiError::local(format!(
            "{name} is larger than {} MiB, the limit for {what}.",
            limit / (1024 * 1024)
        )));
    }
    Ok(name)
}

/// Text fields of a multipart request.
pub type Fields = Vec<(&'static str, String)>;

/// The API path and the text fields of the multipart request.
pub fn plan_request(req: &Request, dry_run: bool) -> Result<(String, Fields), ApiError> {
    let trimmed = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let numbers = trimmed(&req.numbers).unwrap_or_else(|| "file".into());
    if numbers != "file" && numbers != "generate" {
        return Err(ApiError::local(format!(
            "Unknown number source '{numbers}': use 'file' or 'generate'."
        )));
    }
    let mut fields = vec![
        ("numbers", numbers),
        (
            "match_existing",
            req.match_existing.unwrap_or(true).to_string(),
        ),
        ("dry_run", dry_run.to_string()),
    ];
    let path = match (trimmed(&req.item), trimmed(&req.code)) {
        (Some(item), Some(code)) => format!(
            "/items/{}/revisions/{}/bom/import",
            workspace::enc(&item),
            workspace::enc(&code)
        ),
        (None, None) => {
            let project = trimmed(&req.project)
                .ok_or_else(|| ApiError::local("Choose the project group of the new assembly."))?;
            fields.push(("project", project));
            if let Some(name) = trimmed(&req.name) {
                fields.push(("name", name));
            }
            "/bom-imports".to_string()
        }
        _ => return Err(ApiError::local("Item number and revision belong together.")),
    };
    Ok((path, fields))
}

/// Sends the file: with `dry_run` the server answers with the plan, otherwise
/// it imports and answers with what it created.
pub async fn send(
    api: &Api,
    path: &Path,
    req: &Request,
    dry_run: bool,
) -> Result<serde_json::Value, ApiError> {
    let size = tokio::fs::metadata(path).await?.len();
    let name = check_file(path, size)?;
    let (endpoint, fields) = plan_request(req, dry_run)?;
    api.multipart(&endpoint, &[(name, path.to_path_buf())], &fields)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn field<'a>(fields: &'a [(&str, String)], name: &str) -> Option<&'a str> {
        fields
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn into_a_checked_out_revision() {
        let req = Request {
            item: Some("ASM-000007".into()),
            code: Some("B".into()),
            project: Some("ROVER".into()),
            ..Default::default()
        };
        let (path, fields) = plan_request(&req, true).unwrap();
        assert_eq!(path, "/items/ASM-000007/revisions/B/bom/import");
        assert_eq!(field(&fields, "dry_run"), Some("true"));
        assert_eq!(field(&fields, "numbers"), Some("file"));
        assert_eq!(field(&fields, "match_existing"), Some("true"));
        assert_eq!(field(&fields, "project"), None, "the target's group");
        let (_, fields) = plan_request(&req, false).unwrap();
        assert_eq!(field(&fields, "dry_run"), Some("false"));
    }

    #[test]
    fn as_a_new_assembly() {
        let req = Request {
            project: Some(" ROVER ".into()),
            name: Some("Chassis".into()),
            numbers: Some("generate".into()),
            match_existing: Some(false),
            ..Default::default()
        };
        let (path, fields) = plan_request(&req, true).unwrap();
        assert_eq!(path, "/bom-imports");
        assert_eq!(field(&fields, "project"), Some("ROVER"));
        assert_eq!(field(&fields, "name"), Some("Chassis"));
        assert_eq!(field(&fields, "numbers"), Some("generate"));
        assert_eq!(field(&fields, "match_existing"), Some("false"));
        // no name: the server takes the assembly row or the file name
        let req = Request {
            project: Some("ROVER".into()),
            name: Some("  ".into()),
            ..Default::default()
        };
        let (_, fields) = plan_request(&req, true).unwrap();
        assert_eq!(field(&fields, "name"), None);
    }

    #[test]
    fn refuses_incomplete_requests() {
        assert!(
            plan_request(&Request::default(), true).is_err(),
            "no project"
        );
        let half = Request {
            item: Some("ASM-000007".into()),
            ..Default::default()
        };
        assert!(plan_request(&half, true).is_err());
        let odd = Request {
            project: Some("ROVER".into()),
            numbers: Some("random".into()),
            ..Default::default()
        };
        assert!(plan_request(&odd, true).is_err());
        // path segments are encoded
        let req = Request {
            item: Some("A/B".into()),
            code: Some("A".into()),
            ..Default::default()
        };
        assert_eq!(
            plan_request(&req, true).unwrap().0,
            "/items/A%2FB/revisions/A/bom/import"
        );
    }

    #[test]
    fn checks_type_and_size() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(check_file(&p("/cad/bom.CSV"), 10).unwrap(), "bom.CSV");
        assert!(check_file(&p("/cad/Frame BOM.xlsx"), 10).is_ok());
        assert!(check_file(&p("/cad/frame.stp"), 150 * 1024 * 1024).is_ok());
        assert!(check_file(&p("/cad/frame.iam"), 10).is_err(), "native CAD");
        assert!(check_file(&p("/cad/noext"), 10).is_err());
        assert!(check_file(&p("/cad/bom.xlsx"), 21 * 1024 * 1024).is_err());
        assert!(check_file(&p("/cad/frame.step"), 201 * 1024 * 1024).is_err());
    }

    /// One HTTP request against a fake server: path, the text fields and the
    /// file arrive as the server's BOM import expects them.
    #[tokio::test]
    async fn sends_the_file_and_the_fields() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            // read until the end of the multipart body
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            let reply = r#"{"new_items":2,"existing_items":1,"lines":[],"warnings":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            sock.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&raw).to_string()
        });

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chassis.csv");
        std::fs::write(&file, "Level,Part Number,Name,Quantity\n1,WHEEL,Wheel,4\n").unwrap();
        let api = Api {
            client: reqwest::Client::new(),
            base: format!("http://{addr}"),
            token: Some("fake-test-token".into()),
        };
        let req = Request {
            project: Some("ROVER".into()),
            ..Default::default()
        };
        let plan = send(&api, &file, &req, true).await.unwrap();
        assert_eq!(plan["new_items"], 2);

        let raw = server.await.unwrap();
        assert!(raw.starts_with("POST /api/v1/bom-imports "), "{raw}");
        assert!(raw
            .to_ascii_lowercase()
            .contains("authorization: bearer fake-test-token"));
        for expected in [
            "name=\"dry_run\"\r\n\r\ntrue",
            "name=\"project\"\r\n\r\nROVER",
            "name=\"numbers\"\r\n\r\nfile",
            "filename=\"chassis.csv\"",
            "1,WHEEL,Wheel,4",
        ] {
            assert!(raw.contains(expected), "missing {expected:?} in\n{raw}");
        }
    }
}
