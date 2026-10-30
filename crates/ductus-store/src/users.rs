//! People: user accounts, their sign-in bookkeeping (wrong passwords, lockout), their sessions
//! and their settings. Passwords are hashed by the caller (`ductus_core::password`); the store
//! only keeps the hash.

use crate::Store;
use crate::api::{Role, clean_permissions};
use crate::audit::{self, Audit};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use std::time::Duration;

/// A user as shown (never the password hash).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i32,
    pub username: String,
    pub display_name: String,
    pub email: String,
    #[sqlx(try_from = "i16")]
    pub role: Role,
    pub permissions: Vec<String>,
    pub source: String,
    pub must_change_password: bool,
    pub disabled_at: Option<DateTime<Utc>>,
    pub locked_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_sign_in_at: Option<DateTime<Utc>>,
    /// A directory account: when the directory was last asked about them.
    pub directory_checked_at: Option<DateTime<Utc>>,
    /// A directory account the directory refused (no Ductus group, disabled or gone there): it
    /// can't act until a check says yes again.
    pub directory_refused_at: Option<DateTime<Utc>>,
    /// A directory account: its person's id there (`<attribute>:<value>`), once a check read it.
    pub directory_id: Option<String>,
}

pub(crate) const USER_COLUMNS: &str =
    "id, username, display_name, email, role, permissions, source, must_change_password,
    disabled_at, locked_until, created_at, last_sign_in_at, directory_checked_at, directory_refused_at, directory_id";

/// Keep what an account was called before a change (`kind`: `username`, `email` or
/// `display_name`), in the change's transaction. An empty value is nothing to remember.
pub(crate) async fn keep_former(conn: &mut sqlx::PgConnection, user_id: i32, kind: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Ok(());
    }
    sqlx::query("INSERT INTO user_names (user_id, kind, value) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(kind)
        .bind(value)
        .execute(conn)
        .await
        .context("keeping the former name")?;
    Ok(())
}

/// A name, email or display name an account had before.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct FormerName {
    pub kind: String,
    pub value: String,
    pub until: DateTime<Utc>,
}

/// A new account.
#[derive(Debug, Clone)]
pub struct NewUser<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub email: &'a str,
    pub role: Role,
    pub permissions: &'a [String],
    /// None for an account whose password is checked elsewhere (`source` is not `local`).
    pub password_hash: Option<&'a str>,
    pub must_change_password: bool,
    pub source: &'a str,
}

/// What to change; None leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct UserChange {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub role: Option<Role>,
    pub permissions: Option<Vec<String>>,
    pub disabled: Option<bool>,
}

/// What a sign-in needs to know about a username.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Candidate {
    pub id: i32,
    pub password_hash: Option<String>,
    pub source: String,
    pub disabled_at: Option<DateTime<Utc>>,
    pub locked_until: Option<DateTime<Utc>>,
    /// A directory account's id there, if known.
    pub directory_id: Option<String>,
}

/// One of a user's sessions, for them to see and end.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionRow {
    pub id: i32,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The one making this request.
    pub current: bool,
}

/// A user's saved appearance.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserSettings {
    pub theme: String,
    /// The custom colors, as the UI saved them (a JSON object), if it did.
    pub colors: Option<String>,
}

/// Wrong passwords for one username: the limit, within what window, for how long.
#[derive(Debug, Clone, Copy)]
pub struct Lockout {
    pub attempts: u32,
    pub window: Duration,
    pub lock_for: Duration,
}

fn clean_username(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        bail!("a username is 1 to 64 characters");
    }
    if name.chars().any(|c| c.is_control() || c.is_whitespace() || c == '/' || c == ':') {
        bail!("a username can't contain spaces, slashes or colons");
    }
    Ok(name.to_string())
}

