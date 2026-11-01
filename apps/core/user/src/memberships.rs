//! Tenant membership invitations for issue 147. This mirrors Bilimbi's user
//! invitation boundary while keeping the invited account tenant-owned and the
//! accepted sign-in installation-wide.
use crate::{
    accounts::{field_error, validate_email, validate_name, FieldError},
    contributions::INVITE,
    password::{self, check_policy},
    UserService,
};
use bilibi_database::{Change, Context, Error, Result};
use bilibi_mail::{Address, Message};
use bilibi_security::Scope;
use bilibi_settings::Target;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const INVITATION_DAYS_KEY: &str = "user.invitation_days";

#[derive(Clone, Debug, Default)]
pub struct InvitationInput {
    pub name: String,
    pub email: String,
    /// Authorization roles to preassign to the pending tenant account.
    pub roles: Vec<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invitation {
    pub id: i64,
    pub user: i64,
    pub email: String,
}

/// The facts the emailed acceptance page may reveal to the bearer. The
/// address itself stays private; the page only needs to choose between the
/// existing sign-in and new-password flows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvitationAcceptance {
    pub tenant_name: String,
    pub existing_sign_in: bool,
}

pub(crate) fn validate_invitation_input(input: &InvitationInput) -> Vec<FieldError> {
    let mut errors = Vec::new();
    errors.extend(validate_name(&input.name));
    errors.extend(validate_email(&input.email).err());
    errors
}

#[derive(Debug, thiserror::Error)]
pub enum InvitationError {
    #[error("Invalid invitation fields")]
    Validation(Vec<FieldError>),
    #[error("This invitation is invalid or has expired")]
    Link,
    #[error(transparent)]
    Access(#[from] Error),
}

impl From<tokio_postgres::Error> for InvitationError {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::from(error).into()
    }
}

