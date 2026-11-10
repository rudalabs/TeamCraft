//! Direct messages through the Mattermost REST API as a bot account.
//!
//! A notification goes to **one person only**: the bot looks the user up by
//! username (`GET /api/v4/users/username/{name}`, cached), opens the direct
//! channel bot <-> user (`POST /api/v4/channels/direct`) and posts into it
//! (`POST /api/v4/posts`). Nothing is ever posted into a channel and there is
//! no fallback to one: an unknown or deactivated user is a failure of the chat
//! delivery with a reason the user reads under *Account → Notifications*.
//!
//! The server URL and the bot token are set by the administrator (not by
//! users), the HTTP client follows no redirects (the bearer token must not
//! travel to another host) and ignores proxy environment variables.
//!
//! [`classify`] and [`dm_text`] are pure and unit tested; [`BotClient`] is the
//! one network implementation of [`DirectMessenger`].

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::sync::OnceCell;

use crate::delivery::BoxFuture;

/// How long a user lookup (and the direct channel) is reused. A user who is
/// deactivated meanwhile is noticed by the next lookup after this time (or
/// by Mattermost refusing the post).
const CACHE_TTL: Duration = Duration::from_secs(600);
/// Mattermost refuses posts above 16383 characters.
const MAX_MESSAGE_CHARS: usize = 15_000;

/// Why a direct message was not delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatFailure {
    /// Shown to the user (no secrets).
    pub message: String,
    /// `true`: retrying cannot help until somebody changes something (unknown
    /// or deactivated user); the delivery is marked failed at once.
    pub permanent: bool,
}

impl ChatFailure {
    pub fn transient(message: impl Into<String>) -> ChatFailure {
        ChatFailure {
            message: message.into(),
            permanent: false,
        }
    }

    pub fn permanent(message: impl Into<String>) -> ChatFailure {
        ChatFailure {
            message: message.into(),
            permanent: true,
        }
    }
}

impl std::fmt::Display for ChatFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<anyhow::Error> for ChatFailure {
    fn from(e: anyhow::Error) -> ChatFailure {
        ChatFailure::transient(e.to_string())
    }
}

/// Sends a text as a direct message to a Mattermost user (the bot in
/// production, a recorder in tests).
pub trait DirectMessenger: Send + Sync {
    fn send(&self, username: &str, text: &str) -> BoxFuture<'_, Result<(), ChatFailure>>;
}

/// The API call a failure belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `GET /api/v4/users/username/{name}` (the person).
    LookupUser,
    /// `GET /api/v4/users/me` (the bot itself).
    LookupBot,
    /// `POST /api/v4/channels/direct`.
    OpenChannel,
    /// `POST /api/v4/posts`.
    Post,
}

/// Maps an HTTP status of a failed call to what the user (and the retry
/// logic) should learn.
pub fn classify(step: Step, status: u16, username: &str) -> ChatFailure {
    match (step, status) {
        (Step::LookupUser, 404) => ChatFailure::permanent(format!(
            "Mattermost user \"{username}\" not found – check your Mattermost username"
        )),
        (_, 401) => ChatFailure::transient(
            "Mattermost rejected the bot token (HTTP 401) – the administrator must check TEAMCRAFT_MATTERMOST_BOT_TOKEN",
        ),
        (_, 403) => ChatFailure::transient(format!(
            "the Mattermost bot may not do this (HTTP 403) – the administrator must check its permissions for \"{username}\""
        )),
        (_, 429) => ChatFailure::transient("Mattermost is rate limiting the bot (HTTP 429)"),
        (_, code) => ChatFailure::transient(format!("Mattermost answered HTTP {code}")),
    }
}

/// The failure for a user that exists but is deactivated (`delete_at` set).
pub fn deactivated(username: &str) -> ChatFailure {
    ChatFailure::permanent(format!(
        "Mattermost user \"{username}\" is deactivated – check your Mattermost username"
    ))
}

/// Text of the direct message: the title links to the object.
pub fn dm_text(title: &str, body: &str, url: &str) -> String {
    let text = if body.is_empty() {
        format!("**[{title}]({url})**")
    } else {
        format!("**[{title}]({url})**\n{body}")
    };
    if text.chars().count() > MAX_MESSAGE_CHARS {
        let mut cut: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
        cut.push('…');
        cut
    } else {
        text
    }
}

struct Cached {
    at: Instant,
    user_id: String,
    channel_id: Option<String>,
}

/// The Mattermost REST API as a bot.
pub struct BotClient {
    http: reqwest::Client,
    base: reqwest::Url,
    token: String,
    bot_id: OnceCell<String>,
    cache: Mutex<HashMap<String, Cached>>,
}

