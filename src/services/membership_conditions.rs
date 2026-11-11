//! Need-to-know project groups, step 2 (issue #151): conditions on
//! membership. A restricted group with a need-to-know policy can require,
//! on top of membership,
//!
//! * **claims** of the identity provider – `site = main-office,
//!   partner-workshop`, `nda = true` – taken from the user's last single
//!   sign-on (a change at the identity provider applies at the next sign-in;
//!   local accounts have no claims and never meet such a condition);
//! * **networks** – the client address must be in one of the ranges (the
//!   same client address as the sign-in throttle: the peer, or the entry the
//!   reverse proxy added to `X-Forwarded-For`);
//! * a **maximum sign-in age** – the group is visible only for so many hours
//!   after signing in (the web session's start; for tokens and
//!   notifications the user's last sign-in), so a person removed at the
//!   identity provider loses it at the latest then.
//!
//! A reader who does not meet the conditions is treated as a non-member: the
//! group is hidden ("not found"), administrators included. Claims and the
//! sign-in age are part of the SQL that builds the hidden groups
//! ([`unmet_sql`] in `services::access::hidden_of_user`), so they also hold
//! for tokens, the assistant and notification recipients; networks and the
//! session's age depend on the request and are applied by the extractors
//! ([`apply`]). A member refused by a condition is recorded once per session
//! (`project.access_refused`).

use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    sync::{LazyLock, Mutex},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::ProjectVisibility,
    error::{AppError, AppResult},
    services::{access, audit, project_policy, projects},
};

/// Most claim conditions of a group.
pub const MAX_CLAIMS: usize = 10;
/// Most accepted values of one claim condition.
pub const MAX_VALUES: usize = 20;
/// Most network ranges of a group.
pub const MAX_NETWORKS: usize = 50;
/// Longest maximum sign-in age (a year).
pub const MAX_SIGNIN_AGE_HOURS: i32 = 8760;
/// Values of one claim kept from a sign-in.
const MAX_STORED_VALUES: usize = 50;
/// Longest kept claim value.
const MAX_VALUE_CHARS: usize = 200;

/// One claim condition: the claim must hold one of the values (compared
/// without case; a list claim such as `groups` needs one of its entries).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCondition {
    pub claim: String,
    pub values: Vec<String>,
}

/// The conditions of a group (none = membership alone, as before).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conditions {
    pub required_claims: Vec<ClaimCondition>,
    pub allowed_networks: Vec<String>,
    pub max_signin_age_hours: Option<i32>,
}

impl Conditions {
    pub fn is_empty(&self) -> bool {
        self.required_claims.is_empty()
            && self.allowed_networks.is_empty()
            && self.max_signin_age_hours.is_none()
    }
}

// ---------------------------------------------------------------------------
// Networks
// ---------------------------------------------------------------------------

/// An address range (`10.0.0.0/8`, `2001:db8::/32`; a bare address is one
/// host).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl Network {
    pub fn parse(raw: &str) -> Result<Network, String> {
        let raw = raw.trim();
        let (addr, prefix) = match raw.split_once('/') {
            Some((a, p)) => (a.trim(), Some(p.trim())),
            None => (raw, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("'{raw}' is not an address range such as 10.0.0.0/8."))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("'{raw}': the prefix length is 0 to {max}."))?,
        };
        let net = Network { addr, prefix };
        if net.masked() != addr {
            return Err(format!(
                "'{raw}' has host bits set; the range starts at {}/{prefix}.",
                net.masked()
            ));
        }
        Ok(net)
    }

    fn masked(&self) -> IpAddr {
        match self.addr {
            IpAddr::V4(a) => {
                let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
                IpAddr::V4((u32::from(a) & mask).into())
            }
            IpAddr::V6(a) => {
                let mask = u128::MAX.checked_shl(128 - self.prefix as u32).unwrap_or(0);
                IpAddr::V6((u128::from(a) & mask).into())
            }
        }
    }

    /// Whether `ip` is in the range (an IPv4 address mapped into IPv6
    /// counts as IPv4).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - self.prefix as u32).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }

    /// Whether every address of `other` is in this range.
    fn covers(&self, other: &Network) -> bool {
        self.addr.is_ipv4() == other.addr.is_ipv4()
            && other.prefix >= self.prefix
            && self.contains(other.addr)
    }
}

impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Whether the client is in one of the ranges; an unknown client is not.
pub fn network_allows(networks: &[String], client: Option<IpAddr>) -> bool {
    let Some(ip) = client else { return false };
    networks
        .iter()
        .filter_map(|n| Network::parse(n).ok())
        .any(|n| n.contains(ip))
}

// ---------------------------------------------------------------------------
// Parsing and validation
// ---------------------------------------------------------------------------

fn valid_claim_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/'))
}

/// A claim value as it is compared: trimmed, lower case, bounded.
fn normal_value(v: &str) -> String {
    v.trim()
        .to_lowercase()
        .chars()
        .take(MAX_VALUE_CHARS)
        .collect()
}

/// Checks and normalises conditions: claim names are identifiers, values
/// are lower case, networks are canonical ranges, the sign-in age is 1 to
/// [`MAX_SIGNIN_AGE_HOURS`] hours.
pub fn validate(c: Conditions) -> AppResult<Conditions> {
    let bad = |m: String| AppError::BadRequest(m);
    if c.required_claims.len() > MAX_CLAIMS {
        return Err(bad(format!("At most {MAX_CLAIMS} claim conditions.")));
    }
    let mut claims: Vec<ClaimCondition> = Vec::new();
    for cond in c.required_claims {
        let claim = cond.claim.trim().to_string();
        if !valid_claim_name(&claim) {
            return Err(bad(format!(
                "'{claim}' is not a claim name (letters, digits, _ - . : /)."
            )));
        }
        let mut values: Vec<String> = cond
            .values
            .iter()
            .map(|v| normal_value(v))
            .filter(|v| !v.is_empty())
            .collect();
        values.sort();
        values.dedup();
        if values.is_empty() || values.len() > MAX_VALUES {
            return Err(bad(format!(
                "The claim '{claim}' needs 1 to {MAX_VALUES} accepted values."
            )));
        }
        if claims.iter().any(|c| c.claim == claim) {
            return Err(bad(format!("The claim '{claim}' is named twice.")));
        }
        claims.push(ClaimCondition { claim, values });
    }
    if c.allowed_networks.len() > MAX_NETWORKS {
        return Err(bad(format!("At most {MAX_NETWORKS} address ranges.")));
    }
    let mut networks: Vec<String> = Vec::new();
    for raw in &c.allowed_networks {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let net = Network::parse(raw).map_err(bad)?.to_string();
        if !networks.contains(&net) {
            networks.push(net);
        }
    }
    if let Some(h) = c.max_signin_age_hours {
        if !(1..=MAX_SIGNIN_AGE_HOURS).contains(&h) {
            return Err(bad(format!(
                "The maximum sign-in age is 1 to {MAX_SIGNIN_AGE_HOURS} hours."
            )));
        }
    }
    Ok(Conditions {
        required_claims: claims,
        allowed_networks: networks,
        max_signin_age_hours: c.max_signin_age_hours,
    })
}

/// Claim conditions from text, one per line: `claim = value, value`.
pub fn parse_claim_lines(text: &str) -> AppResult<Vec<ClaimCondition>> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|line| {
            let (claim, values) = line.split_once('=').ok_or_else(|| {
                AppError::BadRequest(format!(
                    "'{line}': write a condition as claim = value, value."
                ))
            })?;
            Ok(ClaimCondition {
                claim: claim.trim().to_string(),
                values: values.split(',').map(|v| v.trim().to_string()).collect(),
            })
        })
        .collect()
}