impl Store {
    /// Make an account. Refuses a name taken (in any case).
    pub async fn create_user(&self, new: &NewUser<'_>, audit: &Audit<'_>) -> Result<User> {
        let username = clean_username(new.username)?;
        let permissions = clean_permissions(new.permissions)?;
        let mut tx = self.pool.begin().await.context("creating user")?;
        let sql = format!(
            "INSERT INTO users (username, display_name, email, role, permissions, password_hash, must_change_password, source)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {USER_COLUMNS}"
        );
        let made = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(sql))
            .bind(&username)
            .bind(new.display_name.trim())
            .bind(new.email.trim())
            .bind(new.role as i16)
            .bind(&permissions)
            .bind(new.password_hash)
            .bind(new.must_change_password)
            .bind(new.source)
            .fetch_one(&mut *tx)
            .await;
        match made {
            Ok(user) => {
                let detail = serde_json::json!({ "role": user.role.label(), "permissions": user.permissions, "source": user.source });
                audit::insert(&mut tx, &Audit { subject: Some(&user.username), detail: Some(detail), ..audit.clone() })
                    .await?;
                tx.commit().await.context("creating user")?;
                Ok(user)
            }
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                bail!("there is already a user named `{username}` (names ignore case)")
            }
            Err(e) => Err(e).context("creating user"),
        }
    }

    /// All users by name.
    pub async fn users(&self) -> Result<Vec<User>> {
        let sql = format!("SELECT {USER_COLUMNS} FROM users ORDER BY lower(username)");
        sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(sql)).fetch_all(&self.pool).await.context("reading users")
    }

    pub async fn user(&self, id: i32) -> Result<Option<User>> {
        let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1");
        sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("reading user")
    }

    pub async fn user_by_name(&self, username: &str) -> Result<Option<User>> {
        let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE lower(username) = lower($1)");
        sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(sql))
            .bind(username.trim())
            .fetch_optional(&self.pool)
            .await
            .context("reading user")
    }

    /// Change an account. Refuses a change that would leave no admin who can sign in. A user
    /// who is disabled loses their sessions at once (their tokens stop by themselves).
    pub async fn update_user(&self, id: i32, change: &UserChange, audit: &Audit<'_>) -> Result<User> {
        let permissions = change.permissions.as_deref().map(clean_permissions).transpose()?;
        let mut tx = self.pool.begin().await.context("updating user")?;
        // Lock the admins in id order, so two changes at once can't both think another admin remains.
        let before = sqlx::query("SELECT id FROM users WHERE role = 2 AND disabled_at IS NULL ORDER BY id FOR UPDATE")
            .execute(&mut *tx)
            .await
            .context("updating user")?
            .rows_affected();
        let old: Option<(String, String)> =
            sqlx::query_as("SELECT display_name, email FROM users WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .context("updating user")?;
        let sql = format!(
            "UPDATE users SET
                 display_name = COALESCE($2, display_name),
                 email = COALESCE($3, email),
                 role = COALESCE($4, role),
                 permissions = COALESCE($5, permissions),
                 disabled_at = CASE WHEN $6::boolean IS NULL THEN disabled_at
                                    WHEN $6 THEN COALESCE(disabled_at, now()) ELSE NULL END
             WHERE id = $1 RETURNING {USER_COLUMNS}"
        );
        let user = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(change.display_name.as_deref().map(str::trim))
            .bind(change.email.as_deref().map(str::trim))
            .bind(change.role.map(|r| r as i16))
            .bind(permissions)
            .bind(change.disabled)
            .fetch_optional(&mut *tx)
            .await
            .context("updating user")?
            .with_context(|| format!("no user with id {id}"))?;
        let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE role = 2 AND disabled_at IS NULL")
            .fetch_one(&mut *tx)
            .await
            .context("updating user")?;
        if before > 0 && admins == 0 {
            bail!("that would leave no admin who can sign in: make another user an admin first");
        }
        if let Some((display_name, email)) = old {
            if display_name != user.display_name {
                keep_former(&mut tx, id, "display_name", &display_name).await?;
            }
            if email != user.email {
                keep_former(&mut tx, id, "email", &email).await?;
            }
        }
        if user.disabled_at.is_some() {
            sqlx::query("DELETE FROM api_sessions WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .context("ending the user's sessions")?;
        }
        let detail = serde_json::json!({
            "role": change.role.map(|r| r.label()), "permissions": change.permissions,
            "disabled": change.disabled, "display_name": change.display_name.is_some(), "email": change.email.is_some(),
        });
        audit::insert(&mut tx, &Audit { subject: Some(&user.username), detail: Some(detail), ..audit.clone() }).await?;
        tx.commit().await.context("updating user")?;
        Ok(user)
    }

    /// What account `id` was called before, oldest first.
    pub async fn former_names(&self, id: i32) -> Result<Vec<FormerName>> {
        sqlx::query_as::<_, FormerName>("SELECT kind, value, until FROM user_names WHERE user_id = $1 ORDER BY id")
            .bind(id)
            .fetch_all(&self.pool)
            .await
            .context("reading former names")
    }

    /// What a sign-in as `username` needs (None: nobody has that name).
    pub async fn candidate(&self, username: &str) -> Result<Option<Candidate>> {
        sqlx::query_as::<_, Candidate>(
            "SELECT id, password_hash, source, disabled_at, locked_until, directory_id FROM users
             WHERE lower(username) = lower($1)",
        )
        .bind(username.trim())
        .fetch_optional(&self.pool)
        .await
        .context("reading the account")
    }

    /// A wrong password: counted within the window, and past the limit the account is locked for
    /// a while. Returns when it is locked until, if it now is.
    pub async fn record_failed_login(
        &self,
        id: i32,
        lockout: Lockout,
        audit: &Audit<'_>,
    ) -> Result<Option<DateTime<Utc>>> {
        let mut tx = self.pool.begin().await.context("noting the wrong password")?;
        let locked: Option<DateTime<Utc>> = sqlx::query_scalar(
            "UPDATE users SET
                 failed_logins = CASE WHEN last_failed_at IS NULL OR last_failed_at < now() - make_interval(secs => $2)
                                      THEN 1 ELSE failed_logins + 1 END,
                 last_failed_at = now(),
                 locked_until = CASE WHEN (CASE WHEN last_failed_at IS NULL OR last_failed_at < now() - make_interval(secs => $2)
                                                THEN 1 ELSE failed_logins + 1 END) >= $3
                                     THEN now() + make_interval(secs => $4) ELSE locked_until END
             WHERE id = $1 RETURNING locked_until",
        )
        .bind(id)
        .bind(lockout.window.as_secs_f64())
        .bind(lockout.attempts as i32)
        .bind(lockout.lock_for.as_secs_f64())
        .fetch_optional(&mut *tx)
        .await
        .context("noting the wrong password")?
        .flatten();
        let locked = locked.filter(|until| *until > Utc::now());
        let detail = serde_json::json!({ "reason": "wrong password", "locked": locked.is_some() });
        audit::insert(&mut tx, &Audit { detail: Some(detail), ..audit.clone() }).await?;
        tx.commit().await.context("noting the wrong password")?;
        Ok(locked)
    }

    /// A successful sign-in: the count of wrong passwords starts over.
    pub async fn record_sign_in(&self, id: i32) -> Result<()> {
        sqlx::query(
            "UPDATE users SET failed_logins = 0, last_failed_at = NULL, locked_until = NULL, last_sign_in_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .context("noting the sign-in")?;
        Ok(())
    }

    /// Let a locked account try again.
    pub async fn unlock_user(&self, id: i32, audit: &Audit<'_>) -> Result<()> {
        let mut tx = self.pool.begin().await.context("unlocking the account")?;
        let name: Option<String> = sqlx::query_scalar(
            "UPDATE users SET failed_logins = 0, last_failed_at = NULL, locked_until = NULL WHERE id = $1
             RETURNING username",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .context("unlocking the account")?;
        audit::insert(&mut tx, &Audit { subject: name.as_deref(), ..audit.clone() }).await?;
        tx.commit().await.context("unlocking the account")?;
        Ok(())
    }

    /// A new password. Their other sessions end (the one named by `keep`, a cookie's secret,
    /// stays: the person changing it is using it).
    pub async fn set_password(
        &self,
        id: i32,
        password_hash: &str,
        must_change: bool,
        keep: Option<&str>,
        audit: &Audit<'_>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.context("changing the password")?;
        let name: Option<String> = sqlx::query_scalar(
            "UPDATE users SET password_hash = $2, must_change_password = $3, failed_logins = 0,
                    last_failed_at = NULL, locked_until = NULL
             WHERE id = $1 AND source = 'local' RETURNING username",
        )
        .bind(id)
        .bind(password_hash)
        .bind(must_change)
        .fetch_optional(&mut *tx)
        .await
        .context("changing the password")?;
        let Some(name) = name else {
            bail!("this account's password isn't kept here (its directory decides it)");
        };
        audit::insert(&mut tx, &Audit { subject: Some(&name), ..audit.clone() }).await?;
        sqlx::query("DELETE FROM api_sessions WHERE user_id = $1 AND ($2::bytea IS NULL OR hash <> $2)")
            .bind(id)
            .bind(keep.map(crate::api::hash_secret))
            .execute(&mut *tx)
            .await
            .context("ending the other sessions")?;
        tx.commit().await.context("changing the password")?;
        Ok(())
    }

    /// The stored hash of a local account's password, to check the current one before changing it.
    pub async fn password_hash(&self, id: i32) -> Result<Option<String>> {
        let hash: Option<Option<String>> = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("reading the password")?;
        Ok(hash.flatten())
    }

    /// A user's live sessions, newest first; `current` marks the one whose cookie secret is given.
    pub async fn user_sessions(&self, id: i32, current: Option<&str>, idle: Duration) -> Result<Vec<SessionRow>> {
        sqlx::query_as::<_, SessionRow>(
            "SELECT id, created_at, last_seen_at, expires_at, ($2::bytea IS NOT NULL AND hash = $2) AS current
             FROM api_sessions
             WHERE user_id = $1 AND expires_at > now() AND last_seen_at > now() - make_interval(secs => $3)
             ORDER BY created_at DESC",
        )
        .bind(id)
        .bind(current.map(crate::api::hash_secret))
        .bind(idle.as_secs_f64())
        .fetch_all(&self.pool)
        .await
        .context("reading sessions")
    }

    /// End one of a user's sessions (theirs only). False if it isn't theirs or is gone.
    pub async fn end_user_session(&self, user_id: i32, session_id: i32) -> Result<bool> {
        let done = sqlx::query("DELETE FROM api_sessions WHERE id = $1 AND user_id = $2")
            .bind(session_id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .context("ending the session")?;
        Ok(done.rows_affected() > 0)
    }

    /// End all of a user's sessions but the one named by `keep`.
    pub async fn end_other_sessions(&self, user_id: i32, keep: Option<&str>) -> Result<u64> {
        let done = sqlx::query("DELETE FROM api_sessions WHERE user_id = $1 AND ($2::bytea IS NULL OR hash <> $2)")
            .bind(user_id)
            .bind(keep.map(crate::api::hash_secret))
            .execute(&self.pool)
            .await
            .context("ending the sessions")?;
        Ok(done.rows_affected())
    }

    /// A user's saved appearance, if they have saved one.
    pub async fn user_settings(&self, user_id: i32) -> Result<Option<UserSettings>> {
        sqlx::query_as::<_, UserSettings>("SELECT theme, colors::text AS colors FROM user_settings WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .context("reading settings")
    }

    /// Save a user's appearance (`colors` is a JSON object as text).
    pub async fn save_user_settings(&self, user_id: i32, theme: &str, colors: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_settings (user_id, theme, colors) VALUES ($1, $2, $3::jsonb)
             ON CONFLICT (user_id) DO UPDATE SET theme = $2, colors = $3::jsonb, updated_at = now()",
        )
        .bind(user_id)
        .bind(theme)
        .bind(colors)
        .execute(&self.pool)
        .await
        .context("saving settings")?;
        Ok(())
    }

    /// How many users there are (the first-run message says what to do when there are none).
    pub async fn user_count(&self) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&self.pool).await.context("counting users")
    }
}
