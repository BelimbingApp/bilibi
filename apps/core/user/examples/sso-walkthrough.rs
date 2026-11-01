//! Install the repository walkthrough fixture into a migrated disposable DB.
//! Exercise company-scoped SSO configuration through the real facade; run the
//! local Keycloak and browser steps in ../sso.md. No Bilimbi equivalent.
use bilibi_database::pool;
use bilibi_user::{IdentityConfiguration, UserService};
use serde_json::json;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
// Extend only the disposable fixture's manager role, never product roles.
inventory::submit! {bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.sso",bilibi_module_registry::Provenance{module:"base/authz",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution{domains:&[],verbs:&[],capabilities:&[],roles:&[bilibi_authz::RoleDefinition{code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&["admin.user.sso.manage","admin.authz.role.update","admin.authz.role.list"]}],system_principals:&[]})}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_SSO_WALKTHROUGH").as_deref() != Ok("disposable") {
        return Err(
            "set BILIBI_SSO_WALKTHROUGH=disposable with a fresh migrated DATABASE_URL".into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN")?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let users = app.services.get::<UserService>("core/user.accounts")?;
    let mut login = if let Ok(token) = std::env::var("BILIBI_SSO_SESSION_TOKEN") {
        bilibi_user::Login {
            token,
            csrf: String::new(),
            pending: false,
        }
    } else {
        users
            .login(
                &std::env::var("BILIBI_SSO_ADMIN_EMAIL").unwrap_or_else(|_| "ai@agent.my".into()),
                bilibi_walkthrough_fixture::PASSWORD,
            )
            .await?
    };
    if login.pending {
        login = users
            .complete_sign_in(
                &login.token,
                &login.csrf,
                "core.user.authenticator",
                &std::env::var("BILIBI_SSO_ADMIN_OTP")?,
            )
            .await
            .map_err(|error| format!("administrator factor completion failed: {error}"))?
            .ok_or("administrator factor refused")?;
    }
    let scope = app
        .security
        .guard(&login.token)
        .await
        .map_err(|_| "admitted administrator session scope failed")?;
    // The shared fixture has no instance-role seed. Select Dev explicitly for
    // this disposable HTTP walkthrough through the normal audited Settings API.
    if origin.starts_with("http://") && std::env::var("BILIBI_SSO_CASE").is_err() {
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
    }
    let mode = std::env::var("BILIBI_SSO_CASE").unwrap_or_default();
    match mode.as_str() {
        "mapping-off" | "mapping-on" => {
            let (mut config, _) = users.identity_configuration(&scope, 1, Some(1)).await?;
            config.role_mapping = if mode == "mapping-off" {
                Default::default()
            } else {
                serde_json::from_value(json!({"reader":[3],"operator":[2]}))?
            };
            users
                .save_identity_configuration(&scope, 1, Some(1), config, None)
                .await?;
            println!("Role mapping configuration: {mode}");
            return Ok(());
        }
        "factor-start" | "factor-spend" => {
            use bilibi_step_up::Purpose;
            let factors = app
                .services
                .get::<bilibi_step_up::StepUp>(bilibi_step_up::SERVICE)?;
            let agent = scope
                .clone()
                .for_agent(1, "sso-factor-walkthrough".into(), None);
            let purpose = |target| Purpose {
                operation: "base.system.instance.role.change",
                target,
                channel: bilibi_tenancy::Channel::Api,
            };
            if mode == "factor-start" {
                let challenge = factors
                    .start(&agent, "core.user.provider", purpose("instance"))
                    .await?;
                println!("provider challenge: {:?}", challenge.availability);
            } else {
                let proof = std::env::var("BILIBI_SSO_PROOF")?;
                let qualified = format!("core.user.provider:{proof}");
                println!(
                    "wrong target refused: {}",
                    factors
                        .require(
                            &agent.clone().presenting_step_up(&qualified),
                            "base.system.instance.role.change",
                            "other",
                        )
                        .await
                        .is_err()
                );
                println!(
                    "qualified provider proof accepted: {}",
                    factors
                        .require(
                            &agent.clone().presenting_step_up(&qualified),
                            "base.system.instance.role.change",
                            "instance",
                        )
                        .await
                        .is_ok()
                );
                println!(
                    "spent proof refused: {}",
                    factors
                        .require(
                            &agent.presenting_step_up(&qualified),
                            "base.system.instance.role.change",
                            "instance",
                        )
                        .await
                        .is_err()
                );
            }
            return Ok(());
        }
        "negative-provider" => {
            let mut config = users.identity_configuration(&scope, 1, Some(1)).await?.0;
            config.label = "Protocol refusal walkthrough".into();
            config.protocol = json!({"issuer":"http://127.0.0.1:48111", "client_id":"bilibi"});
            config.enforce_sso = false;
            config.just_in_time = false;
            config.default_role = None;
            config.role_mapping.clear();
            config.groups_claim.clear();
            config.step_up_acr = None;
            config.provider_logout = false;
            let (_, public) = users
                .save_identity_configuration(&scope, 1, None, config, None)
                .await?;
            users
                .link_external_identity(
                    &scope,
                    &public,
                    1,
                    "http://127.0.0.1:48111",
                    "negative-manager",
                )
                .await?;
            println!("protocol connection: {public}");
            return Ok(());
        }
        "inspect" => {
            let (config, _) = users.identity_configuration(&scope, 1, Some(1)).await?;
            println!(
                "provider logout: {}; provider MFA acr: {:?}",
                config.provider_logout, config.mfa_acr
            );
            return Ok(());
        }
        "manual-role" => {
            let client = db.get().await?;
            let user: i64 = client
                .query_one(
                    "SELECT u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=$1 AND i.email=$2",
                    &[&1_i64, &"bilibi-a-new@example.test"],
                )
                .await?
                .get(0);
            drop(client);
            grants.assign_role(&scope, user, 2, true).await?;
            println!("manual viewer role assigned to provisioned account");
            return Ok(());
        }
        "enforce" | "relax" => {
            let (mut config, _) = users.identity_configuration(&scope, 1, Some(1)).await?;
            config.enforce_sso = mode == "enforce";
            config.break_glass_user = Some(1);
            users
                .save_identity_configuration(&scope, 1, Some(1), config, None)
                .await?;
            let client = db.get().await?;
            let user: i64 = client
                .query_one(
                    "SELECT u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=$1 AND i.email=$2",
                    &[&1_i64, &"bilibi-a-new@example.test"],
                )
                .await?
                .get(0);
            drop(client);
            let fallback = std::env::var("BILIBI_SSO_LOCAL_PASSWORD")?;
            users.set_password(&scope, user, &fallback).await?;
            println!(
                "linked password admitted: {}",
                users
                    .login("bilibi-a-new@example.test", &fallback)
                    .await
                    .is_ok()
            );
            println!(
                "emergency password admitted: {}",
                users
                    .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
                    .await
                    .is_ok()
            );
            return Ok(());
        }
        "assurance" => {
            let (mut config, _) = users.identity_configuration(&scope, 1, Some(1)).await?;
            config.mfa_acr = vec!["gold".into()];
            config.step_up_acr = Some("gold".into());
            config.provider_logout = true;
            users
                .save_identity_configuration(&scope, 1, Some(1), config, None)
                .await?;
            println!("provider assurance and logout configured");
            return Ok(());
        }
        "sign-in-policy" => {
            let settings = app
                .services
                .get::<bilibi_settings::Settings>("base/settings.store")?;
            let mut client = db.get().await?;
            let mut tx = users
                .mutations
                .begin(
                    &mut client,
                    scope.mutation_context("walkthrough.sign_in_policy"),
                )
                .await?;
            settings
                .put(
                    &mut tx,
                    &scope,
                    bilibi_settings::Target::Global,
                    "instance.role",
                    json!("staging"),
                )
                .await?;
            settings
                .put(
                    &mut tx,
                    &scope,
                    bilibi_settings::Target::Tenant,
                    "step_up.policy",
                    json!({"core.user.sign_in":"on"}),
                )
                .await?;
            tx.commit().await?;
            println!("Staging sign-in factor policy enabled");
            return Ok(());
        }
        "unreadable-connection" => {
            // Simulates a lost or changed settings key by damaging the sealed record.
            let (mut config, _) = users.identity_configuration(&scope, 1, Some(1)).await?;
            config.enforce_sso = true;
            config.break_glass_user = Some(1);
            users
                .save_identity_configuration(&scope, 1, Some(1), config.clone(), None)
                .await?;
            let client = db.get().await?;
            let member: i64 = client
                .query_one(
                    "SELECT u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=$1 AND i.email=$2",
                    &[&1_i64, &"bilibi-a-new@example.test"],
                )
                .await?
                .get(0);
            let fallback = std::env::var("BILIBI_SSO_LOCAL_PASSWORD")?;
            users.set_password(&scope, member, &fallback).await?;
            client
                .execute(
                    "UPDATE user_identity_providers SET sealed_runtime='{\"version\":1}'::jsonb WHERE tenant_id=$1 AND id=1",
                    &[&1_i64],
                )
                .await?;
            drop(client);
            let password = bilibi_walkthrough_fixture::PASSWORD;
            println!(
                "emergency password admitted: {}",
                users.login("manager@example.test", password).await.is_ok()
            );
            println!(
                "enforced member refused with the company sign-in reason: {}",
                matches!(
                    users.login("bilibi-a-new@example.test", &fallback).await,
                    Err(bilibi_user::CredentialError::CompanySignInRequired)
                )
            );
            println!(
                "unenforced member password admitted: {}",
                users
                    .login("contractor@example.test", password)
                    .await
                    .is_ok()
            );
            let manager = app
                .security
                .guard(&users.login("manager@example.test", password).await?.token)
                .await?;
            println!(
                "unreadable connection left out of the sign-in list: {}",
                users
                    .external_identity_connections(&manager)
                    .await?
                    .is_empty()
            );
            println!(
                "administrator read reports the connection unreadable: {}",
                users
                    .identity_configuration(&scope, 1, Some(1))
                    .await
                    .is_err()
            );
            config.enabled = false;
            config.enforce_sso = false;
            users
                .save_identity_configuration(&scope, 1, Some(1), config, None)
                .await?;
            println!(
                "re-saved and readable again: {}",
                users
                    .identity_configuration(&scope, 1, Some(1))
                    .await
                    .is_ok()
            );
            println!("open {origin}/companies/1/sso?connection=1 before the re-save to see the unreadable notice");
            return Ok(());
        }
        "offboard-configurer" => {
            // Destructive: retires the administrator who last saved the connection.
            let (mut config, public) = users.identity_configuration(&scope, 1, Some(1)).await?;
            let mut open = config.clone();
            open.just_in_time = true;
            open.default_role = open.default_role.or(Some(2));
            open.allowed_email_domains.clear();
            println!(
                "account creation without allowed domains refused: {}",
                users
                    .save_identity_configuration(&scope, 1, Some(1), open, None)
                    .await
                    .is_err()
            );
            config.enforce_sso = true;
            config.break_glass_user = Some(1);
            users
                .save_identity_configuration(&scope, 1, Some(1), config, None)
                .await?;
            let admin = app
                .security
                .guard(
                    &users
                        .login("admin@example.test", bilibi_walkthrough_fixture::PASSWORD)
                        .await?
                        .token,
                )
                .await?;
            users.delete_account(&admin, scope.user()).await?;
            let password = bilibi_walkthrough_fixture::PASSWORD;
            println!(
                "company member password admitted: {}",
                users
                    .login("contractor@example.test", password)
                    .await
                    .is_ok()
            );
            println!(
                "emergency password admitted: {}",
                users.login("manager@example.test", password).await.is_ok()
            );
            println!(
                "company sign-in starts: {}",
                users
                    .begin_external_sign_in(
                        &public,
                        bilibi_user::IdentityPurpose::SignIn,
                        None,
                        "/workspace"
                    )
                    .await
                    .is_ok()
            );
            println!("finish in the browser at {origin}/login?sso={public}: a linked person signs in; a new person with a verified email on an allowed domain is created; one on another domain is refused");
            return Ok(());
        }
        "isolation" => {
            let secret = std::env::var("BILIBI_SSO_CLIENT_SECRET")?;
            let first = users.identity_configuration(&scope, 1, Some(1)).await?.0;
            let mut second = first.clone();
            second.label = "Company two".into();
            second.protocol["issuer"] = json!("http://127.0.0.1:48109/realms/bilibi-b");
            second.enforce_sso = false;
            second.just_in_time = false;
            second.role_mapping.clear();
            second.default_role = None;
            let (id2, public2) = users
                .save_identity_configuration(&scope, 2, None, second, Some(secret.clone()))
                .await?;
            users
                .link_external_identity(
                    &scope,
                    &public2,
                    7,
                    "http://127.0.0.1:48109/realms/bilibi-b",
                    "bilibi-b-manager",
                )
                .await?;
            let other = app
                .security
                .guard(
                    &users
                        .login(
                            "manager@other.example.test",
                            bilibi_walkthrough_fixture::PASSWORD,
                        )
                        .await?
                        .token,
                )
                .await?;
            let persons = app
                .services
                .get::<bilibi_person::PersonService>(bilibi_person::SERVICE)?;
            let affiliations = app
                .services
                .get::<bilibi_affiliation::AffiliationService>(bilibi_affiliation::SERVICE)?;
            let person = persons
                .create(
                    &other,
                    &bilibi_person::PersonInput {
                        name: "Other workspace manager".into(),
                        kind: bilibi_person::PersonKind::Human,
                        email: Some("manager@other.example.test".into()),
                        phone: None,
                    },
                )
                .await?;
            let _affiliation = affiliations
                .create(
                    &other,
                    &bilibi_affiliation::AffiliationInput {
                        person_id: person.id,
                        organisation_id: bilibi_party_ids::OrganisationId::new(100)?,
                        title: None,
                        work_email: None,
                        work_phone: None,
                        dates: bilibi_affiliation::EffectiveDates::new(None, None)?,
                    },
                )
                .await?;
            users.link_person(&other, 5, person.id).await?;
            users.set_desk(&other, 100, person.id.get(), true).await?;
            let other = app
                .security
                .guard(
                    &users
                        .login(
                            "manager@other.example.test",
                            bilibi_walkthrough_fixture::PASSWORD,
                        )
                        .await?
                        .token,
                )
                .await?;
            let mut third = first;
            third.label = "Other workspace".into();
            third.protocol["issuer"] = json!("http://127.0.0.1:48109/realms/bilibi-c");
            third.enforce_sso = false;
            third.just_in_time = false;
            third.default_role = None;
            third.role_mapping.clear();
            let (id3, public3) = users
                .save_identity_configuration(&other, 100, None, third, Some(secret))
                .await?;
            users
                .link_external_identity(
                    &other,
                    &public3,
                    5,
                    "http://127.0.0.1:48109/realms/bilibi-c",
                    "bilibi-c-manager",
                )
                .await?;
            println!("company two connection {id2}: {origin}/login?sso={public2}");
            println!("tenant two connection {id3}: {origin}/login?sso={public3}");
            println!(
                "cross-company read refused: {}",
                users
                    .identity_configuration(&scope, 2, Some(1))
                    .await
                    .is_err()
            );
            println!(
                "cross-tenant read refused: {}",
                users
                    .identity_configuration(&scope, 100, Some(id3))
                    .await
                    .is_err()
            );
            return Ok(());
        }
        _ => {}
    }
    let Some(issuer) = std::env::var("BILIBI_SSO_ISSUER").ok() else {
        println!(
            "fixture installed; sign in with the walkthrough account and configure /settings/sso"
        );
        return Ok(());
    };
    let secret = std::env::var("BILIBI_SSO_CLIENT_SECRET")?;
    let role = {
        let client = db.get().await?;
        client
            .query_one(
                "SELECT id FROM roles WHERE tenant_id=$1 AND code='staff'",
                &[&scope.tenant()],
            )
            .await?
            .get::<_, i64>(0)
    };
    let config = IdentityConfiguration {
        label: "Local identity provider".into(),
        enabled: true,
        protocol: json!({"issuer":issuer,"client_id":"bilibi"}),
        groups_claim: "realm_access.roles".into(),
        default_role: Some(role),
        just_in_time: true,
        allowed_email_domains: std::env::var("BILIBI_SSO_JIT_DOMAINS")
            .unwrap_or_else(|_| "example.test".into())
            .split(',')
            .map(|domain| domain.trim().to_owned())
            .collect(),
        ..Default::default()
    };
    let (id, public) = users
        .save_identity_configuration(&scope, 1, None, config, Some(secret))
        .await?;
    users
        .link_external_identity(&scope, &public, 1, &issuer, "bilibi-a-manager")
        .await?;
    println!("connection {id}: {origin}/login?sso={public}");
    println!("configure: {origin}/companies/1/sso?connection={id}");
    let foreign = users.identity_configuration(&scope, 2, Some(id)).await;
    println!(
        "another company cannot read the connection: {}",
        foreign.is_err()
    );
    Ok(())
}