fn digest(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn token_shaped(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl UserService {
    /// Reads an open invitation for its emailed acceptance page. Invalid,
    /// expired, used and revoked links have the same answer.
    pub async fn member_invitation(
        &self,
        raw_token: &str,
    ) -> std::result::Result<InvitationAcceptance, InvitationError> {
        if !token_shaped(raw_token) {
            return Err(InvitationError::Link);
        }
        let client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT t.name,EXISTS(SELECT 1 FROM user_identities d WHERE lower(d.email)=v.email AND d.kind='staff' AND d.status='active' AND d.deleted_at IS NULL) FROM user_invitations v JOIN users u ON u.tenant_id=v.tenant_id AND u.id=v.user_id JOIN tenants t ON t.id=v.tenant_id AND t.status='active' AND t.deleted_at IS NULL WHERE v.token_hash=$1 AND v.accepted_at IS NULL AND v.revoked_at IS NULL AND v.expires_at>clock_timestamp() AND u.status='invited' AND u.identity_id IS NULL AND u.deleted_at IS NULL",
                &[&digest(raw_token)],
            )
            .await?
            .ok_or(InvitationError::Link)?;
        Ok(InvitationAcceptance {
            tenant_name: row.get(0),
            existing_sign_in: row.get(1),
        })
    }

    /// Prepares one tenant-owned account and emails a one-time invitation.
    /// The response never reveals whether the address already has a sign-in.
    pub async fn invite_member(
        &self,
        scope: &Scope,
        input: &InvitationInput,
    ) -> std::result::Result<Invitation, InvitationError> {
        let mut errors = Vec::new();
        errors.extend(validate_name(&input.name));
        let email = match validate_email(&input.email) {
            Ok(email) => Some(email),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        if !errors.is_empty() {
            return Err(InvitationError::Validation(errors));
        }
        if input.roles.len() > 100
            || input.roles.iter().any(|role| *role <= 0)
            || input
                .roles
                .iter()
                .enumerate()
                .any(|(at, role)| input.roles[..at].contains(role))
        {
            return Err(InvitationError::Access(Error::Invalid));
        }
        let (name, email) = (input.name.trim().to_owned(), email.expect("validated"));
        let raw_token = token()?;
        let token_hash = digest(&raw_token);
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.invitation.create"),
            )
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        bilibi_authz::authorize_key(tx.sql(), &scope, INVITE).await?;
        let days = self
            .settings
            .get(tx.sql(), &scope, Target::Tenant, INVITATION_DAYS_KEY)
            .await?
            .value
            .as_i64()
            .unwrap_or(7)
            .clamp(1, 60) as i32;
        let tenant = scope.tenant();
        let inviter = scope.user();
        if tx
            .sql()
            .query_opt(
                "SELECT 1 FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.kind='staff' AND i.deleted_at IS NULL WHERE u.tenant_id=$1 AND lower(i.email)=$2 AND u.deleted_at IS NULL",
                &[&tenant, &email],
            )
            .await?
            .is_some()
        {
            return Err(InvitationError::Validation(vec![field_error(
                "email",
                "already belongs to this workspace",
            )]));
        }
        let tenant_name: String = tx
            .sql()
            .query_one(
                "SELECT name FROM tenants WHERE id=$1 AND status='active' AND deleted_at IS NULL FOR SHARE",
                &[&tenant],
            )
            .await?
            .get(0);
        let (insert_name, insert_email) = (name.clone(), email.clone());
        let invitation = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    for row in sql
                        .query(
                            "UPDATE user_invitations SET revoked_at=now() WHERE tenant_id=$1 AND email=$2 AND accepted_at IS NULL AND revoked_at IS NULL RETURNING id,user_id",
                            &[&tenant, &insert_email],
                        )
                        .await?
                    {
                        let invitation_id: i64 = row.get(0);
                        let old_user: i64 = row.get(1);
                        sql.execute(
                            "UPDATE users SET status='deleted',deleted_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND status='invited' AND identity_id IS NULL",
                            &[&tenant, &old_user],
                        )
                        .await?;
                        changes.push(Change {
                            kind: "user.invitation".into(),
                            id: invitation_id,
                            old: json!({"status": "pending"}),
                            new: json!({"status": "revoked", "reason": "replaced"}),
                        });
                        changes.push(Change {
                            kind: "user".into(),
                            id: old_user,
                            old: json!({"status": "invited"}),
                            new: json!({"status": "deleted", "reason": "invitation_replaced"}),
                        });
                    }
                    let user: i64 = sql
                        .query_one(
                            "INSERT INTO users(tenant_id,name,status) VALUES($1,$2,'invited') RETURNING id",
                            &[&tenant, &insert_name],
                        )
                        .await?
                        .get(0);
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO user_invitations(tenant_id,user_id,email,token_hash,invited_by,expires_at) VALUES($1,$2,$3,$4,$5,now()+make_interval(days=>$6::int)) RETURNING id",
                            &[&tenant, &user, &insert_email, &token_hash, &inviter, &days],
                        )
                        .await?
                        .get(0);
                    changes.push(Change {
                        kind: "user".into(),
                        id: user,
                        old: json!(null),
                        new: json!({"name": insert_name, "status": "invited"}),
                    });
                    changes.push(Change {
                        kind: "user.invitation".into(),
                        id,
                        old: json!(null),
                        new: json!({"user_id": user, "email": insert_email, "status": "pending"}),
                    });
                    Ok((Invitation { id, user, email: insert_email }, changes))
                })
            })
            .await?;
        for role in &input.roles {
            self.sso_grants
                .assign_role_with_in(
                    &mut tx,
                    &scope,
                    invitation.user,
                    *role,
                    true,
                    &bilibi_authz::grants::Assignment::default(),
                )
                .await?;
        }
        let message = Message::text(
            Address::new(Some(&name), &email).map_err(|_| Error::Invalid)?,
            format!("You're invited to {tenant_name}"),
            format!(
                "You are invited to join {tenant_name}.\n\nOpen this link to accept:\n\n{}/accept/{raw_token}\n\nThe link works once and expires in {days} day(s). If you did not expect this, you can ignore this email.\n",
                self.origin
            ),
        );
        let (mailer, id) = (self.mailer.clone(), invitation.id);
        tx.after_commit(move || {
            tokio::spawn(async move {
                if let Err(error) = mailer.deliver(&message).await {
                    eprintln!("mail: user invitation {id} was not delivered: {error}");
                }
            });
        });
        tx.commit().await?;
        Ok(invitation)
    }

    /// Revokes one open invitation and removes its still-pending tenant
    /// account. Accepted, already-revoked and unknown invitations are the
    /// same not-found answer.
    pub async fn revoke_member_invitation(
        &self,
        scope: &Scope,
        invitation: i64,
    ) -> std::result::Result<(), InvitationError> {
        if invitation <= 0 {
            return Err(InvitationError::Link);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.invitation.revoke"),
            )
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        bilibi_authz::authorize_key(tx.sql(), &scope, INVITE).await?;
        let tenant = scope.tenant();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_opt(
                        "UPDATE user_invitations SET revoked_at=now() WHERE tenant_id=$1 AND id=$2 AND accepted_at IS NULL AND revoked_at IS NULL RETURNING user_id",
                        &[&tenant, &invitation],
                    )
                    .await?
                    .ok_or(Error::NotFound)?;
                let user: i64 = row.get(0);
                let changed = sql
                    .execute(
                        "UPDATE users SET status='deleted',deleted_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND status='invited' AND identity_id IS NULL AND deleted_at IS NULL",
                        &[&tenant, &user],
                    )
                    .await?;
                if changed != 1 {
                    return Err(Error::NotFound);
                }
                Ok((
                    (),
                    vec![
                        Change {
                            kind: "user.invitation".into(),
                            id: invitation,
                            old: json!({"status": "pending"}),
                            new: json!({"status": "revoked"}),
                        },
                        Change {
                            kind: "user".into(),
                            id: user,
                            old: json!({"status": "invited"}),
                            new: json!({"status": "deleted", "reason": "invitation_revoked"}),
                        },
                    ],
                ))
            })
        })
        .await
        .map_err(|error| match error {
            Error::NotFound => InvitationError::Link,
            other => InvitationError::Access(other),
        })?;
        tx.commit().await?;
        Ok(())
    }

    /// Accepts an invitation with the already authenticated platform sign-in.
    /// The address must match, but the target tenant never learns the other
    /// membership through the result or its audit payload.
    pub async fn accept_member_invitation(
        &self,
        signed_in: &Scope,
        raw_token: &str,
    ) -> std::result::Result<i64, InvitationError> {
        if !token_shaped(raw_token) {
            return Err(InvitationError::Link);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let signed_in = self.fresh(&**client, signed_in).await?;
        let identity: i64 = client
            .query_opt(
                "SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2 AND status='active' AND deleted_at IS NULL",
                &[&signed_in.tenant(), &signed_in.user()],
            )
            .await?
            .and_then(|row| row.get(0))
            .ok_or(InvitationError::Link)?;
        self.accept_with_identity(&mut client, raw_token, identity)
            .await
    }

    /// Accepts an invitation for a new address and creates its home sign-in.
    pub async fn accept_new_member_invitation(
        &self,
        raw_token: &str,
        password: &str,
    ) -> std::result::Result<i64, InvitationError> {
        if !token_shaped(raw_token) {
            return Err(InvitationError::Link);
        }
        if let Err(error) = check_policy(password) {
            return Err(InvitationError::Validation(vec![field_error(
                "password",
                error.to_string(),
            )]));
        }
        let hash = password::hash(password).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT i.tenant_id,i.user_id,i.email FROM user_invitations i JOIN users u ON u.tenant_id=i.tenant_id AND u.id=i.user_id WHERE i.token_hash=$1 AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at>clock_timestamp() AND u.status='invited' AND u.identity_id IS NULL AND u.deleted_at IS NULL",
                &[&digest(raw_token)],
            )
            .await?
            .ok_or(InvitationError::Link)?;
        let (tenant, user, email): (i64, i64, String) = (row.get(0), row.get(1), row.get(2));
        if client
            .query_opt(
                "SELECT 1 FROM user_identities WHERE lower(email)=$1 AND kind='staff' AND deleted_at IS NULL",
                &[&email],
            )
            .await?
            .is_some()
        {
            return Err(InvitationError::Link);
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::user(tenant, user, "user.invitation.accept"),
            )
            .await?;
        if let Err(error) =
            finish_new_acceptance(&mut tx, raw_token, tenant, user, email, hash).await
        {
            let refusal = match error {
                Error::NotFound => Err(InvitationError::Link),
                other => Err(other.into()),
            };
            tx.rollback().await?;
            return refusal;
        }
        tx.commit().await?;
        Ok(user)
    }

    async fn accept_with_identity(
        &self,
        client: &mut tokio_postgres::Client,
        raw_token: &str,
        identity: i64,
    ) -> std::result::Result<i64, InvitationError> {
        let row = client
            .query_opt(
                "SELECT v.tenant_id,v.user_id FROM user_invitations v JOIN users u ON u.tenant_id=v.tenant_id AND u.id=v.user_id JOIN user_identities i ON i.id=$2 AND lower(i.email)=v.email AND i.kind='staff' AND i.status='active' AND i.deleted_at IS NULL WHERE v.token_hash=$1 AND v.accepted_at IS NULL AND v.revoked_at IS NULL AND v.expires_at>clock_timestamp() AND u.status='invited' AND u.identity_id IS NULL AND u.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM users m WHERE m.tenant_id=v.tenant_id AND m.identity_id=i.id AND m.deleted_at IS NULL)",
                &[&digest(raw_token), &identity],
            )
            .await?
            .ok_or(InvitationError::Link)?;
        let (tenant, user): (i64, i64) = (row.get(0), row.get(1));
        let mut tx = self
            .mutations
            .begin(
                client,
                Context::user(tenant, user, "user.invitation.accept"),
            )
            .await?;
        if let Err(error) = finish_acceptance(&mut tx, raw_token, tenant, user, identity).await {
            let refusal = match error {
                Error::NotFound => Err(InvitationError::Link),
                other => Err(other.into()),
            };
            tx.rollback().await?;
            return refusal;
        }
        tx.commit().await?;
        Ok(user)
    }
}

