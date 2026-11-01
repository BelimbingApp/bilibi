//! Manual proof of issue 147's invitation and acceptance service against a
//! migrated disposable database. It follows emailed links from the dev mailbox.
use bilibi_database::{pool, Error, Result};
use bilibi_mail::Mailbox;
use bilibi_user::{AccountError, InvitationError, InvitationInput, UserService};
use std::{path::PathBuf, time::Duration};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

fn mailbox_dir() -> PathBuf {
    std::env::var("BILIBI_MAIL_DIR")
        .unwrap_or_else(|_| ".local/mailbox".into())
        .into()
}

fn count(mailbox: &Mailbox) -> usize {
    mailbox.entries().map(|entries| entries.len()).unwrap_or(0)
}

async fn invitation_token(mailbox: &Mailbox, before: usize) -> Result<String> {
    for _ in 0..50 {
        if count(mailbox) > before {
            let newest = mailbox
                .entries()
                .map_err(|_| Error::Internal)?
                .into_iter()
                .next()
                .ok_or(Error::Internal)?;
            let message = mailbox.read(&newest.id).map_err(|_| Error::Internal)?;
            let link = message
                .lines()
                .find(|line| line.starts_with("http"))
                .ok_or(Error::Internal)?;
            return link
                .rsplit('/')
                .next()
                .filter(|token| token.len() == 64)
                .map(str::to_owned)
                .ok_or(Error::Internal);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(Error::Internal)
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_MEMBERSHIPS_DEMO").as_deref() != Ok("disposable") {
        return Err(Error::Invalid);
    }
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(
        db.clone(),
        &std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?,
    )?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let users = app.services.get::<UserService>("core/user.accounts")?;
    let mailbox = Mailbox::new(mailbox_dir(), 100);
    let client = db.get().await?;
    for capability in [
        "admin.user.invite",
        "admin.user.update",
        "admin.user.delete",
    ] {
        client
            .execute(
                "INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(2,5,$1,true) ON CONFLICT(tenant_id,user_id,capability) DO UPDATE SET is_allowed=true",
                &[&capability],
            )
            .await?;
    }

    let inviter_login = users
        .login(
            "manager@other.example.test",
            bilibi_walkthrough_fixture::PASSWORD,
        )
        .await
        .map_err(|_| Error::Unauthorized)?;
    let inviter = app.security.guard(&inviter_login.token).await?;
    let existing_login = users
        .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await
        .map_err(|_| Error::Unauthorized)?;
    let existing = app.security.guard(&existing_login.token).await?;

    let before = count(&mailbox);
    let invitation = users
        .invite_member(
            &inviter,
            &InvitationInput {
                name: "Ada in the other workspace".into(),
                email: "manager@example.test".into(),
                roles: Vec::new(),
            },
        )
        .await
        .map_err(|_| Error::Invalid)?;
    let existing_token = invitation_token(&mailbox, before).await?;
    let joined = users
        .accept_member_invitation(&existing, &existing_token)
        .await
        .map_err(|_| Error::Invalid)?;
    let identity_count: i64 = client
        .query_one(
            "SELECT count(DISTINCT identity_id) FROM users WHERE id IN ($1,$2) AND tenant_id IN (1,2)",
            &[&existing.user(), &joined],
        )
        .await?
        .get(0);
    println!(
        "Existing sign-in joined tenant 2: user={} one_identity={}",
        joined,
        identity_count == 1
    );
    if invitation.email != "manager@example.test" || identity_count != 1 {
        return Err(Error::Internal);
    }
    if !matches!(
        users.set_password(&inviter, joined, "outside-reset").await,
        Err(AccountError::Access(Error::Forbidden))
    ) {
        return Err(Error::Internal);
    }
    users
        .delete_account(&inviter, joined)
        .await
        .map_err(|_| Error::Invalid)?;
    users
        .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await
        .map_err(|_| Error::Unauthorized)?;
    println!("Outside tenant cannot reset credentials; removal keeps the home sign-in active");

    let before = count(&mailbox);
    let new_invitation = users
        .invite_member(
            &inviter,
            &InvitationInput {
                name: "New Member".into(),
                email: "new-member@example.test".into(),
                roles: Vec::new(),
            },
        )
        .await
        .map_err(|_| Error::Invalid)?;
    let new_token = invitation_token(&mailbox, before).await?;
    let new_user = users
        .accept_new_member_invitation(&new_token, "new-member-password")
        .await
        .map_err(|_| Error::Invalid)?;
    let login = users
        .login("new-member@example.test", "new-member-password")
        .await
        .map_err(|_| Error::Unauthorized)?;
    let scope = app.security.guard(&login.token).await?;
    println!(
        "New sign-in home: tenant={} user={} invited_user={}",
        scope.tenant(),
        scope.user(),
        new_invitation.user
    );
    if scope.tenant() != 2 || new_user != new_invitation.user || scope.user() != new_user {
        return Err(Error::Internal);
    }
    if !matches!(
        users
            .accept_new_member_invitation(&new_token, "new-member-password")
            .await,
        Err(InvitationError::Link)
    ) {
        return Err(Error::Internal);
    }
    let stored_plaintext: bool = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM user_invitations WHERE token_hash=$1::text::bytea)",
            &[&new_token],
        )
        .await?
        .get(0);
    println!("Token one-time and digest-only: reused=false plaintext_stored={stored_plaintext}");
    if stored_plaintext {
        return Err(Error::Internal);
    }
    Ok(())
}
