//! HTTP client for the TeamCraft REST API (`/api/v1`, bearer token).
//!
//! All network traffic of the desktop app goes through Rust: the web view
//! never talks to the server itself (no CORS, the token never reaches
//! JavaScript).

use std::{path::Path, time::Duration};

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

const JSON_TIMEOUT: Duration = Duration::from_secs(60);

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
    fn url(&self, path: &str) -> Result<String, ApiError> {
        check_path(path)?;
        Ok(format!("{}/api/v1{path}", self.base))
    }

    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
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
        let mut req = self.request(method, &url).timeout(JSON_TIMEOUT);
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

    /// Streams a file into `dest` for the working folder: written next to it
    /// first (`<name>.teamcraft-download`), checked against the SHA-256 the
    /// server named, then renamed over `dest`. `cancel` stops it between two
    /// chunks (the partial file is removed); `on_bytes` is told every chunk.
    pub async fn download_verified(
        &self,
        path: &str,
        dest: &Path,
        expected_sha256: &str,
        cancel: &std::sync::atomic::AtomicBool,
        on_bytes: &mut (dyn FnMut(u64) + Send),
    ) -> Result<u64, ApiError> {
        use sha2::{Digest, Sha256};
        use std::sync::atomic::Ordering;
        let url = self.url(path)?;
        let mut resp = ensure_ok(self.request(Method::GET, &url).send().await?).await?;
        let mut tmp = dest.as_os_str().to_owned();
        tmp.push(".teamcraft-download");
        let tmp = std::path::PathBuf::from(tmp);
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        let result: Result<(), ApiError> = async {
            while let Some(chunk) = resp.chunk().await? {
                if cancel.load(Ordering::Relaxed) {
                    return Err(ApiError::local("Cancelled."));
                }
                file.write_all(&chunk).await?;
                hasher.update(&chunk);
                written += chunk.len() as u64;
                on_bytes(chunk.len() as u64);
            }
            file.flush().await?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
        let sha = hex::encode(hasher.finalize());
        if !expected_sha256.is_empty() && !sha.eq_ignore_ascii_case(expected_sha256) {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ApiError::local(format!(
                "{} arrived damaged (SHA-256 does not match) – get it again.",
                dest.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        if let Err(e) = tokio::fs::rename(&tmp, dest).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ApiError::local(format!(
                "{} could not be replaced (open in a CAD program?): {e}",
                dest.display()
            )));
        }
        Ok(written)
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
}

async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response, ApiError> {
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
}
