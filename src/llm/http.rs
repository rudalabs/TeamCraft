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
