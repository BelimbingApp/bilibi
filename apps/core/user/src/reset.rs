//! Password change, forgotten-password request and reset. The address alone
//! names the account (unique platform-wide), and a reset is issued and redeemed
//! only for an account whose standing (`identity::standing`) is live.
use crate::{
    password::{self, check_policy},
    CredentialError, UserService,
};
use bilibi_database::{Change, Context, Error};
use bilibi_mail::{Address, Message};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

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
        let stored: String = client
            .query_opt(
                "SELECT password_hash FROM users WHERE tenant_id=$1 AND id=$2 AND status='active' AND deleted_at IS NULL",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(Error::Unauthorized)?
            .get(0);
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
                        "UPDATE users SET password_hash=$3 WHERE tenant_id=$1 AND id=$2 AND password_hash=$4",
                        &[&tenant, &user, &replacement, &stored],
                    )
                    .await?;
                if changed != 1 {
                    return Err(Error::Conflict);
                }

                sql.execute(
                    "DELETE FROM password_reset_tokens WHERE tenant_id=$1 AND user_id=$2",
                    &[&tenant, &user],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "user".into(),
                        id: user,
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
    /// public form gives one response. The mail is sent after this returns, so
    /// response time does not depend on whether an address exists.
    pub async fn request_password_reset(&self, email: &str) -> Result<(), CredentialError> {
        let email = normalize_email(email)?;
        let client = self.db.get().await.map_err(Error::from)?;
        let candidates = client
            .query(
                "SELECT u.id,u.tenant_id,u.name,u.email FROM users u JOIN tenants t ON t.id=u.tenant_id WHERE lower(u.email)=$1 AND u.deleted_at IS NULL AND u.status='active' AND t.deleted_at IS NULL AND t.status='active' LIMIT 2",
                &[&email],
            )
            .await
            .map_err(Error::from)?;
        // Ambiguity is treated as unknown rather than guessed; a frozen account
        // (archived company, retired identity) is unknown here too.
        let [account] = &candidates[..] else {
            password::verify("", None).await?;
            return Ok(());
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
            password::verify("", None).await?;
            return Ok(());
        }
        let token = random_token()?;
        let claimed = client
            .query_opt(
                "INSERT INTO password_reset_tokens(tenant_id,user_id,token_hash,created_at,expires_at) VALUES($1,$2,$3,now(),now()+make_interval(secs => $4::double precision)) ON CONFLICT (tenant_id,user_id) DO UPDATE SET token_hash=excluded.token_hash,created_at=excluded.created_at,expires_at=excluded.expires_at WHERE password_reset_tokens.created_at<=now()-make_interval(secs => $5::double precision) RETURNING user_id",
                &[&tenant, &user, &digest(&token), &RESET_LIFETIME_SECONDS, &RESET_THROTTLE_SECONDS],
            )
            .await
            .map_err(Error::from)?;
        if claimed.is_none() {
            return Ok(());
        }
        // Housekeeping keeps storage bounded without a scheduler.
        client
            .execute(
                "DELETE FROM password_reset_tokens WHERE expires_at<now()",
                &[],
            )
            .await
            .map_err(Error::from)?;
        let message = Message {
            to: Address::new(Some(&name), &address).map_err(|_| Error::Invalid)?,
            subject: "Reset your Bilibi password".into(),
            text: format!(
                "Reset your Bilibi password by opening this link:\n\n{}\n\nIf you did not request this, you can ignore this email.\n",
                self.reset_link(&address, &token)
            ),
        };
        let (mailer, db, hash) = (self.mailer.clone(), self.db.clone(), digest(&token));
        tokio::spawn(async move {
            if let Err(error) = mailer.deliver(&message).await {
                eprintln!("mail: password reset for user {user} was not delivered: {error}");
                // Release the throttle so the person can ask again.
                if let Ok(client) = db.get().await {
                    let _ = client
                        .execute(
                            "DELETE FROM password_reset_tokens WHERE tenant_id=$1 AND user_id=$2 AND token_hash=$3",
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

    /// Consumes the one-time token, replaces the password and ends every
    /// session of the account. Unknown, frozen, expired and reused tokens are
    /// indistinguishable.
    pub async fn reset_password(
        &self,
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
                "SELECT u.id,u.tenant_id,r.token_hash FROM users u JOIN tenants t ON t.id=u.tenant_id LEFT JOIN password_reset_tokens r ON r.tenant_id=u.tenant_id AND r.user_id=u.id AND r.expires_at>now() WHERE lower(u.email)=$1 AND u.deleted_at IS NULL AND u.status='active' AND t.deleted_at IS NULL AND t.status='active' LIMIT 2",
                &[&email],
            )
            .await
            .map_err(Error::from)?;
        let presented = digest(token);
        let user = match &account[..] {
            [row] => row
                .get::<_, Option<Vec<u8>>>(2)
                .filter(|stored| bool::from(stored.ct_eq(&presented)))
                .map(|_| (row.get::<_, i64>(0), row.get::<_, i64>(1))),
            _ => None,
        };
        let Some((user, tenant)) = user else {
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
                            "DELETE FROM password_reset_tokens WHERE tenant_id=$1 AND user_id=$2 AND token_hash=$3 AND expires_at>now()",
                            &[&tenant, &user, &presented],
                        )
                        .await?;
                    if claimed != 1 {
                        return Err(Error::NotFound);
                    }
                    sql.execute(
                        "UPDATE users SET password_hash=$3 WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &user, &replacement],
                    )
                    .await?;

                    Ok((
                        (),
                        vec![Change {
                            kind: "user".into(),
                            id: user,
                            old: json!({"password_hash":"previous"}),
                            new: json!({"password_hash":"replaced"}),
                        }],
                    ))
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
