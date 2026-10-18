//! People who sign in with a directory (LDAP / Active Directory): their `users` rows (source
//! `ldap`), made at their first sign-in and kept as the directory says at every sign-in and every
//! refresh. The directory decides role, permissions, name and email; an admin here can still
//! disable the account.

use crate::Store;
use crate::api::{Role, clean_permissions};
use crate::audit::{self, Audit};
use crate::users::{USER_COLUMNS, User};
use anyhow::{Context, Result, bail};

/// What the directory says about a person who may sign in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryPerson {
    pub display_name: String,
    pub email: String,
    pub role: Role,
    pub permissions: Vec<String>,
}

/// What a directory check changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryUpdate {
    /// The account was made (a first sign-in).
    Created,
    /// Role, permissions, name or email changed, or access came back after a refusal.
    Changed,
    Unchanged,
}

impl Store {
    /// Make or bring up to date the account of `username` from what the directory says, in one
    /// transaction with its audit row (`user.create` or `user.directory_change`, written by
    /// `audit`'s actor). A refusal is lifted. Refuses a name a local account holds.
    pub async fn save_directory_user(
        &self,
        username: &str,
        person: &DirectoryPerson,
        audit: &Audit<'_>,
    ) -> Result<(User, DirectoryUpdate)> {
        let permissions = clean_permissions(&person.permissions)?;
        let mut tx = self.pool.begin().await.context("saving the directory account")?;
        let before = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(format!(
            "SELECT {USER_COLUMNS} FROM users WHERE lower(username) = lower($1) FOR UPDATE"
        )))
        .bind(username.trim())
        .fetch_optional(&mut *tx)
        .await
        .context("saving the directory account")?;
        let (user, update) = match before {
            None => {
                let user = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(format!(
                    "INSERT INTO users (username, display_name, email, role, permissions, source, directory_checked_at)
                     VALUES ($1, $2, $3, $4, $5, 'ldap', now()) RETURNING {USER_COLUMNS}"
                )))
                .bind(username.trim())
                .bind(person.display_name.trim())
                .bind(person.email.trim())
                .bind(person.role as i16)
                .bind(&permissions)
                .fetch_one(&mut *tx)
                .await
                .context("making the directory account")?;
                let detail =
                    serde_json::json!({ "role": user.role.label(), "permissions": user.permissions, "source": "ldap" });
                let row = Audit {
                    action: "user.create",
                    subject: Some(&user.username),
                    detail: Some(detail),
                    ..audit.clone()
                };
                audit::insert(&mut tx, &row).await?;
                (user, DirectoryUpdate::Created)
            }
            Some(old) if old.source != "ldap" => {
                bail!("`{}` is a {} account, not a directory one", old.username, old.source)
            }
            Some(old) => {
                let user = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(format!(
                    "UPDATE users SET display_name = $2, email = $3, role = $4, permissions = $5,
                         directory_checked_at = now(), directory_refused_at = NULL
                     WHERE id = $1 RETURNING {USER_COLUMNS}"
                )))
                .bind(old.id)
                .bind(person.display_name.trim())
                .bind(person.email.trim())
                .bind(person.role as i16)
                .bind(&permissions)
                .fetch_one(&mut *tx)
                .await
                .context("updating the directory account")?;
                let mut changes = serde_json::Map::new();
                if old.role != user.role {
                    changes.insert("role".into(), serde_json::json!([old.role.label(), user.role.label()]));
                }
                if old.permissions != user.permissions {
                    changes.insert("permissions".into(), serde_json::json!([old.permissions, user.permissions]));
                }
                if old.display_name != user.display_name {
                    changes.insert("display_name".into(), serde_json::json!(true));
                }
                if old.email != user.email {
                    changes.insert("email".into(), serde_json::json!(true));
                }
                if old.directory_refused_at.is_some() {
                    changes.insert("access".into(), serde_json::json!("restored"));
                }
                if changes.is_empty() {
                    (user, DirectoryUpdate::Unchanged)
                } else {
                    let row = Audit {
                        action: "user.directory_change",
                        subject: Some(&user.username),
                        detail: Some(serde_json::Value::Object(changes)),
                        ..audit.clone()
                    };
                    audit::insert(&mut tx, &row).await?;
                    (user, DirectoryUpdate::Changed)
                }
            }
        };
        tx.commit().await.context("saving the directory account")?;
        Ok((user, update))
    }

    /// The directory said no to a directory account (`reason`: no Ductus group, disabled or gone
    /// there): it can't act until a check says yes again, and its sessions end now. Logged
    /// (`user.directory_refused`) only when it wasn't refused already. True if it newly was.
    pub async fn refuse_directory_user(&self, id: i32, reason: &str, audit: &Audit<'_>) -> Result<bool> {
        let mut tx = self.pool.begin().await.context("ending the directory account's access")?;
        let newly: Option<String> = sqlx::query_scalar(
            "UPDATE users SET directory_refused_at = COALESCE(directory_refused_at, now()), directory_checked_at = now()
             WHERE id = $1 AND source = 'ldap'
             RETURNING CASE WHEN directory_refused_at = now() THEN username END",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .context("ending the directory account's access")?
        .flatten();
        sqlx::query("DELETE FROM api_sessions WHERE user_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("ending the user's sessions")?;
        if let Some(name) = &newly {
            let row = Audit {
                action: "user.directory_refused",
                subject: Some(name),
                detail: Some(serde_json::json!({ "reason": reason })),
                ..audit.clone()
            };
            audit::insert(&mut tx, &row).await?;
        }
        tx.commit().await.context("ending the directory account's access")?;
        Ok(newly.is_some())
    }

    /// Directory accounts to check again: not disabled here, and with a live session or a token
    /// (a refused account's token comes back if the directory says yes again). By name.
    pub async fn directory_users_to_check(&self) -> Result<Vec<(i32, String)>> {
        sqlx::query_as(
            "SELECT u.id, u.username FROM users u
             WHERE u.source = 'ldap' AND u.disabled_at IS NULL
               AND (EXISTS (SELECT 1 FROM api_sessions s WHERE s.user_id = u.id AND s.expires_at > now())
                    OR EXISTS (SELECT 1 FROM api_tokens t WHERE t.user_id = u.id AND t.revoked_at IS NULL))
             ORDER BY lower(u.username)",
        )
        .fetch_all(&self.pool)
        .await
        .context("finding directory accounts to check")
    }
}