impl BotClient {
    pub fn new(url: &str, token: &str) -> anyhow::Result<BotClient> {
        let base = reqwest::Url::parse(url)
            .map_err(|e| anyhow::anyhow!("TEAMCRAFT_MATTERMOST_URL is not usable: {e}"))?;
        if !matches!(base.scheme(), "http" | "https") || base.cannot_be_a_base() {
            anyhow::bail!("TEAMCRAFT_MATTERMOST_URL must be an http(s) URL");
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent(concat!("TeamCraft-Mattermost/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(BotClient {
            http,
            base,
            token: token.to_string(),
            bot_id: OnceCell::new(),
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// `<base>/api/v4/<segments…>`; the segments are percent-encoded (a
    /// username cannot add path parts). A server below a path prefix works.
    fn endpoint(&self, segments: &[&str]) -> reqwest::Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(["api", "v4"]).extend(segments);
        }
        url
    }

    async fn call(
        &self,
        step: Step,
        username: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<Value, ChatFailure> {
        let response = request.bearer_auth(&self.token).send().await.map_err(|e| {
            ChatFailure::transient(format!("Mattermost is not reachable: {}", short(&e)))
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify(step, status.as_u16(), username));
        }
        response
            .json::<Value>()
            .await
            .map_err(|_| ChatFailure::transient("Mattermost sent an answer TeamCraft cannot read"))
    }

    async fn bot_id(&self) -> Result<String, ChatFailure> {
        self.bot_id
            .get_or_try_init(|| async {
                let me = self
                    .call(
                        Step::LookupBot,
                        "",
                        self.http.get(self.endpoint(&["users", "me"])),
                    )
                    .await?;
                id_of(&me)
            })
            .await
            .cloned()
    }

    fn cached(&self, username: &str) -> Option<(String, Option<String>)> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(username)
            .filter(|c| c.at.elapsed() < CACHE_TTL)
            .map(|c| (c.user_id.clone(), c.channel_id.clone()))
    }

    fn remember(&self, username: &str, user_id: &str, channel_id: Option<&str>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 5000 {
            cache.clear();
        }
        cache.insert(
            username.to_string(),
            Cached {
                at: Instant::now(),
                user_id: user_id.to_string(),
                channel_id: channel_id.map(str::to_string),
            },
        );
    }

    fn forget(&self, username: &str) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(username);
    }

    async fn deliver(&self, username: &str, text: &str) -> Result<(), ChatFailure> {
        let (user_id, channel_id) = match self.cached(username) {
            Some(hit) => hit,
            None => {
                let user = self
                    .call(
                        Step::LookupUser,
                        username,
                        self.http
                            .get(self.endpoint(&["users", "username", username])),
                    )
                    .await?;
                if user["delete_at"].as_i64().unwrap_or(0) > 0 {
                    return Err(deactivated(username));
                }
                let id = id_of(&user)?;
                self.remember(username, &id, None);
                (id, None)
            }
        };
        let channel_id = match channel_id {
            Some(id) => id,
            None => {
                let bot = self.bot_id().await?;
                let channel = self
                    .call(
                        Step::OpenChannel,
                        username,
                        self.http
                            .post(self.endpoint(&["channels", "direct"]))
                            .json(&json!([bot, user_id])),
                    )
                    .await?;
                let id = id_of(&channel)?;
                self.remember(username, &user_id, Some(&id));
                id
            }
        };
        self.call(
            Step::Post,
            username,
            self.http
                .post(self.endpoint(&["posts"]))
                .json(&json!({ "channel_id": channel_id, "message": text })),
        )
        .await?;
        Ok(())
    }
}

impl DirectMessenger for BotClient {
    fn send(&self, username: &str, text: &str) -> BoxFuture<'_, Result<(), ChatFailure>> {
        let (username, text) = (username.to_string(), text.to_string());
        Box::pin(async move {
            let result = self.deliver(&username, &text).await;
            if result.is_err() {
                // a changed account or channel is looked up again on the retry
                self.forget(&username);
            }
            result
        })
    }
}

fn id_of(value: &Value) -> Result<String, ChatFailure> {
    value["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ChatFailure::transient("Mattermost sent an answer without an id"))
}

/// A transport error without the URL (it carries no secret, but is noise).
fn short(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connection failed"
    } else {
        "request failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_user_is_permanent_and_names_the_user() {
        let f = classify(Step::LookupUser, 404, "xy");
        assert!(f.permanent);
        assert_eq!(
            f.message,
            "Mattermost user \"xy\" not found – check your Mattermost username"
        );
        assert!(deactivated("xy").permanent);
    }

    #[test]
    fn configuration_problems_are_retried_and_explained() {
        let f = classify(Step::Post, 401, "xy");
        assert!(!f.permanent && f.message.contains("bot token"));
        assert!(!classify(Step::OpenChannel, 403, "xy").permanent);
        assert!(!classify(Step::Post, 429, "xy").permanent);
        assert_eq!(
            classify(Step::Post, 500, "xy").message,
            "Mattermost answered HTTP 500"
        );
        // a 404 of another step is not "user not found"
        assert!(!classify(Step::Post, 404, "xy").permanent);
    }

    #[test]
    fn message_links_the_title_and_is_capped() {
        assert_eq!(dm_text("T", "B", "https://x/y"), "**[T](https://x/y)**\nB");
        assert_eq!(dm_text("T", "", "https://x/y"), "**[T](https://x/y)**");
        let long = dm_text("T", &"x".repeat(20_000), "https://x/y");
        assert!(long.chars().count() <= MAX_MESSAGE_CHARS + 1);
    }

    #[test]
    fn endpoints_encode_the_username_and_keep_a_path_prefix() {
        let c = BotClient::new("https://chat.example.org/mm/", "t").unwrap();
        assert_eq!(
            c.endpoint(&["users", "username", "a/b c"]).as_str(),
            "https://chat.example.org/mm/api/v4/users/username/a%2Fb%20c"
        );
        assert!(BotClient::new("ftp://chat.example.org", "t").is_err());
    }
}
