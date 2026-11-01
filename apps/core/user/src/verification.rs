//! Signed, expiring email-verification links. The MAC covers the account's
//! current email, so changing the address voids every link already sent.
//! Signing needs `BILIBI_USER_SIGNING_KEY` (64 hex characters), kept apart from
//! the Settings and Media keys; without it verification is unavailable.
use crate::{CredentialError, UserService};
use bilibi_database::{Change, Context, Error};
use bilibi_mail::{Address, Message};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;

const MAX_AGE_SECONDS: i64 = 3600;

/// No Debug: the key must never reach a log.
pub struct Signer {
    key: Option<Vec<u8>>,
}
#[derive(Debug, Eq, PartialEq)]
pub enum Verification {
    Verified,
    AlreadyVerified,
}
impl Signer {
    pub fn from_hex(key: Option<&str>) -> Self {
        let key = key
            .filter(|key| key.len() == 64 && key.is_ascii())
            .and_then(|key| {
                (0..32)
                    .map(|index| u8::from_str_radix(&key[index * 2..index * 2 + 2], 16).ok())
                    .collect()
            });
        Self { key }
    }
    fn mac(
        &self,
        tenant: i64,
        user: i64,
        email: &str,
        issued: i64,
    ) -> Result<Hmac<Sha256>, CredentialError> {
        let key = self
            .key
            .as_deref()
            .ok_or(CredentialError::VerificationUnavailable)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| CredentialError::VerificationUnavailable)?;
        mac.update(
            format!("bilibi-user-email-verification-v1|{tenant}|{user}|{email}|{issued}")
                .as_bytes(),
        );
        Ok(mac)
    }
    fn sign(
        &self,
        tenant: i64,
        user: i64,
        email: &str,
        issued: i64,
    ) -> Result<String, CredentialError> {
        let tag = self
            .mac(tenant, user, email, issued)?
            .finalize()
            .into_bytes();
        let tag: String = tag.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(format!("v1.{tenant}.{user}.{issued}.{tag}"))
    }
}
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    (text.len().is_multiple_of(2) && text.is_ascii())
        .then(|| {
            (0..text.len() / 2)
                .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok())
                .collect()
        })
        .flatten()
}

impl UserService {
    /// Emails the signed-in account a verification link for its current address.
    pub async fn send_email_verification(
        &self,
        token: &str,
        csrf: &str,
    ) -> Result<(), CredentialError> {
        let client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        let (tenant, user) = (scope.tenant(), scope.user());
        let row = client
            .query_opt(
                "SELECT u.name,i.email,i.email_verified_at IS NOT NULL FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.status='active' AND i.deleted_at IS NULL WHERE u.tenant_id=$1 AND u.id=$2 AND u.status='active' AND u.deleted_at IS NULL",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(Error::Unauthorized)?;
        let (name, email, verified): (String, String, bool) = (row.get(0), row.get(1), row.get(2));
        if verified {
            return Err(CredentialError::AlreadyVerified);
        }
        let issued = time_now(&client).await?;
        let link = format!(
            "{}/verify-email/{}",
            self.origin,
            self.signer.sign(tenant, user, &email, issued)?
        );
        let message = Message::text(
            Address::new(Some(&name), &email).map_err(|_| Error::Invalid)?,
            "Verify your Bilibi email address",
            format!("Verify your email address by opening this link:\n\n{link}\n\nThe link expires in one hour. If you did not request this, you can ignore this email.\n"),
        );
        self.mailer
            .deliver(&message)
            .await
            .map(|_| ())
            .map_err(|error| {
                eprintln!("mail: verification for user {user} was not delivered: {error}");
                CredentialError::Delivery
            })
    }

    /// The link proves control of the mailbox, so no sign-in is required.
    pub async fn verify_email(&self, link: &str) -> Result<Verification, CredentialError> {
        let invalid = || CredentialError::InvalidOrExpiredToken;
        let parts: Vec<_> = link.split('.').collect();
        let [version, tenant, user, issued, tag] = parts[..] else {
            return Err(invalid());
        };
        let (tenant, user, issued): (i64, i64, i64) = (
            tenant.parse().map_err(|_| invalid())?,
            user.parse().map_err(|_| invalid())?,
            issued.parse().map_err(|_| invalid())?,
        );
        let tag = decode_hex(tag).ok_or_else(invalid)?;
        if version != "v1" || tenant <= 0 || user <= 0 {
            return Err(invalid());
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT i.email,i.email_verified_at IS NOT NULL,extract(epoch from now())::bigint FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.status='active' AND i.deleted_at IS NULL JOIN tenants t ON t.id=u.tenant_id WHERE u.tenant_id=$1 AND u.id=$2 AND u.status='active' AND u.deleted_at IS NULL AND t.deleted_at IS NULL AND t.status='active'",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or_else(invalid)?;
        let (email, verified, now): (String, bool, i64) = (row.get(0), row.get(1), row.get(2));
        // MAC first, in constant time; age is reported only for authentic links.
        self.signer
            .mac(tenant, user, &email, issued)?
            .verify_slice(&tag)
            .map_err(|_| invalid())?;
        if now - issued > MAX_AGE_SECONDS || issued > now + 60 {
            return Err(invalid());
        }
        if verified {
            return Ok(Verification::AlreadyVerified);
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::user(tenant, user, "user.verify_email"),
            )
            .await?;
        let identity = crate::sign_in_identity::id(tx.sql(), tenant, user).await?;
        let marked = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let changed = sql
                        .execute(
                            "UPDATE user_identities SET email_verified_at=now(),updated_at=now() WHERE id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND email_verified_at IS NULL",
                            &[&tenant, &user],
                        )
                        .await?;
                    if changed != 1 {
                        return Err(Error::NotFound);
                    }
                    Ok((
                        (),
                        vec![Change {
                            kind: "user_identity".into(),
                            id: identity,
                            old: json!({"email_verified":false}),
                            new: json!({"email_verified":true}),
                        }],
                    ))
                })
            })
            .await;
        match marked {
            Ok(()) => {
                tx.commit().await?;
                Ok(Verification::Verified)
            }
            // A concurrent click verified it first.
            Err(Error::NotFound) => Ok(Verification::AlreadyVerified),
            Err(error) => Err(error.into()),
        }
    }
}

async fn time_now(client: &tokio_postgres::Client) -> Result<i64, CredentialError> {
    Ok(client
        .query_one("SELECT extract(epoch from now())::bigint", &[])
        .await
        .map_err(Error::from)?
        .get(0))
}
