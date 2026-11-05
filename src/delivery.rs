//! Delivery of notifications by e-mail and to Mattermost.
//!
//! The `notifications` table is the outbox: a notification is created with
//! `email_state` / `chat_state` = `pending` when the user wants that channel.
//! A background task ([`spawn`]) sends the pending ones every few seconds:
//!
//! * **E-mail** via SMTP ([lettre](https://lettre.rs), MIT) when
//!   `TEAMCRAFT_SMTP_URL` and `TEAMCRAFT_MAIL_FROM` are set; plain text with a
//!   link to the object and to the notification settings.
//! * **Mattermost** via an incoming webhook (`TEAMCRAFT_MATTERMOST_WEBHOOK`):
//!   a post in the webhook's channel that @-mentions the user, or a direct
//!   message (`TEAMCRAFT_MATTERMOST_DIRECT=true`). The Mattermost user is the
//!   TeamCraft username unless the user set another one.
//!
//! Without configuration the pending rows are marked `skipped`. A failed
//! delivery is retried on the next runs and marked `failed` after
//! [`MAX_ATTEMPTS`]. One TeamCraft instance delivers (no row locking).

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use lettre::{
    message::{header::ContentType, Mailbox},
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{config::Config, error::AppResult};

/// Delivery attempts per channel before a notification is marked `failed`.
pub const MAX_ATTEMPTS: i32 = 5;
/// Pause between two runs of the background task.
const INTERVAL: Duration = Duration::from_secs(15);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Sends one e-mail (SMTP in production, a recorder in tests).
pub trait MailSender: Send + Sync {
    fn send(&self, message: Message) -> BoxFuture<'_, anyhow::Result<()>>;
}

struct SmtpSender(AsyncSmtpTransport<Tokio1Executor>);

impl MailSender for SmtpSender {
    fn send(&self, message: Message) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            self.0.send(message).await?;
            Ok(())
        })
    }
}

/// Posts to Mattermost (an incoming webhook in production).
pub trait ChatSender: Send + Sync {
    fn post(&self, payload: Value) -> BoxFuture<'_, anyhow::Result<()>>;
}

struct WebhookSender {
    http: reqwest::Client,
    url: String,
}

impl ChatSender for WebhookSender {
    fn post(&self, payload: Value) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            self.http
                .post(&self.url)
                .json(&payload)
                .send()
                .await?
                .error_for_status()?;
            Ok(())
        })
    }
}

/// The configured channels.
#[derive(Clone)]
pub struct Delivery {
    public_url: String,
    instance_name: String,
    mail: Option<(Arc<dyn MailSender>, Mailbox)>,
    chat: Option<(Arc<dyn ChatSender>, bool)>,
}

/// What one run delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub emails: u64,
    pub chats: u64,
    pub failures: u64,
}

impl Delivery {
    pub fn from_config(config: &Config) -> anyhow::Result<Delivery> {
        let mail = match &config.smtp {
            Some(smtp) => {
                let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(&smtp.url)
                    .map_err(|e| anyhow::anyhow!("TEAMCRAFT_SMTP_URL is not usable: {e}"))?
                    .timeout(Some(Duration::from_secs(20)))
                    .build();
                let from: Mailbox = smtp
                    .from
                    .parse()
                    .map_err(|e| anyhow::anyhow!("TEAMCRAFT_MAIL_FROM: {e}"))?;
                Some((Arc::new(SmtpSender(transport)) as Arc<dyn MailSender>, from))
            }
            None => None,
        };
        let chat = match &config.mattermost {
            Some(m) => {
                let http = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()?;
                let sender = WebhookSender {
                    http,
                    url: m.webhook.clone(),
                };
                Some((Arc::new(sender) as Arc<dyn ChatSender>, m.direct))
            }
            None => None,
        };
        Ok(Delivery {
            public_url: config.public_url.clone(),
            instance_name: config.instance_name.clone(),
            mail,
            chat,
        })
    }

    /// Delivery with explicit senders (tests).
    pub fn with_senders(
        public_url: &str,
        instance_name: &str,
        mail: Option<(Arc<dyn MailSender>, &str)>,
        chat: Option<(Arc<dyn ChatSender>, bool)>,
    ) -> anyhow::Result<Delivery> {
        let mail = match mail {
            Some((sender, from)) => Some((sender, from.parse()?)),
            None => None,
        };
        Ok(Delivery {
            public_url: public_url.trim_end_matches('/').to_string(),
            instance_name: instance_name.to_string(),
            mail,
            chat,
        })
    }

    pub fn email_enabled(&self) -> bool {
        self.mail.is_some()
    }

    pub fn chat_enabled(&self) -> bool {
        self.chat.is_some()
    }

