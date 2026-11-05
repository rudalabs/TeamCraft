//! HTTP client for the TeamCraft REST API (`/api/v1`, bearer token).
//!
//! All network traffic of the desktop app goes through Rust: the web view
//! never talks to the server itself (no CORS, the token never reaches
//! JavaScript).

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const JSON_TIMEOUT: Duration = Duration::from_secs(60);
/// Files larger than this go to the server as chunked, resumable uploads
/// (`/api/v1/uploads`) instead of inside the multipart request.
pub const CHUNKED_ABOVE: u64 = 32 * 1024 * 1024;
/// Largest chunk the app sends (the server may allow less).
const MAX_CHUNK: u64 = 8 * 1024 * 1024;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(300);
/// Failed chunks in a row before an upload gives up.
const CHUNK_RETRIES: u32 = 6;

/// Progress report and cancel switch of an upload.
pub struct Transfer<'a> {
    pub cancel: &'a AtomicBool,
    /// `(file name, bytes sent, file size)`.
    pub progress: &'a (dyn Fn(&str, u64, u64) + Send + Sync),
}

impl Transfer<'_> {
    fn check_cancel(&self) -> Result<(), ApiError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(ApiError::local("Upload cancelled."))
        } else {
            Ok(())
        }
    }
}

/// Errors after which sending a chunk again may succeed: network trouble,
/// a server restart or overload, or an offset the server did not confirm.
fn retryable(e: &ApiError) -> bool {
    matches!(e.status, 0 | 408 | 409 | 429 | 500..=599)
}

/// Waits longer after each failure: 1, 2, 4, 8, then 16 s.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.clamp(1, 5).saturating_sub(1))
}

/// The chunk to send next: from `offset`, at most `chunk` bytes.
fn chunk_len(size: u64, offset: u64, chunk: u64) -> usize {
    usize::try_from(size.saturating_sub(offset).min(chunk)).unwrap_or(0)
}

