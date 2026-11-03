//! Browser sign-in (loopback redirect with PKCE, see `services::desktop` on
//! the server): the app listens on 127.0.0.1, opens the confirmation page in
//! the default browser – where single sign-on works as usual – and exchanges
//! the one-time code it receives for a personal access token.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use reqwest::Method;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
};

use crate::api::{Api, ApiError};

/// How long the app waits for the browser.
const TIMEOUT: Duration = Duration::from_secs(300);

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// PKCE S256.
pub fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The pieces of one sign-in attempt.
pub struct Attempt {
    pub listener: TcpListener,
    pub state: String,
    pub verifier: String,
    /// The page to open in the browser.
    pub url: String,
}

/// Binds the loopback listener and builds the authorize URL.
pub async fn prepare(base: &str, device: &str) -> Result<Attempt, ApiError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let state = random_token(24);
    let verifier = random_token(48);
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("port", &port.to_string())
        .append_pair("state", &state)
        .append_pair("challenge", &challenge_of(&verifier))
        .append_pair("device", device)
        .finish();
    Ok(Attempt {
        listener,

        state,
        verifier,
        url: format!("{base}/desktop/authorize?{query}"),
    })
}

/// Outcome of the browser step.
#[derive(Debug, PartialEq)]
pub enum Callback {
    Code(String),
    Denied,
}

/// Waits for the browser to call `http://127.0.0.1:<port>/callback`. Requests
/// with another path or state are answered and ignored.
pub async fn wait_for_callback(attempt: &Attempt, cancel: &Notify) -> Result<Callback, ApiError> {
    let deadline = tokio::time::sleep(TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(ApiError::local("The sign-in in the browser was not completed within 5 minutes."));
            }
            _ = cancel.notified() => {
                return Err(ApiError::local("Sign-in cancelled."));
            }
            accepted = attempt.listener.accept() => {
                let (stream, _) = accepted?;
                if let Some(result) = handle(stream, &attempt.state).await {
                    return Ok(result);
                }
            }
        }
    }
}

/// Exchanges the code for a token: `{token, token_name, expires_at, user}`.
pub async fn exchange(api: &Api, code: &str, verifier: &str) -> Result<Value, ApiError> {
    api.json(
        Method::POST,
        "/desktop/token",
        Some(&json!({ "code": code, "verifier": verifier })),
    )
    .await
}

/// Reads one HTTP request; returns the outcome if it is the callback.
async fn handle(mut stream: TcpStream, state: &str) -> Option<Callback> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    // the request head only, at most 8 KiB, within 5 seconds
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    })
    .await;
    if read.is_err() {
        return None;
    }
    let head = String::from_utf8_lossy(&buf);
    let target = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or("");
    let outcome = parse_callback(target, state);
    let (status, text) = match &outcome {
        Some(Callback::Code(_)) => (
            "200 OK",
            "You are signed in. You can close this tab and return to TeamCraft.",
        ),
        Some(Callback::Denied) => (
            "200 OK",
            "The sign-in was cancelled. You can close this tab.",
        ),
        None => ("404 Not Found", "Nothing here."),
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>TeamCraft</title></head>\
         <body style=\"font-family:system-ui,sans-serif;background:#f8f8f8;color:#404040;\
         display:grid;place-items:center;height:90vh\"><p>{text}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    outcome
}

/// `/callback?code=…&state=…` or `/callback?error=access_denied&state=…`.
pub fn parse_callback(target: &str, expected_state: &str) -> Option<Callback> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        return None;
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return None;
    }
    match (code, error) {
        (Some(code), None) if !code.is_empty() => Some(Callback::Code(code)),
        (_, Some(_)) => Some(Callback::Denied),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636() {
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let verifier = random_token(48);
        assert_eq!(verifier.len(), 64, "between 43 and 128 characters");
    }

    #[test]
    fn callbacks() {
        assert_eq!(
            parse_callback("/callback?code=tc_dsk_abc&state=s1", "s1"),
            Some(Callback::Code("tc_dsk_abc".into()))
        );
        assert_eq!(
            parse_callback("/callback?error=access_denied&state=s1", "s1"),
            Some(Callback::Denied)
        );
        assert_eq!(parse_callback("/callback?code=x&state=other", "s1"), None);
        assert_eq!(parse_callback("/favicon.ico", "s1"), None);
        assert_eq!(parse_callback("/callback?state=s1", "s1"), None);
    }

    #[tokio::test]
    async fn loopback_round_trip() {
        let attempt = prepare("http://localhost:8080", "Test PC").await.unwrap();
        assert!(attempt
            .url
            .starts_with("http://localhost:8080/desktop/authorize?port="));
        assert!(attempt.url.contains("device=Test+PC"));
        let port = attempt.listener.local_addr().unwrap().port();
        let state = attempt.state.clone();
        let browser = tokio::spawn(async move {
            // a stray request first, then the real callback
            for target in [
                "/favicon.ico".to_string(),
                format!("/callback?code=tc_dsk_1&state={state}"),
            ] {
                let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut answer = String::new();
                s.read_to_string(&mut answer).await.unwrap();
                assert!(answer.starts_with("HTTP/1.1 "));
            }
        });
        let cancel = Notify::new();
        let result = wait_for_callback(&attempt, &cancel).await.unwrap();
        assert_eq!(result, Callback::Code("tc_dsk_1".into()));
        browser.await.unwrap();
    }
}
