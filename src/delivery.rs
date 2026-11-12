//! Delivery of notifications by e-mail and to Mattermost.
//!
//! The `notifications` table is the outbox: a notification is created with
//! `email_state` / `chat_state` = `pending` when the user wants that channel.
//! A background task ([`spawn`]) sends the pending ones every few seconds:
//!
//! * **E-mail** via SMTP ([lettre](https://lettre.rs), MIT) when
//!   `TEAMCRAFT_SMTP_URL` and `TEAMCRAFT_MAIL_FROM` are set; plain text with a
//!   link to the object and to the notification settings.
//! * **Mattermost** as a direct message from a bot account through the REST API
//!   ([`crate::mattermost`], `TEAMCRAFT_MATTERMOST_URL` + `…_BOT_TOKEN`); never a
//!   post in a channel. An unknown or deactivated user fails at once with a
//!   reason the user sees. Deprecated: an incoming webhook
//!   (`TEAMCRAFT_MATTERMOST_WEBHOOK`): a post in the webhook's channel that
//!   @-mentions the user, or a direct message (`TEAMCRAFT_MATTERMOST_DIRECT=true`).
//!   The Mattermost user is the TeamCraft username unless the user set another one.
//!
//! Without configuration the pending rows are marked `skipped`. A failed
//! delivery is retried on the next runs and marked `failed` after
//! [`MAX_ATTEMPTS`]. One TeamCraft instance delivers (no row locking).
//!
//! A run serves [`PARALLEL_RECIPIENTS`] recipients at the same time; the
//! notifications of one recipient go out one after the other, in the order
//! they were queued, e-mail and chat of a notification side by side. A mail
//! server that answers slowly (the SMTP timeout is 20 s) holds up the
//! recipient it is serving, not the whole run and not the chat messages.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use lettre::{
    message::{header::ContentType, Mailbox},
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::Config,
    error::{AppError, AppResult},
    mattermost::{BotClient, ChatFailure, DirectMessenger},
};

/// Delivery attempts per channel before a notification is marked `failed`.
pub const MAX_ATTEMPTS: i32 = 5;
/// Pause between two runs of the background task.
const INTERVAL: Duration = Duration::from_secs(15);
/// Recipients served at the same time, i.e. the connections one run opens
/// to the mail server at most. Hosted mail services limit the simultaneous
/// connections of one sender to a handful (three is common), so this stays
/// small: enough that one slow delivery does not hold up the others.
pub const PARALLEL_RECIPIENTS: usize = 3;

/// A notification with a pending channel, and what its delivery needs.
#[derive(sqlx::FromRow)]
struct Pending {
    id: Uuid,
    user_id: Uuid,
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
    generic: bool,
}

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
    chat: Option<Chat>,
}

