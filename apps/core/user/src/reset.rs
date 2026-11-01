//! Password change, forgotten-password request and reset. On the staff form
//! the address alone names the account (unique platform-wide); on a portal
//! (ADR 16) it names one of that tenant's customers. A reset is issued and
//! redeemed only for an account whose standing (`identity::standing`) is live.
use crate::{
    identity::Audience,
    password::{self, check_policy},
    CredentialError, UserService,
};
use bilibi_database::{Change, Context, Error};
use bilibi_mail::{Address, Message};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio_postgres::GenericClient;

/// A reset link stays valid this long and a new one may be requested this often.
const RESET_LIFETIME_SECONDS: f64 = 3600.0;
const RESET_THROTTLE_SECONDS: f64 = 60.0;

pub(crate) fn normalize_email(email: &str) -> Result<String, Error> {
    let email = email.trim().to_lowercase();
    if email.len() > 254 || !email.contains('@') || email.chars().any(char::is_whitespace) {
        return Err(Error::Invalid);
    }
    Ok(email)
}
pub(crate) fn random_token() -> Result<String, Error> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn digest(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
/// A new one-time reset token for the account, stored hashed, unless one was
/// issued within the throttle (one a minute); expired rows are swept here,
/// which keeps storage bounded without a scheduler.
async fn issue_reset_token(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<Option<String>, Error> {
    let token = random_token()?;
    let claimed = client
        .query_opt(
            "INSERT INTO password_reset_tokens(identity_id,token_hash,created_at,expires_at) SELECT identity_id,$3,now(),now()+make_interval(secs => $4::double precision) FROM users WHERE tenant_id=$1 AND id=$2 ON CONFLICT (identity_id) DO UPDATE SET token_hash=excluded.token_hash,created_at=excluded.created_at,expires_at=excluded.expires_at WHERE password_reset_tokens.created_at<=now()-make_interval(secs => $5::double precision) RETURNING identity_id",
            &[&tenant, &user, &digest(&token), &RESET_LIFETIME_SECONDS, &RESET_THROTTLE_SECONDS],
        )
        .await?;
    if claimed.is_none() {
        return Ok(None);
    }
    client
        .execute(
            "DELETE FROM password_reset_tokens WHERE expires_at<now()",
            &[],
        )
        .await?;
    Ok(Some(token))
}

async fn finish_reset_request() -> Result<(), CredentialError> {
    // Every normal outcome pays for this same expensive check so the public
    // response reveals less about whether the address has an active account.
    password::verify("", None).await?;
    Ok(())
}
fn encode_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Whether `site` can be a portal address segment, so a reset link never
/// carries anything else into a path.
pub(crate) fn site_shaped(site: &str) -> bool {
    (1..=63).contains(&site.len())
        && site
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl UserService {
    /// Self-service change: the session proves who, the current password proves
    /// the holder. Other sessions of the account end; this one continues.
    pub async fn change_password(
        &self,
        token: &str,
        csrf: &str,
        current: &str,
        new: &str,
    ) -> Result<(), CredentialError> {
        if current.len() > password::MAX_LENGTH {
            return Err(CredentialError::IncorrectPassword);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        check_policy(new).map_err(CredentialError::Policy)?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let credential = client
            .query_opt(
                "SELECT i.password_hash,i.id FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.status='active' AND i.deleted_at IS NULL WHERE u.tenant_id=$1 AND u.id=$2 AND u.status='active' AND u.deleted_at IS NULL",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(Error::Unauthorized)?;
        let (stored, identity): (String, i64) = (credential.get(0), credential.get(1));
        if !password::verify(current, Some(&stored)).await? {
            return Err(CredentialError::IncorrectPassword);
        }
        let replacement = password::hash(new).await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.change_password"))
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                // Compare-and-set: a concurrent change makes this one a conflict.
                let changed = sql
                    .execute(
                        "UPDATE user_identities SET password_hash=$3,password_version=password_version+1,updated_at=now() WHERE id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND password_hash=$4",
                        &[&tenant, &user, &replacement, &stored],
                    )
                    .await?;
                if changed != 1 {
                    return Err(Error::Conflict);
                }

                sql.execute(
                    "DELETE FROM password_reset_tokens WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2)",
                    &[&tenant, &user],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "user_identity".into(),
                        id: identity,
                        old: json!({"password_hash":"previous"}),
                        new: json!({"password_hash":"replaced"}),
                    }],
                ))
            })
        })
        .await?;
        // Other live pages of this account hear it once the change is committed.
        let ended = self.sessions.revoke(&mut tx, user, Some(token)).await?;
        let sessions = self.sessions.clone();
        tx.after_commit(move || sessions.announce(ended));
        tx.commit().await?;
        Ok(())
    }

    /// Always answers `Ok` for an unknown, frozen or throttled account so the
    /// public form gives one response. Every normal outcome performs the same
    /// dummy password verification to reduce account-dependent timing clues.
    /// Mail delivery is asynchronous and is never awaited here.
    pub async fn request_password_reset(&self, email: &str) -> Result<(), CredentialError> {
        self.request_reset(Audience::Staff, None, email).await
    }
    /// [`UserService::request_password_reset`] for one audience; a customer's
    /// link opens its portal's reset page at `site`.
    pub(crate) async fn request_reset(
        &self,
        audience: Audience,
        site: Option<&str>,
        email: &str,
    ) -> Result<(), CredentialError> {
        let email = normalize_email(email)?;
        let client = self.db.get().await.map_err(Error::from)?;
        let candidates = client
            .query(
                "SELECT u.id,u.tenant_id,u.name,i.email FROM user_identities i JOIN users u ON u.identity_id=i.id AND u.tenant_id=i.last_tenant_id JOIN tenants t ON t.id=u.tenant_id WHERE lower(i.email)=$1 AND i.kind=$2 AND ($3::bigint IS NULL OR i.tenant_id=$3) AND i.deleted_at IS NULL AND i.status='active' AND u.deleted_at IS NULL AND u.status='active' AND t.deleted_at IS NULL AND t.status='active' LIMIT 2",
                &[&email, &audience.kind().as_str(), &audience.tenant()],
            )
            .await
            .map_err(Error::from)?;
        // Ambiguity is treated as unknown rather than guessed; a frozen account
        // (archived company, retired identity) is unknown here too.
        let [account] = &candidates[..] else {
            return finish_reset_request().await;
        };
        let (user, tenant, name, address): (i64, i64, String, String) = (
            account.get(0),
            account.get(1),
            account.get(2),
            account.get(3),
        );
        if !matches!(
            crate::identity::standing(&**client, tenant, user).await?,
            crate::identity::Standing::Live(_)
        ) {
            return finish_reset_request().await;
        }
        let Some(token) = issue_reset_token(&**client, tenant, user).await? else {
            return finish_reset_request().await;
        };
        let link = match site {
            Some(site) => self.portal_reset_link(site, &address, &token),
            None => self.reset_link(&address, &token),
        };
        let message = Message::text(
            Address::new(Some(&name), &address).map_err(|_| Error::Invalid)?,
            "Reset your Bilibi password",
            format!(
                "Reset your Bilibi password by opening this link:\n\n{link}\n\nIf you did not request this, you can ignore this email.\n"
            ),
        );
        let (mailer, db, hash) = (self.mailer.clone(), self.db.clone(), digest(&token));
        tokio::spawn(async move {
            if let Err(error) = mailer.deliver(&message).await {
                eprintln!("mail: password reset for user {user} was not delivered: {error}");
                // Release the throttle so the person can ask again.
                if let Ok(client) = db.get().await {
                    let _ = client
                        .execute(
                            "DELETE FROM password_reset_tokens WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND token_hash=$3",
                            &[&tenant, &user, &hash],
                        )
                        .await;
                }
            }
        });
        finish_reset_request().await
    }

    /// Mails `text` to the account with a reset link appended, when a token
    /// can be issued now (a request within the last minute already holds
    /// one; the mail then points to "Forgot your password?"). A failed
    /// delivery releases the token, as for a requested reset.
    pub(crate) async fn mail_with_reset_link(
        &self,
        tenant: i64,
        user: i64,
        subject: &str,
        text: &str,
    ) -> Result<(), Error> {
        let client = self.db.get().await?;
        let Some(row) = client
            .query_opt(
                "SELECT u.name,i.email FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.deleted_at IS NULL AND i.status='active' WHERE u.tenant_id=$1 AND u.id=$2 AND u.deleted_at IS NULL AND u.status='active'",
                &[&tenant, &user],
            )
            .await?
        else {
            return Ok(());
        };
        let (name, address): (String, String) = (row.get(0), row.get(1));
        let token = issue_reset_token(&**client, tenant, user).await?;
        let link = match &token {
            Some(token) => format!(
                "Reset your password with this link:\n\n{}\n",
                self.reset_link(&address, token)
            ),
            None => {
                "Reset your password with \"Forgot your password?\" on the sign-in page.\n".into()
            }
        };
        let message = Message::text(
            Address::new(Some(&name), &address).map_err(|_| Error::Invalid)?,
            subject,
            format!("{text}\n\n{link}"),
        );
        let (mailer, db, hash) = (
            self.mailer.clone(),
            self.db.clone(),
            token.as_deref().map(digest),
        );
        tokio::spawn(async move {
            if let Err(error) = mailer.deliver(&message).await {
                eprintln!("mail: notice for user {user} was not delivered: {error}");
                if let (Some(hash), Ok(client)) = (hash, db.get().await) {
                    let _ = client
                        .execute(
                            "DELETE FROM password_reset_tokens WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND token_hash=$3",
                            &[&tenant, &user, &hash],
                        )
                        .await;
                }
            }
        });
        Ok(())
    }

    /// Where the emailed link points; the reset page reads both values.
    pub fn reset_link(&self, email: &str, token: &str) -> String {
        format!(
            "{}/reset-password/{token}?email={}",
            self.origin,
            encode_component(email)
        )
    }

    /// Where a portal's emailed link points: the host's reset page of the
    /// portal at `site` (ADR 16).
    pub(crate) fn portal_reset_link(&self, site: &str, email: &str, token: &str) -> String {
        format!(
            "{}/portal/reset/{site}/{token}?email={}",
            self.origin,
            encode_component(email)
        )
    }

    /// Consumes the one-time token, replaces the password and ends every
    /// session of the account. Unknown, frozen, expired and reused tokens are
    /// indistinguishable.
    pub async fn reset_password(
        &self,
        email: &str,
        token: &str,
        new: &str,
    ) -> Result<(), CredentialError> {
        self.reset(Audience::Staff, email, token, new).await
    }
    /// [`UserService::reset_password`] for one audience.
    pub(crate) async fn reset(
        &self,
        audience: Audience,
        email: &str,
        token: &str,
        new: &str,
    ) -> Result<(), CredentialError> {
        let email = normalize_email(email).map_err(|_| CredentialError::InvalidOrExpiredToken)?;
        if token.is_empty() || token.len() > 128 {
            return Err(CredentialError::InvalidOrExpiredToken);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let account = client
            .query(
                "SELECT u.id,u.tenant_id,i.id,r.token_hash FROM user_identities i JOIN users u ON u.identity_id=i.id AND u.tenant_id=i.last_tenant_id JOIN tenants t ON t.id=u.tenant_id LEFT JOIN password_reset_tokens r ON r.identity_id=i.id AND r.expires_at>now() WHERE lower(i.email)=$1 AND i.kind=$2 AND ($3::bigint IS NULL OR i.tenant_id=$3) AND i.deleted_at IS NULL AND i.status='active' AND u.deleted_at IS NULL AND u.status='active' AND t.deleted_at IS NULL AND t.status='active' LIMIT 2",
                &[&email, &audience.kind().as_str(), &audience.tenant()],
            )
            .await
            .map_err(Error::from)?;
        let presented = digest(token);
        let user = match &account[..] {
            [row] => row
                .get::<_, Option<Vec<u8>>>(3)
                .filter(|stored| bool::from(stored.ct_eq(&presented)))
                .map(|_| {
                    (
                        row.get::<_, i64>(0),
                        row.get::<_, i64>(1),
                        row.get::<_, i64>(2),
                    )
                }),
            _ => None,
        };
        let Some((user, tenant, identity)) = user else {
            password::verify("", None).await?;
            return Err(CredentialError::InvalidOrExpiredToken);
        };
        // A frozen account cannot redeem a link either; same answer as unknown.
        if !matches!(
            crate::identity::standing(&**client, tenant, user).await?,
            crate::identity::Standing::Live(_)
        ) {
            return Err(CredentialError::InvalidOrExpiredToken);
        }
        check_policy(new).map_err(CredentialError::Policy)?;
        let replacement = password::hash(new).await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::user(tenant, user, "user.reset_password"),
            )
            .await?;
        let claimed = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    // Deleting the row is the single-use claim; a racing reset loses.
                    let claimed = sql
                        .execute(
                            "DELETE FROM password_reset_tokens WHERE identity_id=$1 AND token_hash=$2 AND expires_at>now()",
                            &[&identity, &presented],
                        )
                        .await?;
                    if claimed != 1 {
                        return Err(Error::NotFound);
                    }
                    sql.execute(
                        "UPDATE user_identities SET password_hash=$2,password_version=password_version+1,updated_at=now() WHERE id=$1",
                        &[&identity, &replacement],
                    )
                    .await?;
                    let mut changes = vec![Change {
                        kind: "user_identity".into(),
                        id: identity,
                        old: json!({"password_hash":"previous"}),
                        new: json!({"password_hash":"replaced"}),
                    }];
                    // The emailed link proves the holder: a sign-in lock ends.
                    let kind = crate::credential_attempts::Kind::SignIn;
                    if crate::credential_attempts::forget(sql, tenant, user, kind).await? {
                        changes.push(crate::credential_attempts::change(
                            user,
                            kind,
                            json!({}),
                            json!({"cleared_by": "password_reset"}),
                        ));
                    }
                    Ok(((), changes))
                })
            })
            .await;
        match claimed {
            Ok(()) => {
                let ended = self.sessions.revoke(&mut tx, user, None).await?;
                let sessions = self.sessions.clone();
                tx.after_commit(move || sessions.announce(ended));
                Ok(tx.commit().await?)
            }
            Err(Error::NotFound) => Err(CredentialError::InvalidOrExpiredToken),
            Err(error) => Err(error.into()),
        }
    }
}