/// Claim conditions as text lines (the form field).
pub fn claim_lines(claims: &[ClaimCondition]) -> String {
    claims
        .iter()
        .map(|c| format!("{} = {}", c.claim, c.values.join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `new` asks at least as much as `old` (every old claim condition
/// kept with no new accepted value, every new range inside an old one, a
/// sign-in age no longer than before). Anything else loosens the group.
pub fn at_least_as_strict(new: &Conditions, old: &Conditions) -> bool {
    let claims = old.required_claims.iter().all(|o| {
        new.required_claims
            .iter()
            .any(|n| n.claim == o.claim && n.values.iter().all(|v| o.values.contains(v)))
    });
    let networks = old.allowed_networks.is_empty()
        || (!new.allowed_networks.is_empty()
            && new.allowed_networks.iter().all(|n| {
                let Ok(n) = Network::parse(n) else {
                    return false;
                };
                old.allowed_networks
                    .iter()
                    .filter_map(|o| Network::parse(o).ok())
                    .any(|o| o.covers(&n))
            }));
    let age = match (old.max_signin_age_hours, new.max_signin_age_hours) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(o), Some(n)) => n <= o,
    };
    claims && networks && age
}

/// The claims of a sign-in as they are stored: names to lower-case values
/// (a string, a number, a boolean or a list of them; other shapes are left
/// out), only for the claims in `wanted`.
pub fn claims_to_store(
    claims: &BTreeMap<String, Vec<String>>,
    wanted: &[String],
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for name in wanted {
        if let Some(values) = claims.get(name) {
            let values: Vec<String> = values
                .iter()
                .map(|v| normal_value(v))
                .filter(|v| !v.is_empty())
                .take(MAX_STORED_VALUES)
                .collect();
            out.insert(name.clone(), json!(values));
        }
    }
    serde_json::Value::Object(out)
}

/// The claims of an ID token (and userinfo) as name → values.
pub fn claim_values(claims: &serde_json::Value) -> BTreeMap<String, Vec<String>> {
    use serde_json::Value;
    let scalar = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let mut out = BTreeMap::new();
    if let Value::Object(map) = claims {
        for (k, v) in map {
            let values: Vec<String> = match v {
                Value::Array(list) => list.iter().filter_map(scalar).collect(),
                other => scalar(other).into_iter().collect(),
            };
            if !values.is_empty() {
                out.insert(k.clone(), values);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

/// SQL: the conditions of the policy row `pp` that the user aliased `u`
/// does not meet with their stored claims and last sign-in (`TRUE` = not
/// met). Without conditions (or without a policy row) it is `FALSE`.
pub fn unmet_sql(u: &str, pp: &str) -> String {
    format!(
        "COALESCE((jsonb_array_length({pp}.required_claims) > 0
                   AND EXISTS (SELECT 1 FROM jsonb_array_elements({pp}.required_claims) mrc
                               WHERE NOT (COALESCE({u}.idp_claims -> (mrc->>'claim'), '[]'::jsonb)
                                          ?| ARRAY(SELECT jsonb_array_elements_text(mrc->'values')))))
                  OR ({pp}.max_signin_age_hours IS NOT NULL
                      AND ({u}.last_login_at IS NULL
                           OR {u}.last_login_at
                              < now() - make_interval(hours => {pp}.max_signin_age_hours))),
                  FALSE)"
    )
}

/// The conditions of a group (empty without a policy row).
pub async fn of(conn: &mut PgConnection, project_id: Uuid) -> AppResult<Conditions> {
    let row: Option<(serde_json::Value, Vec<String>, Option<i32>)> = sqlx::query_as(
        "SELECT required_claims, allowed_networks, max_signin_age_hours
         FROM project_policies WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        None => Conditions::default(),
        Some((claims, networks, age)) => Conditions {
            required_claims: serde_json::from_value(claims).unwrap_or_default(),
            allowed_networks: networks,
            max_signin_age_hours: age,
        },
    })
}

/// The claim names any group's conditions name: the claims kept from a
/// sign-in.
pub async fn wanted_claims(conn: &mut PgConnection) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT c->>'claim' FROM project_policies pp,
                jsonb_array_elements(pp.required_claims) c
         WHERE c->>'claim' IS NOT NULL ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// Stores the claims of a single sign-on that conditions name (called by
/// `users::upsert_oidc_user` in its transaction).
pub(crate) async fn store_claims(
    conn: &mut PgConnection,
    user_id: Uuid,
    claims: &BTreeMap<String, Vec<String>>,
) -> AppResult<()> {
    let wanted = wanted_claims(conn).await?;
    sqlx::query("UPDATE users SET idp_claims = $2, idp_claims_at = now() WHERE id = $1")
        .bind(user_id)
        .bind(claims_to_store(claims, &wanted))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per request
// ---------------------------------------------------------------------------

/// Where a request comes from, for the conditions that depend on it.
#[derive(Clone, Debug, Default)]
pub struct Origin {
    /// The client address (see `auth::client_address`).
    pub client: Option<IpAddr>,
    /// Start of the web session; `None` for tokens (their age is the
    /// user's last sign-in, checked in SQL).
    pub session_started: Option<DateTime<Utc>>,
    /// The session or token (a hash prefix) – refusals are recorded once
    /// per key. `None` records nothing.
    pub key: Option<String>,
    /// The request came with a personal access token (the `api_tokens`
    /// switch applies to work done for it).
    pub token: bool,
}

#[derive(sqlx::FromRow)]
struct Conditioned {
    project_id: Uuid,
    code: String,
    allowed_networks: Vec<String>,
    stored_unmet: bool,
    member: bool,
    session_too_old: bool,
}

/// Refusals already recorded in this process: (key, group).
static RECORDED: LazyLock<Mutex<HashSet<(String, Uuid)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
const MAX_RECORDED: usize = 10_000;

/// The actor as the request may see it: groups whose network or session-age
/// condition the request does not meet are hidden (claims and the last
/// sign-in are already part of the actor's hidden groups). One query; without
/// conditioned groups nothing changes. A member refused by any condition is
/// recorded once per `origin.key`.
pub async fn apply(pool: &PgPool, actor: Actor, origin: &Origin) -> AppResult<Actor> {
    if actor.id.is_nil() {
        return Ok(actor);
    }
    let rows = sqlx::query_as::<_, Conditioned>(&format!(
        "SELECT p.id AS project_id, p.code, pp.allowed_networks,
                {stored} AS stored_unmet,
                EXISTS (SELECT 1 FROM project_members pm
                        WHERE pm.project_id = p.id AND pm.user_id = u.id) AS member,
                COALESCE(pp.max_signin_age_hours IS NOT NULL AND $2::timestamptz IS NOT NULL
                         AND $2 < now() - make_interval(hours => pp.max_signin_age_hours), FALSE)
                    AS session_too_old
         FROM project_policies pp
         JOIN projects p ON p.id = pp.project_id
         JOIN users u ON u.id = $1
         WHERE p.visibility = 'restricted'
           AND (jsonb_array_length(pp.required_claims) > 0
                OR cardinality(pp.allowed_networks) > 0
                OR pp.max_signin_age_hours IS NOT NULL)",
        stored = unmet_sql("u", "pp")
    ))
    .bind(actor.id)
    .bind(origin.session_started)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(actor);
    }
    let mut hide = Vec::new();
    let mut refused = Vec::new();
    for r in &rows {
        let network =
            !r.allowed_networks.is_empty() && !network_allows(&r.allowed_networks, origin.client);
        if network || r.session_too_old {
            hide.push(r.project_id);
        }
        if r.member && (network || r.session_too_old || r.stored_unmet) {
            let reason = if network {
                "network"
            } else if r.session_too_old {
                "session_age"
            } else {
                "claims_or_sign_in_age"
            };
            refused.push((r.project_id, r.code.clone(), reason));
        }
    }
    if let Some(key) = &origin.key {
        record_refusals(pool, &actor, key, origin.client, &refused).await?;
    }
    Ok(if hide.is_empty() {
        actor
    } else {
        actor.without_groups(&hide)
    })
}

/// Records `project.access_refused` for refusals not yet recorded for this
/// key (in this process).
async fn record_refusals(
    pool: &PgPool,
    actor: &Actor,
    key: &str,
    client: Option<IpAddr>,
    refused: &[(Uuid, String, &str)],
) -> AppResult<()> {
    let fresh: Vec<&(Uuid, String, &str)> = {
        let mut seen = RECORDED.lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() >= MAX_RECORDED {
            seen.clear();
        }
        refused
            .iter()
            .filter(|(id, _, _)| seen.insert((key.to_string(), *id)))
            .collect()
    };
    if fresh.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for (id, code, reason) in fresh {
        audit::record(
            &mut tx,
            Some(actor),
            "project.access_refused",
            "project",
            Some(*id),
            format!(
                "{} does not meet the conditions of {code} ({reason}): the group is hidden",
                actor.username
            ),
            json!({ "reason": reason, "client": client.map(|c| c.to_string()) }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Setting the conditions
// ---------------------------------------------------------------------------

/// A change of the conditions; `None` leaves a part as it is.
#[derive(Clone, Debug, Default)]
pub struct ConditionsUpdate {
    pub required_claims: Option<Vec<ClaimCondition>>,
    pub allowed_networks: Option<Vec<String>>,
    /// `Some(None)` removes the maximum sign-in age.
    pub max_signin_age_hours: Option<Option<i32>>,
}

/// Sets the conditions on membership of a restricted group (managers and
/// administrators).
///
/// # Invariants
///
/// * **Permission:** the actor manages the group (403; "not found" for a
///   group the actor may not know); the group must be restricted (409).
///   Tightening is open to managers and administrators; loosening (a
///   condition removed, a value or range added, a longer or no sign-in age)
///   is for the managers while administrators are limited by the policy, like
///   `project_policy::set_access`.
/// * **Locks:** the group row (`FOR UPDATE`).
/// * **Data:** validated by [`validate`] (400). A member of the group may not
///   set conditions that their own request would fail (409: they would lock
///   themselves out). No change, no audit entry.
/// * **Audit:** `project.conditions` with the old and the new conditions.
/// * **Notifications / webhooks:** the other managers get an access alert; no
///   webhooks.
/// * **Atomic:** one transaction.
pub async fn set(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    update: ConditionsUpdate,
    origin: &Origin,
) -> AppResult<Conditions> {
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, (String, String)>(
        "SELECT code, visibility FROM projects WHERE id = $1 FOR UPDATE",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(access::not_found_project)?;
    projects::require_manager(&mut tx, actor, project_id).await?;
    if project.1 != ProjectVisibility::Restricted.as_str() {
        return Err(AppError::Conflict(format!(
            "{} is not restricted: restrict the group first, the conditions build on that.",
            project.0
        )));
    }
    let old = of(&mut tx, project_id).await?;
    let new = validate(Conditions {
        required_claims: update
            .required_claims
            .unwrap_or_else(|| old.required_claims.clone()),
        allowed_networks: update
            .allowed_networks
            .unwrap_or_else(|| old.allowed_networks.clone()),
        max_signin_age_hours: update
            .max_signin_age_hours
            .unwrap_or(old.max_signin_age_hours),
    })?;
    if new == old {
        tx.rollback().await?;
        return Ok(old);
    }
    if !at_least_as_strict(&new, &old) {
        project_policy::require_member_manager_under_policy(
            &mut tx,
            actor,
            project_id,
            "Loosening the conditions on membership",
        )
        .await?;
    }
    lockout_check(&mut tx, actor, project_id, &new, origin).await?;
    sqlx::query(
        "INSERT INTO project_policies (project_id, required_claims, allowed_networks,
                                       max_signin_age_hours, created_by, updated_by)
         VALUES ($1, $2, $3, $4, $5, $5)
         ON CONFLICT (project_id) DO UPDATE
         SET required_claims = EXCLUDED.required_claims,
             allowed_networks = EXCLUDED.allowed_networks,
             max_signin_age_hours = EXCLUDED.max_signin_age_hours,
             updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(project_id)
    .bind(json!(new.required_claims))
    .bind(&new.allowed_networks)
    .bind(new.max_signin_age_hours)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.conditions",
        "project",
        Some(project_id),
        format!("Conditions on membership of {} changed", project.0),
        json!({ "old": old, "new": new }),
    )
    .await?;
    project_policy::alert_managers(
        &mut tx,
        actor,
        project_id,
        format!("Conditions on membership of {} changed", project.0),
        format!(
            "{} changed the conditions on membership of {}.",
            actor.username, project.0
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(new)
}

/// Refuses conditions under which the acting member would no longer see the
/// group (administrators who are no members manage it anyway).
async fn lockout_check(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
    new: &Conditions,
    origin: &Origin,
) -> AppResult<()> {
    let member: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM project_members WHERE project_id = $1 AND user_id = $2)",
    )
    .bind(project_id)
    .bind(actor.id)
    .fetch_one(&mut *conn)
    .await?;
    if !member {
        return Ok(());
    }
    let mut problems = Vec::new();
    if !new.allowed_networks.is_empty() && !network_allows(&new.allowed_networks, origin.client) {
        problems.push(match origin.client {
            Some(ip) => format!("your address {ip} is in none of the ranges"),
            None => "your address is not known".to_string(),
        });
    }
    if !new.required_claims.is_empty() {
        let stored: serde_json::Value =
            sqlx::query_scalar("SELECT idp_claims FROM users WHERE id = $1")
                .bind(actor.id)
                .fetch_one(&mut *conn)
                .await?;
        for c in &new.required_claims {
            let mine: Vec<String> = stored
                .get(&c.claim)
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            if !mine.iter().any(|v| c.values.contains(v)) {
                problems.push(format!(
                    "your last single sign-on did not give the claim '{}' one of the values \
                     (sign in again after the condition is set elsewhere)",
                    c.claim
                ));
            }
        }
    }
    if let (Some(h), Some(started)) = (new.max_signin_age_hours, origin.session_started) {
        if started < Utc::now() - chrono::Duration::hours(h as i64) {
            problems.push(format!(
                "you signed in more than {h} hours ago (sign in again first)"
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::Conflict(format!(
            "These conditions would hide the group from you: {}.",
            problems.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(claim: &str, values: &[&str]) -> ClaimCondition {
        ClaimCondition {
            claim: claim.into(),
            values: values.iter().map(|v| v.to_string()).collect(),
        }
    }

    #[test]
    fn the_sql_fragments_are_balanced() {
        for sql in [
            unmet_sql("u", "pp"),
            crate::services::access::hidden_of_user("u"),
        ] {
            let mut depth = 0i32;
            for c in sql.chars() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0, "closes too early: {sql}");
            }
            assert_eq!(depth, 0, "{sql}");
        }
    }

    #[test]
    fn networks_parse_and_match() {
        let n = Network::parse("10.20.0.0/16").unwrap();
        assert!(n.contains("10.20.3.4".parse().unwrap()));
        assert!(!n.contains("10.21.0.1".parse().unwrap()));
        assert!(
            n.contains("::ffff:10.20.0.9".parse().unwrap()),
            "IPv4 mapped into IPv6"
        );
        assert!(!n.contains("2001:db8::1".parse().unwrap()));
        let host = Network::parse(" 192.0.2.7 ").unwrap();
        assert_eq!(host.to_string(), "192.0.2.7/32");
        assert!(host.contains("192.0.2.7".parse().unwrap()));
        assert!(!host.contains("192.0.2.8".parse().unwrap()));
        let v6 = Network::parse("2001:db8::/32").unwrap();
        assert!(v6.contains("2001:db8:ffff::1".parse().unwrap()));
        assert!(!v6.contains("2001:db9::1".parse().unwrap()));
        let all = Network::parse("0.0.0.0/0").unwrap();
        assert!(all.contains("203.0.113.1".parse().unwrap()));
        for bad in ["10.0.0.1/8", "10.0.0.0/33", "nope", "10.0.0.0/x", "::/129"] {
            assert!(Network::parse(bad).is_err(), "{bad}");
        }
        assert!(network_allows(
            &["10.0.0.0/8".into()],
            "10.1.1.1".parse().ok()
        ));
        assert!(
            !network_allows(&["10.0.0.0/8".into()], None),
            "unknown client"
        );
    }

    #[test]
    fn conditions_are_validated_and_normalised() {
        let c = validate(Conditions {
            required_claims: vec![cond(
                " site ",
                &["Main-Office", " partner ", "main-office", ""],
            )],
            allowed_networks: vec!["10.0.0.0/8".into(), " ".into(), "10.0.0.0/8".into()],
            max_signin_age_hours: Some(12),
        })
        .unwrap();
        assert_eq!(
            c.required_claims,
            vec![cond("site", &["main-office", "partner"])]
        );
        assert_eq!(c.allowed_networks, vec!["10.0.0.0/8".to_string()]);
        for bad in [
            Conditions {
                required_claims: vec![cond("site", &[" "])],
                ..Default::default()
            },
            Conditions {
                required_claims: vec![cond("bad name", &["x"])],
                ..Default::default()
            },
            Conditions {
                required_claims: vec![cond("a", &["x"]), cond("a", &["y"])],
                ..Default::default()
            },
            Conditions {
                allowed_networks: vec!["10.0.0.1/8".into()],
                ..Default::default()
            },
            Conditions {
                max_signin_age_hours: Some(0),
                ..Default::default()
            },
            Conditions {
                max_signin_age_hours: Some(MAX_SIGNIN_AGE_HOURS + 1),
                ..Default::default()
            },
        ] {
            assert!(matches!(validate(bad), Err(AppError::BadRequest(_))));
        }
    }

    #[test]
    fn claim_lines_round_trip() {
        let parsed = parse_claim_lines("site = a, b\n\n nda=true \n").unwrap();
        assert_eq!(
            parsed,
            vec![cond("site", &["a", "b"]), cond("nda", &["true"])]
        );
        assert_eq!(claim_lines(&parsed), "site = a, b\nnda = true");
        assert!(parse_claim_lines("site").is_err());
    }

    #[test]
    fn strictness_tells_tightening_from_loosening() {
        let base = Conditions {
            required_claims: vec![cond("site", &["a", "b"])],
            allowed_networks: vec!["10.0.0.0/8".into()],
            max_signin_age_hours: Some(24),
        };
        // fewer values, a narrower range, a shorter age, an extra condition
        let tighter = Conditions {
            required_claims: vec![cond("site", &["a"]), cond("nda", &["true"])],
            allowed_networks: vec!["10.1.0.0/16".into()],
            max_signin_age_hours: Some(8),
        };
        assert!(at_least_as_strict(&tighter, &base));
        assert!(at_least_as_strict(&base, &base));
        assert!(!at_least_as_strict(&base, &tighter));
        for looser in [
            Conditions {
                required_claims: vec![cond("site", &["a", "b", "c"])],
                ..base.clone()
            },
            Conditions {
                required_claims: vec![],
                ..base.clone()
            },
            Conditions {
                allowed_networks: vec![],
                ..base.clone()
            },
            Conditions {
                allowed_networks: vec!["192.168.0.0/16".into()],
                ..base.clone()
            },
            Conditions {
                max_signin_age_hours: None,
                ..base.clone()
            },
            Conditions {
                max_signin_age_hours: Some(48),
                ..base.clone()
            },
        ] {
            assert!(!at_least_as_strict(&looser, &base), "{looser:?}");
        }
        // anything is at least as strict as nothing
        assert!(at_least_as_strict(&base, &Conditions::default()));
    }

    #[test]
    fn only_the_named_claims_are_kept() {
        let claims = claim_values(&json!({
            "sub": "x", "site": "Main-Office", "nda": true, "level": 3,
            "groups": ["A", "B", 7, {"x": 1}], "address": {"street": "y"},
        }));
        assert_eq!(claims["nda"], vec!["true".to_string()]);
        assert_eq!(
            claims["groups"],
            vec!["A".to_string(), "B".into(), "7".into()]
        );
        assert!(!claims.contains_key("address"));
        let stored = claims_to_store(&claims, &["site".into(), "groups".into(), "missing".into()]);
        assert_eq!(
            stored,
            json!({ "site": ["main-office"], "groups": ["a", "b", "7"] })
        );
    }
}