    /// Delivers up to 100 pending notifications.
    pub async fn run_once(&self, pool: &PgPool) -> AppResult<Report> {
        // Channels that are not configured: nothing will ever send them.
        if self.mail.is_none() {
            sqlx::query(
                "UPDATE notifications SET email_state = 'skipped' WHERE email_state = 'pending'",
            )
            .execute(pool)
            .await?;
        }
        if self.chat.is_none() {
            sqlx::query(
                "UPDATE notifications SET chat_state = 'skipped' WHERE chat_state = 'pending'",
            )
            .execute(pool)
            .await?;
        }
        // A group restricted (or a membership lost) since the notification was
        // queued: the recipient may no longer read it, so it is not sent.
        sqlx::query(&format!(
            "UPDATE notifications n
             SET email_state = CASE WHEN n.email_state = 'pending' THEN 'skipped' ELSE n.email_state END,
                 chat_state = CASE WHEN n.chat_state = 'pending' THEN 'skipped' ELSE n.chat_state END
             FROM users u
             WHERE u.id = n.user_id AND (n.email_state = 'pending' OR n.chat_state = 'pending')
               AND NOT {}",
            crate::services::notifications::notification_visible(
                "n",
                "u.id",
                &crate::services::access::hidden_of_user("u")
            )
        ))
        .execute(pool)
        .await?;
        #[derive(sqlx::FromRow)]
        struct Row {
            id: Uuid,
            title: String,
            body: String,
            link: String,
            email_state: String,
            chat_state: String,
            delivery_attempts: i32,
            email: Option<String>,
            display_name: String,
            chat_user: String,
            confidential: bool,
        }
        // `confidential` is evaluated now: a group restricted after the row
        // was queued keeps the text out of a shared channel.
        let rows = sqlx::query_as::<_, Row>(&format!(
            "SELECT n.id, n.title, n.body, n.link, n.email_state, n.chat_state, n.delivery_attempts,
                    u.email, u.display_name, COALESCE(u.mattermost_username, u.username) AS chat_user,
                    {} AS confidential
             FROM notifications n JOIN users u ON u.id = n.user_id
             WHERE n.email_state = 'pending' OR n.chat_state = 'pending'
             ORDER BY n.created_at LIMIT 100",
            crate::services::notifications::confidential_now("n")
        ))
        .fetch_all(pool)
        .await?;
        let mut report = Report::default();
        for row in rows {
            let url = format!("{}{}", self.public_url, row.link);
            let mut errors = Vec::new();
            let mut email_state = row.email_state.clone();
            if email_state == "pending" {
                match self
                    .send_mail(
                        &row.title,
                        &row.body,
                        &url,
                        row.email.as_deref(),
                        &row.display_name,
                    )
                    .await
                {
                    Ok(()) => {
                        email_state = "sent".into();
                        report.emails += 1;
                    }
                    Err(e) => errors.push(format!("e-mail: {e}")),
                }
            }
            let mut chat_state = row.chat_state.clone();
            if chat_state == "pending" {
                match self
                    .post_chat(
                        &row.title,
                        &row.body,
                        &url,
                        &row.chat_user,
                        row.confidential,
                    )
                    .await
                {
                    Ok(()) => {
                        chat_state = "sent".into();
                        report.chats += 1;
                    }
                    Err(e) => errors.push(format!("Mattermost: {e}")),
                }
            }
            let attempts = row.delivery_attempts + i32::from(!errors.is_empty());
            if !errors.is_empty() {
                report.failures += 1;
                tracing::warn!(notification = %row.id, attempts, errors = %errors.join("; "), "notification delivery failed");
                if attempts >= MAX_ATTEMPTS {
                    if email_state == "pending" {
                        email_state = "failed".into();
                    }
                    if chat_state == "pending" {
                        chat_state = "failed".into();
                    }
                }
            }
            sqlx::query(
                "UPDATE notifications SET email_state = $2, chat_state = $3, delivery_attempts = $4,
                        delivery_error = $5
                 WHERE id = $1",
            )
            .bind(row.id)
            .bind(&email_state)
            .bind(&chat_state)
            .bind(attempts)
            .bind(errors.join("; "))
            .execute(pool)
            .await?;
        }
        Ok(report)
    }