/// How Mattermost is reached.
#[derive(Clone)]
enum Chat {
    /// Bot account, REST API: direct messages only.
    Bot(Arc<dyn DirectMessenger>),
    /// Deprecated incoming webhook (`direct`: `"channel": "@user"`).
    Webhook(Arc<dyn ChatSender>, bool),
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
            Some(m) => match (&m.bot, &m.webhook) {
                (Some(bot), webhook) => {
                    if webhook.is_some() {
                        tracing::warn!(
                            "TEAMCRAFT_MATTERMOST_WEBHOOK is ignored: notifications go out as direct messages of the bot"
                        );
                    }
                    let client = BotClient::new(&bot.url, &bot.token)?;
                    Some(Chat::Bot(Arc::new(client) as Arc<dyn DirectMessenger>))
                }
                (None, Some(webhook)) => {
                    tracing::warn!(
                        "TEAMCRAFT_MATTERMOST_WEBHOOK is deprecated: it posts into a channel or as the webhook's creator. Create a bot account and set TEAMCRAFT_MATTERMOST_URL and TEAMCRAFT_MATTERMOST_BOT_TOKEN (docs: admin/upgrade)"
                    );
                    let http = reqwest::Client::builder()
                        .timeout(Duration::from_secs(10))
                        .build()?;
                    let sender = WebhookSender {
                        http,
                        url: webhook.clone(),
                    };
                    Some(Chat::Webhook(
                        Arc::new(sender) as Arc<dyn ChatSender>,
                        m.direct,
                    ))
                }
                (None, None) => None,
            },
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
            chat: chat.map(|(sender, direct)| Chat::Webhook(sender, direct)),
        })
    }

    /// Delivery whose Mattermost is a bot with a fake API (tests).
    pub fn with_bot(
        public_url: &str,
        instance_name: &str,
        bot: Arc<dyn DirectMessenger>,
    ) -> Delivery {
        Delivery {
            public_url: public_url.trim_end_matches('/').to_string(),
            instance_name: instance_name.to_string(),
            mail: None,
            chat: Some(Chat::Bot(bot)),
        }
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
        // Neither to a recipient deactivated since (deactivation keeps the
        // memberships, but the account reads nothing any more).
        sqlx::query(&format!(
            "UPDATE notifications n
             SET email_state = CASE WHEN n.email_state = 'pending' THEN 'skipped' ELSE n.email_state END,
                 chat_state = CASE WHEN n.chat_state = 'pending' THEN 'skipped' ELSE n.chat_state END
             FROM users u
             WHERE u.id = n.user_id AND (n.email_state = 'pending' OR n.chat_state = 'pending')
               AND (NOT u.active OR NOT {})",
            crate::services::notifications::notification_visible(
                "n",
                "u.id",
                &crate::services::access::hidden_of_user("u")
            )
        ))
        .execute(pool)
        .await?;
        // `confidential` is evaluated now: a group restricted after the row
        // was queued keeps the text out of a shared channel.
        let rows = sqlx::query_as::<_, Pending>(&format!(
            "SELECT n.id, n.user_id, n.title, n.body, n.link, n.email_state, n.chat_state,
                    n.delivery_attempts,
                    u.email, u.display_name, COALESCE(u.mattermost_username, u.username) AS chat_user,
                    {} AS confidential, {} AS generic
             FROM notifications n JOIN users u ON u.id = n.user_id
             WHERE (n.email_state = 'pending' OR n.chat_state = 'pending') AND u.active
             ORDER BY n.created_at LIMIT 100",
            crate::services::notifications::restricted_now_sql("n"),
            crate::services::notifications::generic_text_sql("n")
        ))
        .fetch_all(pool)
        .await?;
        let name = crate::services::branding::current(pool)
            .await?
            .name(&self.instance_name)
            .to_string();
        // One line per recipient, in the order the notifications were queued:
        // a user reads theirs in order, and the recipients are served side by
        // side, so a slow mail server holds up one line, not all of them.
        let mut lines: Vec<Vec<Pending>> = Vec::new();
        let mut line_of: HashMap<Uuid, usize> = HashMap::new();
        for row in rows {
            let at = *line_of.entry(row.user_id).or_insert(lines.len());
            match lines.get_mut(at) {
                Some(line) => line.push(row),
                None => lines.push(vec![row]),
            }
        }
        let mut lines = lines.into_iter();
        let mut tasks = tokio::task::JoinSet::new();
        let mut report = Report::default();
        let mut failed: Option<AppError> = None;
        loop {
            // after a failure (the database) no further line is started
            while failed.is_none() && tasks.len() < PARALLEL_RECIPIENTS {
                let Some(line) = lines.next() else { break };
                let (delivery, pool, name) = (self.clone(), pool.clone(), name.clone());
                tasks.spawn(async move {
                    let mut report = Report::default();
                    for row in line {
                        delivery.deliver(&pool, &name, row, &mut report).await?;
                    }
                    Ok::<_, AppError>(report)
                });
            }
            // a line that was started is finished: what it sent is recorded
            let Some(done) = tasks.join_next().await else {
                break;
            };
            match done {
                Ok(Ok(line)) => {
                    report.emails += line.emails;
                    report.chats += line.chats;
                    report.failures += line.failures;
                }
                Ok(Err(e)) => failed = failed.or(Some(e)),
                Err(e) => {
                    failed = failed.or(Some(AppError::Other(anyhow::anyhow!(
                        "notification delivery task failed: {e}"
                    ))));
                }
            }
        }
        match failed {
            Some(e) => Err(e),
            None => Ok(report),
        }
    }

    /// Sends one notification on its pending channels – e-mail and chat at
    /// the same time, they do not depend on each other – and records the
    /// outcome.
    async fn deliver(
        &self,
        pool: &PgPool,
        name: &str,
        mut row: Pending,
        report: &mut Report,
    ) -> AppResult<()> {
        // a group that switched `notification_content` off: generic text
        // and the link only, in e-mail and chat (direct messages too)
        if row.generic {
            (row.title, row.body) = generic_text(&self.instance_name);
        }
        let url = format!("{}{}", self.public_url, row.link);
        let mail = async {
            if row.email_state != "pending" {
                return None;
            }
            Some(
                self.send_mail(
                    name,
                    &row.title,
                    &row.body,
                    &url,
                    row.email.as_deref(),
                    &row.display_name,
                )
                .await,
            )
        };
        let chat = async {
            if row.chat_state != "pending" {
                return None;
            }
            Some(
                self.post_chat(
                    name,
                    &row.title,
                    &row.body,
                    &url,
                    &row.chat_user,
                    row.confidential,
                )
                .await,
            )
        };
        let (mailed, posted) = tokio::join!(mail, chat);
        let mut errors = Vec::new();
        let mut email_state = row.email_state.clone();
        match mailed {
            Some(Ok(())) => {
                email_state = "sent".into();
                report.emails += 1;
            }
            Some(Err(e)) => errors.push(format!("e-mail: {e}")),
            None => {}
        }
        let mut chat_state = row.chat_state.clone();
        let mut permanent_chat = false;
        match posted {
            Some(Ok(())) => {
                chat_state = "sent".into();
                report.chats += 1;
            }
            Some(Err(e)) => {
                // an unknown or deactivated user does not appear by retrying
                permanent_chat = e.permanent;
                errors.push(format!("Mattermost: {e}"));
            }
            None => {}
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
            if permanent_chat && chat_state == "pending" {
                chat_state = "failed".into();
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
        Ok(())
    }

    async fn send_mail(
        &self,
        instance_name: &str,
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
            .subject(format!("[{}] {title}", instance_name))
            .header(ContentType::TEXT_PLAIN)
            .body(email_text(
                instance_name,
                &self.public_url,
                title,
                body,
                url,
            ))?;
        sender.send(message).await
    }

    async fn post_chat(
        &self,
        instance_name: &str,
        title: &str,
        body: &str,
        url: &str,
        user: &str,
        confidential: bool,
    ) -> Result<(), ChatFailure> {
        match self.chat.as_ref() {
            None => Err(ChatFailure::transient("Mattermost is not configured")),
            // a direct message is read by its recipient only: full text
            Some(Chat::Bot(bot)) => {
                bot.send(user, &crate::mattermost::dm_text(title, body, url))
                    .await
            }
            Some(Chat::Webhook(sender, direct)) => {
                let (title, body) = chat_text(instance_name, title, body, confidential, *direct);
                sender
                    .post(chat_payload(
                        instance_name,
                        &title,
                        &body,
                        url,
                        user,
                        *direct,
                    ))
                    .await?;
                Ok(())
            }
        }
    }
}

/// Title and body of a notification whose group switched
/// `notification_content` off: nothing about the object, only that there is
/// something and (added by the sender) the link.
pub fn generic_text(instance: &str) -> (String, String) {
    (
        format!("new {instance} notification"),
        "Open the link to read it (sign-in required).".to_string(),
    )
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