/// The entry for `name` in a JSON-object field of a multipart request
/// (`bases` or `folders`: `{"file name": …}`, names matched like on the
/// server, case-insensitively).
fn field_entry(fields: &[(&str, String)], field: &str, name: &str) -> Option<Value> {
    let (_, raw) = fields.iter().find(|(f, _)| *f == field)?;
    let object: serde_json::Map<String, Value> = serde_json::from_str(raw).ok()?;
    object
        .into_iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

/// The assistant runs several model and tool steps (server limit: 240 s).
const ASSISTANT_TIMEOUT: Duration = Duration::from_secs(300);

/// An error as the UI shows it.
#[derive(Clone, Debug, Serialize)]
pub struct ApiError {
    /// HTTP status, 0 for network errors.
    pub status: u16,
    pub message: String,
}

impl ApiError {
    pub fn local(message: impl Into<String>) -> ApiError {
        ApiError {
            status: 0,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> ApiError {
        let message = if e.is_timeout() {
            "The server did not answer in time.".to_string()
        } else if e.is_connect() {
            "The TeamCraft server cannot be reached – check the address and your network."
                .to_string()
        } else {
            format!("Network error: {e}")
        };
        ApiError {
            status: e.status().map(|s| s.as_u16()).unwrap_or(0),
            message,
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> ApiError {
        ApiError::local(format!("File error: {e}"))
    }
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("TeamCraft-Desktop/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("HTTP client")
}

/// Only plain API paths such as `/items?q=x` (or `""` for the API index) –
/// no other hosts.
pub fn check_path(path: &str) -> Result<(), ApiError> {
    if path.is_empty() {
        return Ok(());
    }
    let ok = path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains("://")
        && !path.contains("..")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_control() || c == ' ');
    if ok {
        Ok(())
    } else {
        Err(ApiError::local(format!("Invalid API path '{path}'.")))
    }
}

#[derive(Clone)]
pub struct Api {
    pub client: reqwest::Client,
    /// Server base URL without trailing slash.
    pub base: String,
    pub token: Option<String>,
}

impl Api {
    pub(crate) fn url(&self, path: &str) -> Result<String, ApiError> {
        check_path(path)?;
        Ok(format!("{}/api/v1{path}", self.base))
    }

    pub(crate) fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.client.request(method, url);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        req
    }

    /// JSON request; error responses become their message.
    pub async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ApiError> {
        let url = self.url(path)?;
        let timeout = if path.starts_with("/assistant") {
            ASSISTANT_TIMEOUT
        } else {
            JSON_TIMEOUT
        };
        let mut req = self.request(method, &url).timeout(timeout);
        if let Some(body) = body {
            req = req.json(body);
        }
        read_json(req.send().await?).await
    }

    /// Downloads a resource (e.g. `/files/{id}/content`) into memory.
    pub async fn bytes(&self, path: &str) -> Result<Vec<u8>, ApiError> {
        let url = self.url(path)?;
        let resp = self.request(Method::GET, &url).send().await?;
        let resp = ensure_ok(resp).await?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// Streams a resource into a file (written next to it first, then renamed).
    pub async fn download_to(&self, path: &str, dest: &Path) -> Result<u64, ApiError> {
        let url = self.url(path)?;
        let mut resp = ensure_ok(self.request(Method::GET, &url).send().await?).await?;
        let tmp = dest.with_extension("teamcraft-download");
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut written = 0u64;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk).await?;
            written += chunk.len() as u64;
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&tmp, dest).await?;
        Ok(written)
    }

    /// Streams a file into `dest` for the working folder through a partial
    /// file next to it, checked against the SHA-256 the server named, then
    /// renamed over `dest`. `cancel` stops it between two chunks; what
    /// arrived stays in the partial file, and the next call asks the server
    /// only for the rest (`Range` / `If-Range`, see `resume`). `on_bytes` is
    /// told every chunk (and once the bytes already there).
    pub async fn download_verified(
        &self,
        path: &str,
        dest: &Path,
        expected_sha256: &str,
        cancel: &std::sync::atomic::AtomicBool,
        on_bytes: &mut (dyn FnMut(u64) + Send),
    ) -> Result<u64, ApiError> {
        crate::resume::download(self, path, dest, expected_sha256, cancel, on_bytes).await
    }

    /// Multipart request with `file` parts and text fields (uploads, check-in).
    pub async fn multipart(
        &self,
        path: &str,
        files: &[(String, std::path::PathBuf)],
        fields: &[(&str, String)],
    ) -> Result<Value, ApiError> {
        let url = self.url(path)?;
        let mut form = reqwest::multipart::Form::new();
        for (name, value) in fields {
            form = form.text(name.to_string(), value.clone());
        }
        for (filename, path) in files {
            let bytes = tokio::fs::read(path).await?;
            let part = reqwest::multipart::Part::bytes(bytes)
                .file_name(filename.clone())
                .mime_str("application/octet-stream")?;
            form = form.part("file", part);
        }
        read_json(
            self.request(Method::POST, &url)
                .multipart(form)
                .send()
                .await?,
        )
        .await
    }

    /// A multipart upload or check-in (`path`) of `files` into the revision
    /// `item`/`code`: small files go inside the request, files above
    /// [`CHUNKED_ABOVE`] are sent before as chunked uploads that resume
    /// after network errors and join the request by their ids (field
    /// `uploads`) – the server stores everything in one transaction.
    pub async fn send_files(
        &self,
        path: &str,
        item: &str,
        code: &str,
        files: &[(String, PathBuf)],
        fields: &[(&str, String)],
        transfer: &Transfer<'_>,
    ) -> Result<Value, ApiError> {
        let mut small = Vec::new();
        let mut sessions: Vec<String> = Vec::new();
        for (name, file) in files {
            let size = tokio::fs::metadata(file).await?.len();
            if size <= CHUNKED_ABOVE {
                small.push((name.clone(), file.clone()));
                continue;
            }
            // The session carries the file's base version and folder, so
            // the server refuses a conflict before any byte is sent.
            let mut announce = json!({ "item": item, "revision": code, "filename": name,
                                       "size": size });
            if let Some(base) = field_entry(fields, "bases", name) {
                announce["base"] = base;
            }
            if let Some(folder) = field_entry(fields, "folders", name).filter(Value::is_string) {
                announce["folder"] = folder;
            }
            match self
                .upload_chunked(announce, name, file, size, transfer)
                .await
            {
                Ok(id) => sessions.push(id),
                Err(e) => {
                    self.discard_uploads(&sessions).await;
                    return Err(e);
                }
            }
        }
        if let Err(e) = transfer.check_cancel() {
            self.discard_uploads(&sessions).await;
            return Err(e);
        }
        let mut fields: Vec<(&str, String)> = fields.to_vec();
        if !sessions.is_empty() {
            fields.push(("uploads", json!(sessions).to_string()));
        }
        let result = self.multipart(path, &small, &fields).await;
        if result.is_err() {
            self.discard_uploads(&sessions).await;
        }
        result
    }

    /// Cancels chunked uploads that will not be used (best effort – the
    /// server removes forgotten ones after a day).
    async fn discard_uploads(&self, ids: &[String]) {
        for id in ids {
            let _ = self
                .json(Method::DELETE, &format!("/uploads/{id}"), None)
                .await;
        }
    }

    /// Sends one file as a chunked upload (`announce` = the body that starts
    /// the session) and returns the upload id. A failed chunk is retried at
    /// the offset the server confirmed.
    async fn upload_chunked(
        &self,
        mut announce: Value,
        name: &str,
        file: &Path,
        size: u64,
        transfer: &Transfer<'_>,
    ) -> Result<String, ApiError> {
        (transfer.progress)(name, 0, size);
        // The server checks the content against it before it is stored.
        announce["sha256"] = Value::String(crate::workspace::sha256_of(file).await?);
        transfer.check_cancel()?;
        let session = self.json(Method::POST, "/uploads", Some(&announce)).await?;
        let id = session["id"]
            .as_str()
            .ok_or_else(|| ApiError::local("The server did not start the upload."))?
            .to_string();
        let chunk = session["chunk_bytes"]
            .as_u64()
            .unwrap_or(MAX_CHUNK)
            .clamp(64 * 1024, MAX_CHUNK);
        match self
            .send_chunks(&id, name, file, size, chunk, transfer)
            .await
        {
            Ok(()) => Ok(id),
            Err(e) => {
                self.discard_uploads(std::slice::from_ref(&id)).await;
                Err(e)
            }
        }
    }

    async fn send_chunks(
        &self,
        id: &str,
        name: &str,
        file: &Path,
        size: u64,
        chunk: u64,
        transfer: &Transfer<'_>,
    ) -> Result<(), ApiError> {
        let mut source = tokio::fs::File::open(file).await?;
        let mut buf = vec![0u8; usize::try_from(chunk).unwrap_or(1024 * 1024)];
        let mut offset = 0u64;
        let mut failures = 0u32;
        while offset < size {
            transfer.check_cancel()?;
            let len = chunk_len(size, offset, chunk);
            source.seek(std::io::SeekFrom::Start(offset)).await?;
            source.read_exact(&mut buf[..len]).await?;
            match self.patch_chunk(id, offset, buf[..len].to_vec()).await {
                Ok(next) if next > offset && next <= size => {
                    offset = next;
                    failures = 0;
                    (transfer.progress)(name, offset, size);
                }
                Ok(_) => {
                    return Err(ApiError::local(
                        "The server confirmed an unexpected upload offset.",
                    ))
                }
                Err(e) if retryable(&e) && failures < CHUNK_RETRIES => {
                    failures += 1;
                    tokio::time::sleep(backoff(failures)).await;
                    // Resume where the server is: the chunk may have arrived
                    // although its answer was lost.
                    if let Ok(server) = self.upload_offset(id).await {
                        if server <= size {
                            offset = server;
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Sends one chunk at `offset`; returns the offset the server confirmed.
    async fn patch_chunk(&self, id: &str, offset: u64, bytes: Vec<u8>) -> Result<u64, ApiError> {
        let url = self.url(&format!("/uploads/{id}"))?;
        let resp = self
            .request(Method::PATCH, &url)
            .timeout(CHUNK_TIMEOUT)
            .header("Upload-Offset", offset.to_string())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/offset+octet-stream",
            )
            .body(bytes)
            .send()
            .await?;
        let session = read_json(resp).await?;
        session["offset"]
            .as_u64()
            .ok_or_else(|| ApiError::local("The server did not confirm the upload offset."))
    }

    /// The offset the server has confirmed for an upload.
    async fn upload_offset(&self, id: &str) -> Result<u64, ApiError> {
        let session = self
            .json(Method::GET, &format!("/uploads/{id}"), None)
            .await?;
        session["offset"]
            .as_u64()
            .ok_or_else(|| ApiError::local("The server did not report the upload offset."))
    }
}

pub(crate) async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    if resp.status().is_success() {
        Ok(resp)
    } else {
        Err(error_of(resp).await)
    }
}

async fn read_json(resp: reqwest::Response) -> Result<Value, ApiError> {
    let resp = ensure_ok(resp).await?;
    if resp.status() == StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }
    let text = resp.text().await?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text)
        .map_err(|e| ApiError::local(format!("Unexpected answer of the server: {e}")))
}

/// `{"error": {"code", "message"}}` of the TeamCraft API, or the status text.
async fn error_of(resp: reqwest::Response) -> ApiError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| match status {
            StatusCode::UNAUTHORIZED => "Your sign-in has expired – please sign in again.".into(),
            StatusCode::NOT_FOUND => {
                "Not found on the server (is this a TeamCraft server?).".into()
            }
            _ => format!(
                "The server answered {} {}.",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ),
        });
    ApiError {
        status: status.as_u16(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths() {
        assert!(check_path("/items?q=bracket&limit=50").is_ok());
        assert!(check_path("/items/PRT-000001/revisions/A/files").is_ok());
        assert!(check_path("").is_ok(), "the API index");
        assert!(check_path("items").is_err());
        assert!(check_path("//evil.example/x").is_err());
        assert!(check_path("/x/../../admin").is_err());
        assert!(check_path("/https://evil.example").is_err());
        assert!(check_path("/a b").is_err());
    }

    #[test]
    fn chunks_and_retries() {
        assert_eq!(chunk_len(100, 0, 40), 40);
        assert_eq!(chunk_len(100, 80, 40), 20);
        assert_eq!(chunk_len(100, 100, 40), 0);
        assert_eq!(chunk_len(100, 120, 40), 0);
        assert!(retryable(&ApiError::local("network")));
        assert!(retryable(&ApiError {
            status: 409,
            message: "offset".into()
        }));
        assert!(retryable(&ApiError {
            status: 503,
            message: "busy".into()
        }));
        assert!(!retryable(&ApiError {
            status: 403,
            message: "no".into()
        }));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(20), Duration::from_secs(16));
    }

    #[test]
    fn session_takes_base_and_folder_from_the_fields() {
        let fields = [
            ("comment", "x".to_string()),
            (
                "bases",
                r#"{"Frame.IAM": "ab12", "hub.step": 3}"#.to_string(),
            ),
            ("folders", r#"{"frame.iam": "chassis/frames"}"#.to_string()),
        ];
        assert_eq!(
            field_entry(&fields, "bases", "frame.iam"),
            Some(json!("ab12"))
        );
        assert_eq!(field_entry(&fields, "bases", "hub.step"), Some(json!(3)));
        assert_eq!(
            field_entry(&fields, "folders", "FRAME.iam"),
            Some(json!("chassis/frames"))
        );
        assert_eq!(field_entry(&fields, "folders", "hub.step"), None);
        assert_eq!(field_entry(&fields, "missing", "hub.step"), None);
        assert_eq!(field_entry(&fields, "comment", "hub.step"), None);
    }

    #[test]
    fn cancel_switch() {
        let cancel = AtomicBool::new(false);
        let progress = |_: &str, _: u64, _: u64| {};
        let transfer = Transfer {
            cancel: &cancel,
            progress: &progress,
        };
        assert!(transfer.check_cancel().is_ok());
        cancel.store(true, Ordering::Relaxed);
        assert!(transfer.check_cancel().is_err());
    }
}