    async fn send_mail(
        &self,
        title: &str,
        body: &str,
        url: &str,
        to: Option<&str>,
        display_name: &str,
    ) -> anyhow::Result<()> {
        let (sender, from) = self
            .mail
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("SMTP is not configured"))?;
        let address = to
            .filter(|a| !a.is_empty())
            .ok_or_else(|| anyhow::anyhow!("the user has no e-mail address"))?;
        // lettre quotes and encodes the name; control characters are dropped.
        let name = sanitize_display_name(display_name);
        let to = Mailbox::new((!name.is_empty()).then_some(name), address.parse()?);
        let message = Message::builder()
            .from(from.clone())
            .to(to)
            .subject(format!("[{}] {title}", self.instance_name))
            .header(ContentType::TEXT_PLAIN)
            .body(email_text(
                &self.instance_name,
                &self.public_url,
                title,
                body,
                url,
            ))?;
        sender.send(message).await
    }

    async fn post_chat(
        &self,
        title: &str,
        body: &str,
        url: &str,
        user: &str,
        confidential: bool,
    ) -> anyhow::Result<()> {
        let (sender, direct) = self
            .chat
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Mattermost is not configured"))?;
        let (title, body) = chat_text(&self.instance_name, title, body, confidential, *direct);
        sender
            .post(chat_payload(
                &self.instance_name,
                &title,
                &body,
                url,
                user,
                *direct,
            ))
            .await
    }
}

/// What a chat message may show. A notification about a restricted project
/// group posted into a shared channel (not a direct message) shows only a
/// generic text and the link; everybody in the channel could read it.
pub fn chat_text(
    instance: &str,
    title: &str,
    body: &str,
    confidential: bool,
    direct: bool,
) -> (String, String) {
    if confidential && !direct {
        (format!("new {instance} notification"), String::new())
    } else {
        (title.to_string(), body.to_string())
    }
}

/// Display names go into the `To:` header: no quotes, brackets or line breaks.
fn sanitize_display_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>' | ',' | ';' | '\\'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// Plain-text body of a notification e-mail.
pub fn email_text(instance: &str, public_url: &str, title: &str, body: &str, url: &str) -> String {
    format!(
        "{title}\n\n{body}\n\nOpen in {instance}: {url}\n\n-- \nYou receive this e-mail because of your notification \
         settings in {instance}. Change them on your account page: {public_url}/account#notifications\n"
    )
}

/// Mattermost webhook payload: a mention in the channel or a direct message.
pub fn chat_payload(
    instance: &str,
    title: &str,
    body: &str,
    url: &str,
    user: &str,
    direct: bool,
) -> Value {
    let text = if direct {
        format!("**[{title}]({url})**\n{body}")
    } else {
        format!("@{user} **[{title}]({url})**\n{body}")
    };
    let mut payload = json!({ "username": instance, "text": text });
    if direct {
        payload["channel"] = json!(format!("@{user}"));
    }
    payload
}

/// Starts the background delivery (does nothing if no channel is configured,
/// apart from marking the pending notifications as skipped).
pub fn spawn(pool: PgPool, delivery: Delivery) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match delivery.run_once(&pool).await {
                Ok(r) if r.emails + r.chats > 0 => {
                    tracing::info!(
                        emails = r.emails,
                        mattermost = r.chats,
                        "delivered notifications"
                    )
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "notification delivery failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_body_links_object_and_settings() {
        let text = email_text(
            "TeamCraft",
            "https://plm.example.org",
            "Review requested: CHG-000004",
            "Erika asks you to review PRT-000003/B.",
            "https://plm.example.org/changes/1",
        );
        assert!(text.starts_with("Review requested: CHG-000004\n\nErika asks"));
        assert!(text.contains("Open in TeamCraft: https://plm.example.org/changes/1"));
        assert!(text.contains("https://plm.example.org/account#notifications"));
    }

    #[test]
    fn mattermost_mentions_or_direct_messages() {
        let p = chat_payload("TeamCraft", "T", "B", "https://x/y", "erika", false);
        assert_eq!(p["text"], "@erika **[T](https://x/y)**\nB");
        assert!(p.get("channel").is_none());
        let p = chat_payload("TeamCraft", "T", "B", "https://x/y", "erika", true);
        assert_eq!(p["channel"], "@erika");
        assert!(!p["text"].as_str().unwrap().contains("@erika"));
        assert_eq!(p["username"], "TeamCraft");
    }

    #[test]
    fn confidential_notifications_are_generic_in_channels() {
        let (t, b) = chat_text("TeamCraft", "Secret PRT", "body", true, false);
        assert_eq!(t, "new TeamCraft notification");
        assert!(b.is_empty());
        let (t, b) = chat_text("TeamCraft", "Secret PRT", "body", true, true);
        assert_eq!((t.as_str(), b.as_str()), ("Secret PRT", "body"));
        let (t, _) = chat_text("TeamCraft", "Open PRT", "body", false, false);
        assert_eq!(t, "Open PRT");
    }

    #[test]
    fn display_names_are_header_safe() {
        assert_eq!(
            sanitize_display_name("Erika \"E\" <x>\r\nBcc: y"),
            "Erika E xBcc: y"
        );
    }
}
