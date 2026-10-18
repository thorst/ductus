//! What the REST API reads and keeps: its tokens and sessions, the message browser, and queue counts.

use crate::audit::{self, Audit, By};
use crate::{DestinationId, Status, Store};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// What a token may do. Each role includes the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i16)]
pub enum Role {
    /// Read everything (status, messages, reports).
    Viewer = 0,
    /// Also act on the engine: drain and resume, connection checks.
    Operator = 1,
    /// Also manage tokens.
    Admin = 2,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Role::Viewer, Role::Operator, Role::Admin].into_iter().find(|r| r.label().eq_ignore_ascii_case(text.trim()))
    }
}

impl TryFrom<i16> for Role {
    type Error = String;
    fn try_from(v: i16) -> std::result::Result<Self, String> {
        Ok(match v {
            0 => Role::Viewer,
            1 => Role::Operator,
            2 => Role::Admin,
            _ => return Err(format!("unknown role {v}")),
        })
    }
}

/// Permissions a role doesn't say (admins have them all).
pub const PERMISSIONS: [&str; 2] = ["payloads", "audit"];

/// `list` as a sorted set of known permissions, or what is wrong with it.
pub fn clean_permissions(list: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for p in list {
        let p = p.trim().to_lowercase();
        if !PERMISSIONS.contains(&p.as_str()) {
            bail!("unknown permission `{p}` (known: {})", PERMISSIONS.join(", "));
        }
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

/// A token as stored (never the secret itself).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Token {
    pub id: i32,
    pub name: String,
    #[sqlx(try_from = "i16")]
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// The user it belongs to (none: a service token made by an admin).
    pub user_id: Option<i32>,
    pub permissions: Vec<String>,
    /// That user's name.
    pub owner: Option<String>,
}

/// Who is calling, whichever way they signed in: a user (password) or a token. A token that
/// belongs to a user acts as that user at most: never above their role or permissions.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: Option<i32>,
    pub token_id: Option<i32>,
    /// The username, or the token's name.
    pub name: String,
    pub role: Role,
    pub permissions: Vec<String>,
    /// Signed in with a password an admin chose: nothing works until it is changed.
    pub must_change_password: bool,
}

impl Principal {
    /// Admins may do everything; others only what they were given.
    pub fn can(&self, permission: &str) -> bool {
        self.role == Role::Admin || self.permissions.iter().any(|p| p == permission)
    }
}

/// How long sessions last (`auth.session`).
#[derive(Debug, Clone, Copy)]
pub struct SessionLimits {
    pub idle: std::time::Duration,
    pub max: std::time::Duration,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self { idle: std::time::Duration::from_secs(12 * 3600), max: std::time::Duration::from_secs(7 * 86_400) }
    }
}

/// Who a new session is for.
#[derive(Debug, Clone, Copy)]
pub enum SessionFor {
    User(i32),
    Token(i32),
}

/// Tokens start with this, so they're recognisable in a secret scanner or a leaked log.
pub const TOKEN_PREFIX: &str = "dux_";

/// Session cookies start with this (never accepted as a token, nor a token as a session).
pub const SESSION_PREFIX: &str = "dus_";

pub(crate) fn hash_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn hash(secret: &str) -> Vec<u8> {
    hash_secret(secret)
}

