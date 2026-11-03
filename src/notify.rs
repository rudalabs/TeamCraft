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

/// Reports an internal error. Never blocks and never fails.
pub fn report_error(message: &str) {
    let Some(n) = NOTIFIER.get() else { return };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    {
        let mut last = n.last_sent.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < MIN_INTERVAL) {
            n.suppressed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        *last = Some(Instant::now());
    }
    let suppressed = n.suppressed.swap(0, Ordering::Relaxed);
    let mut text = format!(
        "#### :rotating_light: {} error\n`{}`\n```\n{}\n```",
        n.instance,
        n.public_url,
        truncate(message, 3000)
    );
    if suppressed > 0 {
        text.push_str(&format!(
            "\n_{suppressed} further error(s) were suppressed during the last minute._"
        ));
    }
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
}
