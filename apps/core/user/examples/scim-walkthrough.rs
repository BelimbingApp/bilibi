//! Disposable SCIM fixture and session admission proof; extends Bilimbi User
//! walkthroughs. The protocol itself is driven by scripts/scim-conformance.py.
use bilibi_database::pool;
use bilibi_user::{IdentityConfiguration, UserService};
use serde_json::json;
use std::{io::Write, os::unix::fs::OpenOptionsExt};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_SCIM_WALKTHROUGH").as_deref() != Ok("disposable") {
        return Err(
            "Set BILIBI_SCIM_WALKTHROUGH=disposable with a freshly migrated disposable database."
                .into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN")?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let users = app.services.get::<UserService>("core/user.accounts")?;
    let login = users
        .login("ai@agent.my", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    if login.pending {
        return Err("The disposable administrator unexpectedly requires a second factor.".into());
    }
    let scope = app.security.guard(&login.token).await?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("inspect") {
        let email = args
            .get(1)
            .ok_or("Provide the provisioned sign-in email.")?;
        let client = db.get().await?;
        let row = client.query_one("SELECT u.id,u.name,p.name,p.email,a.work_email FROM user_scim_users s JOIN users u ON u.tenant_id=s.tenant_id AND u.id=s.user_id JOIN user_identities i ON i.id=u.identity_id JOIN persons p ON p.tenant_id=u.tenant_id AND p.id=s.person_id JOIN person_affiliations a ON a.tenant_id=u.tenant_id AND a.id=s.affiliation_id WHERE s.tenant_id=$1 AND i.email=$2", &[&scope.tenant(),email]).await?;
        let user: i64 = row.get(0);
        let roles = client.query("SELECT r.code FROM effective_principal_roles e JOIN roles r ON r.tenant_id=e.tenant_id AND r.id=e.role_id WHERE e.tenant_id=$1 AND e.user_id=$2 ORDER BY r.code", &[&scope.tenant(),&user]).await?.iter().map(|r|r.get::<_,String>(0)).collect::<Vec<_>>();
        println!(
            "{}",
            json!({"roles":roles,"account_name":row.get::<_,String>(1),"person_name":row.get::<_,String>(2),"person_email":row.get::<_,String>(3),"work_email":row.get::<_,String>(4)})
        );
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("session") {
        let email = args
            .get(1)
            .ok_or("Provide the provisioned sign-in email.")?;
        let path = args
            .get(2)
            .ok_or("Provide a new private session scratch file.")?;
        let client = db.get().await?;
        let user:i64=client.query_one("SELECT u.id FROM users u JOIN user_identities i ON i.id=u.identity_id JOIN user_scim_users s ON s.tenant_id=u.tenant_id AND s.user_id=u.id WHERE u.tenant_id=$1 AND i.email=$2 AND s.active",&[&scope.tenant(),email]).await?.get(0);
        drop(client);
        let password = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        users.set_password(&scope, user, &password).await?;
        let session = users.login(email, &password).await?;
        if session.pending {
            return Err("A real admitted session is required for the revocation proof.".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(
            serde_json::to_string(
                &json!({"cookie":format!("bilibi_session={}",session.token),"csrf":session.csrf}),
            )?
            .as_bytes(),
        )?;
        println!("Provisioned account admitted; private session scratch written.");
        return Ok(());
    }
    let settings = app
        .services
        .get::<bilibi_settings::Settings>("base/settings.store")?;
    let mut client = db.get().await?;
    let mut tx = users
        .mutations
        .begin(&mut client, scope.mutation_context("walkthrough.instance"))
        .await?;
    settings
        .put(
            &mut tx,
            &scope,
            bilibi_settings::Target::Global,
            "instance.role",
            json!("development"),
        )
        .await?;
    tx.commit().await?;
    let staff: i64 = client
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=$1 AND code='staff'",
            &[&scope.tenant()],
        )
        .await?
        .get(0);
    let viewer: i64 = client
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=$1 AND code='viewer'",
            &[&scope.tenant()],
        )
        .await?
        .get(0);
    drop(client);
    for company in [1, 2] {
        let configuration = IdentityConfiguration {
            label: format!("Provisioning company {company}"),
            protocol: json!({"issuer":"https://identity.example.test","client_id":"bilibi"}),
            groups_claim: "groups".into(),
            default_role: Some(viewer),
            role_mapping: serde_json::from_value(json!({"scim-operators":[staff]}))?,
            ..Default::default()
        };
        let (id, public) = users
            .save_identity_configuration(&scope, company, None, configuration, None)
            .await?;
        println!("company={company} connection={id} admin={origin}/companies/{company}/sso?connection={id} base={origin}/scim/v2/{public}");
    }
    println!("No SCIM token exists. Create tokens in the browser to activate provisioning.");
    Ok(())
}