fn random_secret(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    format!("{prefix}{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

const TOKEN_COLUMNS: &str =
    "t.id, t.name, t.role, t.created_at, t.last_used_at, t.revoked_at, t.user_id, t.permissions,
    u.username AS owner";

/// What a token's or session's caller is made from: the token (if any) and the user (a session's
/// own, or a token's owner), as one row. A user the directory refused can't act, like a disabled one.
const WHO: &str =
    "t.id AS t_id, t.name AS t_name, t.role AS t_role, t.permissions AS t_perms, t.revoked_at AS t_revoked,
    t.last_used_at AS t_last_used, u.id AS u_id, u.username AS u_name, u.role AS u_role, u.permissions AS u_perms,
    COALESCE(u.disabled_at, u.directory_refused_at) AS u_disabled, u.must_change_password AS u_must";

#[derive(sqlx::FromRow)]
struct WhoRow {
    session_id: Option<i32>,
    session_user: Option<i32>,
    last_seen_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
    t_id: Option<i32>,
    t_name: Option<String>,
    t_role: Option<i16>,
    t_perms: Option<Vec<String>>,
    t_revoked: Option<DateTime<Utc>>,
    t_last_used: Option<DateTime<Utc>>,
    u_id: Option<i32>,
    u_name: Option<String>,
    u_role: Option<i16>,
    u_perms: Option<Vec<String>>,
    u_disabled: Option<DateTime<Utc>>,
    u_must: Option<bool>,
}

impl WhoRow {
    /// None when the account behind it can't act any more: a revoked token, a disabled user.
    fn principal(&self) -> Option<Principal> {
        let user_role = self.u_role.and_then(|r| Role::try_from(r).ok());
        if self.session_user.is_some() {
            // A user's own session.
            if self.u_disabled.is_some() {
                return None;
            }
            return Some(Principal {
                user_id: self.u_id,
                token_id: None,
                name: self.u_name.clone()?,
                role: user_role?,
                permissions: self.u_perms.clone().unwrap_or_default(),
                must_change_password: self.u_must.unwrap_or(false),
            });
        }
        if self.t_revoked.is_some() {
            return None;
        }
        let mut role = Role::try_from(self.t_role?).ok()?;
        let mut permissions = self.t_perms.clone().unwrap_or_default();
        if self.u_id.is_some() {
            // A user's token: the user must be able to act, and it never goes above them.
            if self.u_disabled.is_some() {
                return None;
            }
            let user_role = user_role?;
            role = role.min(user_role);
            if user_role != Role::Admin {
                let theirs = self.u_perms.clone().unwrap_or_default();
                permissions.retain(|p| theirs.contains(p));
            }
        }
        Some(Principal {
            user_id: self.u_id,
            token_id: self.t_id,
            name: self.t_name.clone()?,
            role,
            permissions,
            must_change_password: false,
        })
    }
}

/// A message as listed by the browser (no payload).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MessageRow {
    pub id: i64,
    pub received_at: DateTime<Utc>,
    pub interface: String,
    #[sqlx(try_from = "i16")]
    pub status: Status,
    pub ordering_key: Option<String>,
    pub control_id: Option<String>,
    /// Payload size in bytes.
    pub size: i32,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// Routed from another interface: the id of the message there.
    pub origin_id: Option<i64>,
}

/// Which messages to list: an interface's, newest first, optionally of one status, older than
/// `before` (the last id of the previous page).
#[derive(Debug, Clone)]
pub struct Browse<'a> {
    pub interface: &'a str,
    pub status: Option<Status>,
    pub before: Option<i64>,
    pub limit: i64,
}

/// One destination's queue right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct QueueCount {
    pub destination_id: DestinationId,
    pub pending: i64,
    pub quarantined: i64,
}

/// One lane of a destination's queue: how many wait, and the first of them.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LaneRow {
    pub lane: i32,
    pub waiting: i64,
    pub head_id: i64,
    pub received_at: DateTime<Utc>,
    pub attempts: i32,
    pub last_error: Option<String>,
    /// When the head was last changed (its last failed attempt, when it has any).
    pub updated_at: DateTime<Utc>,
    pub ordering_key: Option<String>,
}

const MESSAGE_COLUMNS: &str = "id, received_at, interface, status, ordering_key, control_id,
    octet_length(payload) AS size, last_error, updated_at, origin_id";

