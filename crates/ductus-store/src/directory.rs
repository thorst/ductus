//! People who sign in with a directory (LDAP / Active Directory): their `users` rows (source
//! `ldap`), made at their first sign-in and kept as the directory says at every sign-in and every
//! refresh. The directory decides role, permissions, name and email; an admin here can still
//! disable the account.
//!
//! An account is the directory's *person*, known by their directory id (`users.directory_id`:
//! entryUUID or objectGUID) once one was read: a renamed sign-in name renames the account, and a
//! name given to someone new gets them an account of their own, while the earlier person's moves
//! out of the way (`<name>#<id>`) and stays refused, tokens and all.

use crate::Store;
use crate::api::{Role, clean_permissions};
use crate::audit::{self, Audit};
use crate::users::{USER_COLUMNS, User, keep_former};
use anyhow::{Context, Result, bail};
use sqlx::{Postgres, Transaction};

/// What the directory says about a person who may sign in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryPerson {
    pub display_name: String,
    pub email: String,
    pub role: Role,
    pub permissions: Vec<String>,
    /// Their sign-in name as the directory spells it now (`auth.ldap.attributes.username`), if it
    /// was read: the account takes it.
    pub username: Option<String>,
    /// Their id in the directory (`<attribute>:<value>`), if it gives one.
    pub directory_id: Option<String>,
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

/// A directory account to check again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryAccount {
    pub id: i32,
    pub username: String,
    pub directory_id: Option<String>,
}

/// Whether two stored ids were read from the same attribute (after a change of
/// `attributes.id`, an old id says nothing about who someone is).
fn same_attribute(a: &str, b: &str) -> bool {
    let attribute = |s: &str| s.split_once(':').map_or(s, |(x, _)| x).to_ascii_lowercase();
    attribute(a) == attribute(b)
}

impl Store {
    /// Make or bring up to date the account of the person the directory found as `username` (what
    /// they typed, or the account's name at a refresh), in one transaction with its audit rows
    /// (`user.create` or `user.directory_change`, written by `audit`'s actor). A refusal is lifted.
    ///
    /// With a directory id, the account is the one with that id: renamed when the directory's
    /// sign-in name changed. An account made before ids were kept takes its id at its first check.
    /// An account holding the name with *another* id was someone else's: it is renamed
    /// `<name>#<account id>` and refused (its sessions end, its tokens stay dead), and the person
    /// gets an account of their own. Every change of name or email keeps the old value
    /// (`user_names`). Refuses a name a local account holds. A new account's `user.create` row
    /// takes `audit.detail`'s fields too (the engine's `"reason": "owner"`).
    ///
    /// Two engines (or two sign-ins) making the same person's account at once: the one that
    /// commits second hits the unique name or id, and is retried once, finding the account.
    pub async fn save_directory_user(
        &self,
        username: &str,
        person: &DirectoryPerson,
        audit: &Audit<'_>,
    ) -> Result<(User, DirectoryUpdate)> {
        match self.save_directory_user_once(username, person, audit).await {
            Err(e) if unique_violation(&e) => self.save_directory_user_once(username, person, audit).await,
            saved => saved,
        }
    }

    /// `save_directory_user` for a refresh: an account the directory says nothing new about is
    /// read and left as it is, with no lock and nothing written (no commit to wait for); the round
    /// notes everyone it asked about at once (`directory_checked`). Anything new is saved as a
    /// sign-in's would be.
    pub async fn check_directory_user(
        &self,
        username: &str,
        person: &DirectoryPerson,
        audit: &Audit<'_>,
    ) -> Result<(User, DirectoryUpdate)> {
        let permissions = clean_permissions(&person.permissions)?;
        let (name, typed, id) = names(username, person);
        let found =
            candidates(name, typed, id, false).fetch_all(&self.pool).await.context("checking the directory account")?;
        let (account, holder) = resolve(&found, name, typed, id);
        if let Some(old) = account
            && holder.is_none_or(|h| h.id == old.id)
            && old.source == "ldap"
            && unchanged(old, name, person, &permissions, id)
        {
            return Ok((old.clone(), DirectoryUpdate::Unchanged));
        }
        self.save_directory_user(username, person, audit).await
    }

    /// The directory was asked about these accounts just now (one statement for a whole round).
    pub async fn directory_checked(&self, ids: &[i32]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        sqlx::query("UPDATE users SET directory_checked_at = now() WHERE id = ANY($1) AND source = 'ldap'")
            .bind(ids)
            .execute(&self.pool)
            .await
            .context("noting when the directory was asked")?;
        Ok(())
    }