async fn finish_new_acceptance(
    tx: &mut bilibi_database::MutationTransaction<'_>,
    raw_token: &str,
    tenant: i64,
    user: i64,
    email: String,
    hash: String,
) -> Result<()> {
    let token_hash = digest(raw_token);
    tx.mutate(move |sql| {
        Box::pin(async move {
            let invitation: i64 = sql
                .query_opt(
                    "UPDATE user_invitations SET accepted_at=now() WHERE tenant_id=$1 AND user_id=$2 AND token_hash=$3 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp() RETURNING id",
                    &[&tenant, &user, &token_hash],
                )
                .await?
                .ok_or(Error::NotFound)?
                .get(0);
            let identity: i64 = sql
                .query_one(
                    "INSERT INTO user_identities(kind,email,password_hash,email_verified_at,home_tenant_id,home_user_id,last_tenant_id) VALUES('staff',$1,$2,now(),$3,$4,$3) RETURNING id",
                    &[&email, &hash, &tenant, &user],
                )
                .await?
                .get(0);
            let changed = sql
                .execute(
                    "UPDATE users SET identity_id=$3,status='active',version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND status='invited' AND identity_id IS NULL AND deleted_at IS NULL",
                    &[&tenant, &user, &identity],
                )
                .await?;
            if changed != 1 {
                return Err(Error::NotFound);
            }
            Ok((
                (),
                vec![
                    Change {
                        kind: "user_identity".into(),
                        id: identity,
                        old: json!(null),
                        new: json!({
                            "kind": "staff",
                            "email": email,
                            "password_hash": "set",
                            "home_tenant_id": tenant,
                            "home_user_id": user,
                        }),
                    },
                    Change {
                        kind: "user".into(),
                        id: user,
                        old: json!({"status": "invited"}),
                        new: json!({"status": "active", "identity_id": identity}),
                    },
                    Change {
                        kind: "user.invitation".into(),
                        id: invitation,
                        old: json!({"status": "pending"}),
                        new: json!({"status": "accepted"}),
                    },
                ],
            ))
        })
    })
    .await
}

