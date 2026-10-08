//! What the REST API reads and keeps: its tokens, the message browser, and queue counts.

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
}

/// Tokens start with this, so they're recognisable in a secret scanner or a leaked log.
pub const TOKEN_PREFIX: &str = "dux_";

fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
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

const MESSAGE_COLUMNS: &str = "id, received_at, interface, status, ordering_key, control_id,
    octet_length(payload) AS size, last_error, updated_at";

impl Store {
    /// A new token with `name` (no other live token may have it). Returns the token and its
    /// secret, which is never stored and can't be shown again.
    pub async fn create_token(&self, name: &str, role: Role) -> Result<(Token, String)> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a token needs a name (who or what uses it, e.g. `ci` or `monitoring`)");
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("OS random number generator");
        let secret = format!("{TOKEN_PREFIX}{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let token = sqlx::query_as::<_, Token>(
            "INSERT INTO api_tokens (name, role, hash) VALUES ($1, $2, $3)
             RETURNING id, name, role, created_at, last_used_at, revoked_at",
        )
        .bind(name)
        .bind(role as i16)
        .bind(hash(&secret))
        .fetch_one(&self.pool)
        .await;
        match token {
            Ok(token) => Ok((token, secret)),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                bail!("a token named `{name}` already exists (revoke it first, or pick another name)")
            }
            Err(e) => Err(e).context("creating token"),
        }
    }

    /// Tokens by name; revoked ones only if asked.
    pub async fn tokens(&self, revoked_too: bool) -> Result<Vec<Token>> {
        sqlx::query_as::<_, Token>(
            "SELECT id, name, role, created_at, last_used_at, revoked_at FROM api_tokens
             WHERE $1 OR revoked_at IS NULL ORDER BY name, id",
        )
        .bind(revoked_too)
        .fetch_all(&self.pool)
        .await
        .context("reading tokens")
    }

    /// Revoke the live token named `name`. False if there's none.
    pub async fn revoke_token(&self, name: &str) -> Result<bool> {
        let result = sqlx::query("UPDATE api_tokens SET revoked_at = now() WHERE name = $1 AND revoked_at IS NULL")
            .bind(name.trim())
            .execute(&self.pool)
            .await
            .context("revoking token")?;
        Ok(result.rows_affected() > 0)
    }

    /// The live token with this secret, if any. Notes when it was used, at most once a minute
    /// per token (a write per API call would be wasted work).
    pub async fn authenticate(&self, secret: &str) -> Result<Option<Token>> {
        if !secret.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let token = sqlx::query_as::<_, Token>(
            "SELECT id, name, role, created_at, last_used_at, revoked_at FROM api_tokens
             WHERE hash = $1 AND revoked_at IS NULL",
        )
        .bind(hash(secret))
        .fetch_optional(&self.pool)
        .await
        .context("checking token")?;
        if let Some(t) = &token
            && t.last_used_at.is_none_or(|at| Utc::now() - at > chrono::TimeDelta::minutes(1))
        {
            sqlx::query("UPDATE api_tokens SET last_used_at = now() WHERE id = $1")
                .bind(t.id)
                .execute(&self.pool)
                .await
                .context("noting token use")?;
        }
        Ok(token)
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