    async fn save_directory_user_once(
        &self,
        username: &str,
        person: &DirectoryPerson,
        audit: &Audit<'_>,
    ) -> Result<(User, DirectoryUpdate)> {
        let permissions = clean_permissions(&person.permissions)?;
        let (name, typed, id) = names(username, person);
        let mut tx = self.pool.begin().await.context("saving the directory account")?;
        let found =
            candidates(name, typed, id, true).fetch_all(&mut *tx).await.context("saving the directory account")?;
        let (account, holder) = resolve(&found, name, typed, id);
        if let Some(local) = account.into_iter().chain(holder).find(|u| u.source != "ldap") {
            bail!("`{}` is a {} account, not a directory one", local.username, local.source);
        }
        if let Some(earlier) = holder.filter(|h| account.is_none_or(|a| a.id != h.id)) {
            step_aside(&mut tx, earlier, audit).await?;
        }
        let (user, update) = match account {
            None => {
                let user = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(format!(
                    "INSERT INTO users (username, display_name, email, role, permissions, source, directory_checked_at, directory_id)
                     VALUES ($1, $2, $3, $4, $5, 'ldap', now(), $6) RETURNING {USER_COLUMNS}"
                )))
                .bind(name)
                .bind(person.display_name.trim())
                .bind(person.email.trim())
                .bind(person.role as i16)
                .bind(&permissions)
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .context("making the directory account")?;
                let mut detail =
                    serde_json::json!({ "role": user.role.label(), "permissions": user.permissions, "source": "ldap" });
                if let (Some(serde_json::Value::Object(extra)), Some(fields)) = (&audit.detail, detail.as_object_mut())
                {
                    fields.extend(extra.clone());
                }
                let row = Audit {
                    action: "user.create",
                    subject: Some(&user.username),
                    detail: Some(detail),
                    ..audit.clone()
                };
                audit::insert(&mut tx, &row).await?;
                (user, DirectoryUpdate::Created)
            }
            Some(old) => {
                // The name keeps its spelling unless the directory's is another name.
                let username = if old.username.to_lowercase() == name.to_lowercase() { &old.username } else { name };
                let user = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(format!(
                    "UPDATE users SET username = $2, display_name = $3, email = $4, role = $5, permissions = $6,
                         directory_id = COALESCE($7, directory_id), directory_checked_at = now(), directory_refused_at = NULL
                     WHERE id = $1 RETURNING {USER_COLUMNS}"
                )))
                .bind(old.id)
                .bind(username)
                .bind(person.display_name.trim())
                .bind(person.email.trim())
                .bind(person.role as i16)
                .bind(&permissions)
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .context("updating the directory account")?;
                let mut changes = serde_json::Map::new();
                if old.username != user.username {
                    keep_former(&mut tx, old.id, "username", &old.username).await?;
                    changes.insert("username".into(), serde_json::json!([old.username, user.username]));
                }
                if old.role != user.role {
                    changes.insert("role".into(), serde_json::json!([old.role.label(), user.role.label()]));
                }
                if old.permissions != user.permissions {
                    changes.insert("permissions".into(), serde_json::json!([old.permissions, user.permissions]));
                }
                if old.display_name != user.display_name {
                    keep_former(&mut tx, old.id, "display_name", &old.display_name).await?;
                    changes.insert("display_name".into(), serde_json::json!(true));
                }
                if old.email != user.email {
                    keep_former(&mut tx, old.id, "email", &old.email).await?;
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
        let newly = refuse(&mut tx, id, reason, audit).await?;
        tx.commit().await.context("ending the directory account's access")?;
        Ok(newly)
    }

    /// Directory accounts to check again: not disabled here, and with a live session or a token
    /// (a refused account's token comes back if the directory says yes again).
    pub async fn directory_users_to_check(&self) -> Result<Vec<DirectoryAccount>> {
        let rows: Vec<(i32, String, Option<String>)> = sqlx::query_as(
            "SELECT u.id, u.username, u.directory_id FROM users u
             WHERE u.source = 'ldap' AND u.disabled_at IS NULL
               AND (EXISTS (SELECT 1 FROM api_sessions s WHERE s.user_id = u.id AND s.expires_at > now())
                    OR EXISTS (SELECT 1 FROM api_tokens t WHERE t.user_id = u.id AND t.revoked_at IS NULL))
             ORDER BY lower(u.username)",
        )
        .fetch_all(&self.pool)
        .await
        .context("finding directory accounts to check")?;
        Ok(rows
            .into_iter()
            .map(|(id, username, directory_id)| DirectoryAccount { id, username, directory_id })
            .collect())
    }
}

/// The sign-in name the account takes (the directory's spelling, else what was typed), what was
/// typed, and the directory id.
fn names<'a>(username: &'a str, person: &'a DirectoryPerson) -> (&'a str, &'a str, Option<&'a str>) {
    let typed = username.trim();
    let name = person.username.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(typed);
    (name, typed, person.directory_id.as_deref())
}