async fn finish_acceptance(
    tx: &mut bilibi_database::MutationTransaction<'_>,
    raw_token: &str,
    tenant: i64,
    user: i64,
    identity: i64,
) -> Result<()> {
    let token_hash = digest(raw_token);
    tx.mutate(move |sql| {
        Box::pin(async move {
            let invitation: i64 = sql
                .query_opt(
                    "UPDATE user_invitations SET accepted_at=now() WHERE tenant_id=$1 AND user_id=$2 AND token_hash=$3 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp() RETURNING id",
                    &[&tenant, &user, &token_hash],
                )
                .await?
                .ok_or(Error::NotFound)?
                .get(0);
            let changed = sql
                .execute(
                    "UPDATE users SET identity_id=$3,status='active',version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND status='invited' AND identity_id IS NULL AND deleted_at IS NULL",
                    &[&tenant, &user, &identity],
                )
                .await?;
            if changed != 1 {
                return Err(Error::NotFound);
            }
            Ok((
                (),
                vec![
                    Change {
                        kind: "user".into(),
                        id: user,
                        old: json!({"status": "invited"}),
                        new: json!({"status": "active", "identity_id": identity}),
                    },
                    Change {
                        kind: "user.invitation".into(),
                        id: invitation,
                        old: json!({"status": "pending"}),
                        new: json!({"status": "accepted"}),
                    },
                ],
            ))
        })
    })
    .await
}
