//! The one HTTP client of the LLM layer: timeouts, no redirects (a 3xx is an
//! error, so the API key never goes to a second host), the key only in
//! its request header.

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};

use super::{AuthStyle, HttpCall, HttpResponse, LlmError, Transport};

/// Time limit of one HTTP call.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// HTTPS to the configured API.
pub struct HttpTransport {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    auth: AuthStyle,
}

impl HttpTransport {
    /// `base_url` without a trailing slash; the call's path is added.
    pub fn new(
        base_url: &str,
        api_key: &str,
        auth: AuthStyle,
        timeout: Duration,
    ) -> anyhow::Result<HttpTransport> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("TeamCraft/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(HttpTransport {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            auth,
        })
    }
}

impl Transport for HttpTransport {
    async fn send(&self, call: &HttpCall) -> Result<HttpResponse, LlmError> {
        let mut req = self
            .http
            .post(format!("{}{}", self.base_url, call.path))
            .json(&call.body);
        for (name, value) in &call.headers {
            req = req.header(*name, value);
        }
        if !self.api_key.is_empty() {
            let (name, value) = match self.auth {
                AuthStyle::XApiKey => ("x-api-key", self.api_key.clone()),
                AuthStyle::Bearer => ("authorization", format!("Bearer {}", self.api_key)),
            };
            let mut value = HeaderValue::from_str(&value)
                .map_err(|_| LlmError::Network("the API key is not a valid header value".into()))?;
            value.set_sensitive(true);
            req = req.header(HeaderName::from_static(name), value);
        }
        let response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout
            } else {
                // without the URL: keep logs short
                LlmError::Network(e.without_url().to_string())
            }
        })?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response
            .text()
            .await
            .map_err(|e| LlmError::Network(e.without_url().to_string()))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";

    /// Reads one request (the headers are enough) and answers `response`.
    async fn serve_once(listener: TcpListener, response: String) -> String {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 16 * 1024];
        let mut seen = 0;
        loop {
            let n = socket.read(&mut buf[seen..]).await.unwrap();
            seen += n;
            if n == 0 || buf[..seen].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.ok();
        String::from_utf8_lossy(&buf[..seen]).to_string()
    }

    fn call() -> HttpCall {
        HttpCall {
            path: "/chat/completions",
            headers: vec![("x-extra", "1".to_string())],
            body: serde_json::json!({ "model": "m" }),
        }
    }

    fn transport(base: &str, key: &str, auth: AuthStyle) -> HttpTransport {
        HttpTransport::new(base, key, auth, Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_the_key_stays_on_the_host() {
        let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (p1, p2) = (
            first.local_addr().unwrap().port(),
            second.local_addr().unwrap().port(),
        );
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{p2}/elsewhere\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let server = tokio::spawn(serve_once(first, redirect));
        let t = transport(
            &format!("http://127.0.0.1:{p1}/v1/"),
            "secret-key",
            AuthStyle::Bearer,
        );
        let response = t.send(&call()).await.unwrap();
        assert_eq!(response.status, 302, "returned, not followed");
        let request = server.await.unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("post /v1/chat/completions "),
            "{request}"
        );
        assert!(request.contains("authorization: bearer secret-key"));
        assert!(request.contains("x-extra: 1"));
        // nobody connected to the second host
        let hit = tokio::time::timeout(Duration::from_millis(300), second.accept()).await;
        assert!(hit.is_err(), "the key must not go to another host");
    }

    #[tokio::test]
    async fn the_key_header_follows_the_provider_and_is_optional() {
        for (auth, expected) in [
            (AuthStyle::XApiKey, "x-api-key: secret-key"),
            (AuthStyle::Bearer, "authorization: bearer secret-key"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(serve_once(listener, OK.to_string()));
            let t = transport(&format!("http://127.0.0.1:{port}"), "secret-key", auth);
            let r = t.send(&call()).await.unwrap();
            assert_eq!((r.status, r.body.as_str()), (200, "{}"));
            assert!(server
                .await
                .unwrap()
                .to_ascii_lowercase()
                .contains(expected));
        }
        // no key (local server): no authorization header at all
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(serve_once(listener, OK.to_string()));
        let t = transport(&format!("http://127.0.0.1:{port}"), "", AuthStyle::Bearer);
        t.send(&call()).await.unwrap();
        let request = server.await.unwrap().to_ascii_lowercase();
        assert!(!request.contains("authorization") && !request.contains("x-api-key"));
    }

    #[tokio::test]
    async fn network_errors_do_not_carry_the_url() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let t = transport(
            &format!("http://127.0.0.1:{port}/v1"),
            "secret-key",
            AuthStyle::Bearer,
        );
        let e = t.send(&call()).await.unwrap_err();
        assert!(matches!(e, LlmError::Network(_)), "{e:?}");
        let text = e.to_string();
        assert!(
            !text.contains("secret-key") && !text.contains("/v1"),
            "{text}"
        );
    }
}