/// Everyone the person could be: by either name, or by id. `lock`: in id order, so two sign-ins
/// at once take them the same way.
fn candidates<'a>(
    name: &'a str,
    typed: &'a str,
    id: Option<&'a str>,
    lock: bool,
) -> sqlx::query::QueryAs<'a, Postgres, User, sqlx::postgres::PgArguments> {
    let lock = if lock { "FOR UPDATE" } else { "" };
    sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(format!(
        "SELECT {USER_COLUMNS} FROM users
         WHERE lower(username) IN (lower($1), lower($2)) OR directory_id = $3
         ORDER BY id {lock}"
    )))
    .bind(name)
    .bind(typed)
    .bind(id)
}

/// Of the candidates: the person's account, if they have one, and the account holding the name.
/// With an id, the account is the one with that id, else the name's holder when it isn't known by
/// an id that says who it is (a local account, or a directory one without an id, made before ids
/// were kept, or with one from another attribute).
fn resolve<'u>(found: &'u [User], name: &str, typed: &str, id: Option<&str>) -> (Option<&'u User>, Option<&'u User>) {
    let named = |n: &str| found.iter().find(|u| u.username.to_lowercase() == n.to_lowercase());
    let unproven = |u: &User| match (&u.directory_id, id) {
        (Some(theirs), Some(ours)) => !same_attribute(theirs, ours),
        _ => true,
    };
    let holder = named(name);
    let account = match id {
        None => holder,
        Some(id) => found
            .iter()
            .find(|u| u.directory_id.as_deref() == Some(id))
            .or(holder.filter(|u| unproven(u)))
            .or(named(typed).filter(|u| u.source == "ldap" && u.directory_id.is_none())),
    };
    (account, holder)
}

/// Whether saving `person` would change nothing of `old` (its name, as saving would spell it).
fn unchanged(old: &User, name: &str, person: &DirectoryPerson, permissions: &[String], id: Option<&str>) -> bool {
    old.username.to_lowercase() == name.to_lowercase()
        && old.display_name == person.display_name.trim()
        && old.email == person.email.trim()
        && old.role == person.role
        && old.permissions == permissions
        && id.is_none_or(|i| old.directory_id.as_deref() == Some(i))
        && old.directory_refused_at.is_none()
}

/// A unique index refused the write (`users_username`, `users_directory_id`): someone else saved
/// the same person in between.
fn unique_violation(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<sqlx::Error>(), Some(sqlx::Error::Database(d)) if d.is_unique_violation())
}

/// Refuse account `id` in `tx` (see `refuse_directory_user`). True if it newly was. One refused
/// already isn't written again (a refresh round notes when it was asked, `directory_checked`).
async fn refuse(tx: &mut Transaction<'_, Postgres>, id: i32, reason: &str, audit: &Audit<'_>) -> Result<bool> {
    let newly: Option<String> = sqlx::query_scalar(
        "UPDATE users SET directory_refused_at = now(), directory_checked_at = now()
         WHERE id = $1 AND source = 'ldap' AND directory_refused_at IS NULL
         RETURNING username",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .context("ending the directory account's access")?;
    sqlx::query("DELETE FROM api_sessions WHERE user_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .context("ending the user's sessions")?;
    if let Some(name) = &newly {
        let row = Audit {
            action: "user.directory_refused",
            subject: Some(name),
            detail: Some(serde_json::json!({ "reason": reason })),
            ..audit.clone()
        };
        audit::insert(tx, &row).await?;
    }
    Ok(newly.is_some())
}

/// The account that held a sign-in name the directory now gives someone else: renamed
/// `<name>#<id>` (the old name kept in `user_names`) and refused, so nothing of it comes back.
async fn step_aside(tx: &mut Transaction<'_, Postgres>, earlier: &User, audit: &Audit<'_>) -> Result<()> {
    let aside = format!("{}#{}", earlier.username, earlier.id);
    sqlx::query("UPDATE users SET username = $2 WHERE id = $1")
        .bind(earlier.id)
        .bind(&aside)
        .execute(&mut **tx)
        .await
        .context("moving the earlier account out of the way")?;
    keep_former(tx, earlier.id, "username", &earlier.username).await?;
    let detail = serde_json::json!({
        "username": [earlier.username, aside],
        "reason": "the directory gives this name to another person now",
    });
    let row = Audit { action: "user.directory_change", subject: Some(&aside), detail: Some(detail), ..audit.clone() };
    audit::insert(tx, &row).await?;
    refuse(tx, earlier.id, "its sign-in name is another person's in the directory now", audit).await?;
    Ok(())
}
