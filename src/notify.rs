//! Error notifications to a Mattermost incoming webhook
//! (`MATTERMOST_ERROR_WEBHOOK`). At most one message per minute is sent;
//! further errors in that window are counted and reported with the next one.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use serde_json::json;

use crate::config::Config;

const MIN_INTERVAL: Duration = Duration::from_secs(60);

static NOTIFIER: OnceLock<Notifier> = OnceLock::new();

struct Notifier {
    url: String,
    instance: String,
    public_url: String,
    http: reqwest::Client,
    last_sent: Mutex<Option<Instant>>,
    suppressed: AtomicU64,
}

/// Installs the notifier if a webhook is configured. Safe to call once.
pub fn init(config: &Config) {
    let Some(url) = config.mattermost_error_webhook.clone() else {
        return;
    };
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "cannot create HTTP client for Mattermost notifications");
            return;
        }
    };
    let _ = NOTIFIER.set(Notifier {
        url,
        instance: config.instance_name.clone(),
        public_url: config.public_url.clone(),
        http,
        last_sent: Mutex::new(None),
        suppressed: AtomicU64::new(0),
    });
}

impl Notifier {
    /// Whether a message may go out at `now`: at most one per
    /// [`MIN_INTERVAL`]. Returns the number of errors suppressed since the
    /// last message (and starts counting anew), or `None` when this error is
    /// only counted.
    fn admit(&self, now: Instant) -> Option<u64> {
        let mut last = self.last_sent.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| now.saturating_duration_since(t) < MIN_INTERVAL) {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        *last = Some(now);
        Some(self.suppressed.swap(0, Ordering::Relaxed))
    }

    /// The Mattermost message for an error.
    fn text(&self, message: &str, suppressed: u64) -> String {
        let mut text = format!(
            "#### :rotating_light: {} error\n`{}`\n```\n{}\n```",
            self.instance,
            self.public_url,
            truncate(message, 3000)
        );
        if suppressed > 0 {
            text.push_str(&format!(
                "\n_{suppressed} further error(s) were suppressed during the last minute._"
            ));
        }
        text
    }
}

/// Reports an internal error. Never blocks and never fails.
pub fn report_error(message: &str) {
    let Some(n) = NOTIFIER.get() else { return };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(suppressed) = n.admit(Instant::now()) else {
        return;
    };
    let text = n.text(message, suppressed);
    let request = n
        .http
        .post(&n.url)
        .json(&json!({ "username": "TeamCraft", "text": text }));
    runtime.spawn(async move {
        if let Err(e) = request.send().await.and_then(|r| r.error_for_status()) {
            tracing::warn!(error = %e, "sending the Mattermost error notification failed");
        }
    });
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 3), "abc…");
    }

    #[test]
    fn report_without_notifier_is_a_noop() {
        report_error("nothing happens");
    }

    fn notifier() -> Notifier {
        Notifier {
            url: "http://127.0.0.1:9/hooks/x".into(),
            instance: "TeamCraft Test".into(),
            public_url: "https://plm.example.org/".into(),
            http: reqwest::Client::new(),
            last_sent: Mutex::new(None),
            suppressed: AtomicU64::new(0),
        }
    }

    #[test]
    fn one_message_per_minute_and_the_rest_counted() {
        let n = notifier();
        let t0 = Instant::now();
        assert_eq!(n.admit(t0), Some(0), "the first error goes out");
        assert_eq!(n.admit(t0 + Duration::from_secs(1)), None);
        assert_eq!(n.admit(t0 + Duration::from_secs(59)), None);
        // the minute is over: the next message reports the two in between
        assert_eq!(n.admit(t0 + MIN_INTERVAL), Some(2));
        assert_eq!(n.admit(t0 + MIN_INTERVAL + Duration::from_secs(1)), None);
        assert_eq!(
            n.admit(t0 + MIN_INTERVAL * 3),
            Some(1),
            "counting starts anew"
        );
        // a clock that seems to go backwards does not send twice
        assert_eq!(n.admit(t0), None);
    }

    #[test]
    fn the_message_names_instance_and_address_and_is_bounded() {
        let n = notifier();
        let text = n.text("database error: boom", 0);
        assert!(text.starts_with("#### :rotating_light: TeamCraft Test error\n"));
        assert!(text.contains("`https://plm.example.org/`"));
        assert!(text.contains("```\ndatabase error: boom\n```"));
        assert!(!text.contains("suppressed"));
        let text = n.text("x", 3);
        assert!(text.ends_with("_3 further error(s) were suppressed during the last minute._"));
        // a huge error text is cut at 3000 characters (multi-byte safe)
        let text = n.text(&"ü".repeat(10_000), 0);
        assert_eq!(text.matches('ü').count(), 3000);
        assert!(text.contains("ü…\n```"));
    }
}