impl Store {
    /// A new token with `name` (no other live token may have it), for a user or (`user_id` none) as
    /// a service token. Returns the token and its secret, which is never stored and can't be
    /// shown again.
    pub async fn create_token(
        &self,
        name: &str,
        role: Role,
        permissions: &[String],
        user_id: Option<i32>,
        audit: &Audit<'_>,
    ) -> Result<(Token, String)> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a token needs a name (who or what uses it, e.g. `ci` or `monitoring`)");
        }
        let permissions = clean_permissions(permissions)?;
        let secret = random_secret(TOKEN_PREFIX);
        let mut tx = self.pool.begin().await.context("creating token")?;
        let sql = format!(
            "WITH t AS (
                 INSERT INTO api_tokens (name, role, hash, permissions, user_id) VALUES ($1, $2, $3, $4, $5)
                 RETURNING id, name, role, created_at, last_used_at, revoked_at, user_id, permissions
             )
             SELECT {TOKEN_COLUMNS} FROM t LEFT JOIN users u ON u.id = t.user_id"
        );
        let token = sqlx::query_as::<_, Token>(sqlx::AssertSqlSafe(sql))
            .bind(name)
            .bind(role as i16)
            .bind(hash(&secret))
            .bind(&permissions)
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await;
        match token {
            Ok(token) => {
                let role = role.label();
                let detail = serde_json::json!({ "role": role, "permissions": permissions });
                audit::insert(&mut tx, &Audit { subject: Some(&token.name), detail: Some(detail), ..audit.clone() })
                    .await?;
                tx.commit().await.context("creating token")?;
                Ok((token, secret))
            }
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                bail!("a token named `{name}` already exists (revoke it first, or pick another name)")
            }
            Err(e) => Err(e).context("creating token"),
        }
    }

    /// Tokens by name; revoked ones only if asked; only `user_id`'s if given.
    pub async fn tokens(&self, revoked_too: bool, user_id: Option<i32>) -> Result<Vec<Token>> {
        let sql = format!(
            "SELECT {TOKEN_COLUMNS} FROM api_tokens t LEFT JOIN users u ON u.id = t.user_id
             WHERE ($1 OR t.revoked_at IS NULL) AND ($2::int IS NULL OR t.user_id = $2) ORDER BY t.name, t.id"
        );
        sqlx::query_as::<_, Token>(sqlx::AssertSqlSafe(sql))
            .bind(revoked_too)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .context("reading tokens")
    }

    /// Revoke the live token named `name` (only if it is `user_id`'s, when given). False if
    /// there's none.
    pub async fn revoke_token(&self, name: &str, user_id: Option<i32>, audit: &Audit<'_>) -> Result<bool> {
        let mut tx = self.pool.begin().await.context("revoking token")?;
        let result = sqlx::query(
            "UPDATE api_tokens SET revoked_at = now()
             WHERE name = $1 AND revoked_at IS NULL AND ($2::int IS NULL OR user_id = $2)",
        )
        .bind(name.trim())
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .context("revoking token")?;
        if result.rows_affected() == 0 {
            return Ok(false);
        }
        audit::insert(&mut tx, &Audit { subject: Some(name.trim()), ..audit.clone() }).await?;
        tx.commit().await.context("revoking token")?;
        Ok(true)
    }

    /// Who the live token with this secret is, if anyone. Notes when it was used, at most once a
    /// minute per token (a write per API call would be wasted work).
    pub async fn authenticate(&self, secret: &str) -> Result<Option<Principal>> {
        if !secret.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let sql = format!(
            "SELECT NULL::int4 AS session_id, NULL::int4 AS session_user, NULL::timestamptz AS last_seen_at,
                    NULL::timestamptz AS expires_at, {WHO}
             FROM api_tokens t LEFT JOIN users u ON u.id = t.user_id WHERE t.hash = $1"
        );
        let row = sqlx::query_as::<_, WhoRow>(sqlx::AssertSqlSafe(sql))
            .bind(hash(secret))
            .fetch_optional(&self.pool)
            .await
            .context("checking token")?;
        let Some(row) = row else { return Ok(None) };
        let Some(who) = row.principal() else { return Ok(None) };
        if row.t_last_used.is_none_or(|at| Utc::now() - at > chrono::TimeDelta::minutes(1)) {
            sqlx::query("UPDATE api_tokens SET last_used_at = now() WHERE id = $1")
                .bind(row.t_id)
                .execute(&self.pool)
                .await
                .context("noting token use")?;
        }
        Ok(Some(who))
    }

    /// Sign a browser in: a new session for `owner` (already authenticated). Returns the secret
    /// for the cookie, which is never stored. Expired sessions are cleared on the way.
    pub async fn create_session(
        &self,
        owner: SessionFor,
        limits: SessionLimits,
        audit: &Audit<'_>,
    ) -> Result<(String, DateTime<Utc>)> {
        let mut tx = self.pool.begin().await.context("creating session")?;
        sqlx::query(
            "DELETE FROM api_sessions
             WHERE expires_at < now() OR last_seen_at < now() - make_interval(secs => $1)",
        )
        .bind(limits.idle.as_secs_f64())
        .execute(&mut *tx)
        .await
        .context("clearing expired sessions")?;
        let secret = random_secret(SESSION_PREFIX);
        let (token_id, user_id) = match owner {
            SessionFor::Token(id) => (Some(id), None),
            SessionFor::User(id) => (None, Some(id)),
        };
        let expires_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO api_sessions (hash, token_id, user_id, expires_at)
             VALUES ($1, $2, $3, now() + make_interval(secs => $4))
             RETURNING expires_at",
        )
        .bind(hash(&secret))
        .bind(token_id)
        .bind(user_id)
        .bind(limits.max.as_secs_f64())
        .fetch_one(&mut *tx)
        .await
        .context("creating session")?;
        audit::insert(&mut tx, audit).await?;
        tx.commit().await.context("creating session")?;
        Ok((secret, expires_at))
    }

    /// Who is behind a live session (not expired, not idle too long, the user or token still able
    /// to act). Notes the use at most once a minute, which keeps an idle session's clock honest.
    pub async fn authenticate_session(
        &self,
        secret: &str,
        limits: SessionLimits,
    ) -> Result<Option<(Principal, DateTime<Utc>)>> {
        if !secret.starts_with(SESSION_PREFIX) {
            return Ok(None);
        }
        let sql = format!(
            "SELECT s.id AS session_id, s.user_id AS session_user, s.last_seen_at, s.expires_at, {WHO}
             FROM api_sessions s
             LEFT JOIN api_tokens t ON t.id = s.token_id
             LEFT JOIN users u ON u.id = COALESCE(s.user_id, t.user_id)
             WHERE s.hash = $1 AND s.expires_at > now() AND s.last_seen_at > now() - make_interval(secs => $2)"
        );
        let row = sqlx::query_as::<_, WhoRow>(sqlx::AssertSqlSafe(sql))
            .bind(hash(secret))
            .bind(limits.idle.as_secs_f64())
            .fetch_optional(&self.pool)
            .await
            .context("checking session")?;
        let Some(row) = row else { return Ok(None) };
        let Some(who) = row.principal() else { return Ok(None) };
        if row.last_seen_at.is_some_and(|at| Utc::now() - at > chrono::TimeDelta::minutes(1)) {
            sqlx::query("UPDATE api_sessions SET last_seen_at = now() WHERE id = $1")
                .bind(row.session_id)
                .execute(&self.pool)
                .await
                .context("noting session use")?;
        }
        Ok(row.expires_at.map(|ends| (who, ends)))
    }

    /// Sign out: the session stops working at once. False if there was none.
    pub async fn end_session(&self, secret: &str, ip: Option<std::net::IpAddr>) -> Result<bool> {
        let mut tx = self.pool.begin().await.context("ending session")?;
        // Who it was, for the audit log (a session belongs to a user or a token).
        type Ended = (Option<i32>, Option<String>, Option<i32>, Option<String>, Option<i32>);
        let who: Option<Ended> = sqlx::query_as(
            "DELETE FROM api_sessions s WHERE s.hash = $1
             RETURNING s.user_id, (SELECT username FROM users WHERE id = s.user_id), s.token_id,
                       (SELECT name FROM api_tokens WHERE id = s.token_id),
                       (SELECT user_id FROM api_tokens WHERE id = s.token_id)",
        )
        .bind(hash(secret))
        .fetch_optional(&mut *tx)
        .await
        .context("ending session")?;
        let Some((user, username, token, token_name, owner)) = who else { return Ok(false) };
        let by = match (user, token) {
            (Some(id), _) => By::user(id, username.as_deref().unwrap_or(""), "session", ip),
            (None, Some(id)) => By::token(id, owner, token_name.as_deref().unwrap_or(""), "session", ip),
            (None, None) => return Ok(true),
        };
        audit::insert(&mut tx, &Audit::new(by, "session.sign_out")).await?;
        tx.commit().await.context("ending session")?;
        Ok(true)
    }

    /// A page of an interface's messages, newest first.
    pub async fn browse(&self, b: &Browse<'_>) -> Result<Vec<MessageRow>> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE interface = $1 AND ($2::smallint IS NULL OR status = $2) AND ($3::bigint IS NULL OR id < $3)
             ORDER BY id DESC LIMIT $4"
        );
        sqlx::query_as::<_, MessageRow>(sqlx::AssertSqlSafe(sql))
            .bind(b.interface)
            .bind(b.status.map(|s| s as i16))
            .bind(b.before)
            .bind(b.limit)
            .fetch_all(&self.pool)
            .await
            .context("listing messages")
    }

    /// One message and its payload.
    pub async fn message(&self, id: i64) -> Result<Option<(MessageRow, Vec<u8>)>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            #[sqlx(flatten)]
            row: MessageRow,
            payload: Vec<u8>,
        }
        let sql = format!("SELECT {MESSAGE_COLUMNS}, payload FROM messages WHERE id = $1");
        let found = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("reading message")?;
        Ok(found.map(|r| (r.row, r.payload)))
    }

    /// Pending and quarantined deliveries per destination that has any. Both come from small
    /// partial indexes, so this stays quick however many messages are kept.
    pub async fn queue_counts(&self) -> Result<Vec<QueueCount>> {
        sqlx::query_as::<_, QueueCount>(
            "SELECT destination_id, sum(pending)::bigint AS pending, sum(quarantined)::bigint AS quarantined FROM (
                 SELECT destination_id, count(*) AS pending, 0 AS quarantined FROM deliveries WHERE status = 0 GROUP BY 1
                 UNION ALL
                 SELECT destination_id, 0, count(*) FROM deliveries WHERE status = 3 GROUP BY 1
             ) c GROUP BY 1 ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await
        .context("counting queues")
    }

    /// A destination's lanes with messages waiting, most first: the count and the first
    /// message of each (the one its worker handles next). The counts come from the queue index
    /// alone; then each head is looked up by its id (one probe per partition, as nothing else
    /// about it is known yet), so the cost grows with the backlog and the number of lanes with
    /// one, never with what was delivered.
    pub async fn lanes(&self, destination: DestinationId) -> Result<Vec<LaneRow>> {
        sqlx::query_as::<_, LaneRow>(
            "WITH lanes AS (
                 SELECT partition_key AS lane, count(*) AS waiting, min(message_id) AS head_id
                 FROM deliveries WHERE destination_id = $1 AND status = 0 GROUP BY partition_key
             )
             SELECT l.lane, l.waiting, l.head_id, d.received_at, d.attempts, d.last_error, d.updated_at, m.ordering_key
             FROM lanes l
             CROSS JOIN LATERAL (
                 SELECT received_at, attempts, last_error, updated_at FROM deliveries x
                 WHERE x.message_id = l.head_id AND x.destination_id = $1 LIMIT 1
             ) d
             LEFT JOIN LATERAL (
                 SELECT ordering_key FROM messages m WHERE m.id = l.head_id AND m.received_at = d.received_at LIMIT 1
             ) m ON true
             ORDER BY l.waiting DESC, l.lane",
        )
        .bind(destination)
        .fetch_all(&self.pool)
        .await
        .context("reading the queue's lanes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_parse_and_order() {
        assert_eq!(Role::parse(" Operator "), Some(Role::Operator));
        assert_eq!(Role::parse("root"), None);
        assert!(Role::Admin > Role::Operator && Role::Operator > Role::Viewer);
        assert_eq!(hash("dux_a").len(), 32);
    }
}
