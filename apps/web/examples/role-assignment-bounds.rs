//! Disposable running proof for company/date-qualified Authz assignments.
//! Migrate and seed-production first, with BILIBI_ENV=development. Then run
//! BILIBI_ASSIGNMENT_PROOF=disposable cargo run -p bilibi-web --example role-assignment-bounds.
//! No product records or credentials are emitted. Finance supplies the source,
//! report, artifact and HTTP MCP proof of the same company authorization path.
use bilibi_authz::{
    self as authz,
    grants::{Assignment, Grants},
    CapabilityDefinition as C, EngagementRole, RoleDefinition, SystemPrincipalDefinition,
    VocabularyContribution, ENGAGEMENT_ROLES, VOCABULARY,
};
use bilibi_database::{pool, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::{Authentication, Scope};
use std::sync::Arc;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
const VIEW: &str = "reference.engagement.view";
const GLOBAL: &str = "reference.unscoped.view";
inventory::submit! { Contribution::new(VOCABULARY,"assignment-bounds.proof",Provenance {module:"base/authz",source:file!(),line:line!()}, &VocabularyContribution {
    domains:&["reference"],verbs:&["view"],capabilities:&[C::company(VIEW),C::tenant(GLOBAL)],
    roles:&[
        RoleDefinition {code:"engagement_reader",name:"Engagement reader",description:"Disposable proof",grant_all:false,capabilities:&[VIEW]},
        RoleDefinition {code:"unscoped_reader",name:"Unscoped reader",description:"Disposable proof",grant_all:false,capabilities:&[GLOBAL]},
        RoleDefinition {code:"context_reader",name:"Company-context reader",description:"Disposable proof",grant_all:false,capabilities:&[VIEW]},
    ],system_principals:&[SystemPrincipalDefinition {name:"reference-engagement-reader",capabilities:&[VIEW]}],
}) }
inventory::submit! { Contribution::new(ENGAGEMENT_ROLES,"assignment-bounds.engagement",Provenance {module:"base/authz",source:file!(),line:line!()}, &EngagementRole {role:"engagement_reader"}) }

// A registered example read uses the same owner company proof as a Domain.
struct CompanyRead;
impl bilibi_agent_api::ReadHandler for CompanyRead {
    fn owner(&self) -> &'static str {
        "base/authz_agent"
    }
    fn call<'a>(
        &'a self,
        client: &'a dyn bilibi_agent_api::OperationClient,
        scope: &'a Scope,
        input: &'a serde_json::Value,
    ) -> bilibi_agent_api::OperationFuture<'a> {
        Box::pin(async move {
            let company = input["company_id"].as_i64().ok_or(Error::Invalid)?;
            bilibi_company::authorize_work_target(client, scope, company, VIEW).await?;
            Ok(serde_json::json!({"company_id":company}).into())
        })
    }
}
fn company_input() -> serde_json::Value {
    serde_json::json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1}}})
}
fn company_output() -> serde_json::Value {
    serde_json::json!({"type":"object"})
}
const COMPANY_READ: &str = "base.authz_agent.company_scope_proof.get";
inventory::submit! { Contribution::new(bilibi_agent_api::DEFINITIONS,"assignment-bounds.company-read",Provenance {module:"base/authz_agent",source:file!(),line:line!()}, &bilibi_agent_api::Definitions {
    operations:&[bilibi_agent_api::Operation {key:COMPANY_READ,title:"Read a company-scoped proof record",summary:"Disposable owner company proof",keywords:&[],capability:VIEW,input:company_input,output:company_output,handler:bilibi_agent_api::Handler::Read(&CompanyRead),channels:bilibi_agent_api::Channels::Every,step_up:&[]}],guides:&[],
}) }

// A preview-only write on the same capability: Plans decides it against the
// operation's company, as the read path does. Nothing here is ever committed.
struct CompanyReview;
impl bilibi_agent_api::WriteHandler for CompanyReview {
    fn owner(&self) -> &'static str {
        "base/authz_agent"
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn bilibi_agent_api::OperationClient,
        scope: &'a Scope,
        input: &'a serde_json::Value,
    ) -> bilibi_agent_api::OperationFuture<'a> {
        bilibi_agent_api::ReadHandler::call(&CompanyRead, client, scope, input)
    }
    fn commit<'a>(
        &'a self,
        _tx: &'a mut bilibi_database::MutationTransaction<'_>,
        _scope: &'a Scope,
        _input: &'a serde_json::Value,
    ) -> bilibi_agent_api::OperationFuture<'a> {
        Box::pin(async { Err(bilibi_agent_api::Error::WritesNotEnabled) })
    }
}
const COMPANY_REVIEW: &str = "base.authz_agent.company_scope_proof.review";
inventory::submit! { Contribution::new(bilibi_agent_api::DEFINITIONS,"assignment-bounds.company-review",Provenance {module:"base/authz_agent",source:file!(),line:line!()}, &bilibi_agent_api::Definitions {
    operations:&[bilibi_agent_api::Operation {key:COMPANY_REVIEW,title:"Review a company-scoped proof change",summary:"Disposable owner company proof",keywords:&[],capability:VIEW,input:company_input,output:company_output,handler:bilibi_agent_api::Handler::SafeWrite(&CompanyReview),channels:bilibi_agent_api::Channels::Every,step_up:&[]}],guides:&[],
}) }

struct ProofIdentity(Scope);
impl bilibi_agent_api::plans::PlanAuthority for ProofIdentity {
    fn verify<'a>(
        &'a self,
        _tx: &'a tokio_postgres::Transaction<'_>,
    ) -> bilibi_tenancy::ProofFuture<'a, bilibi_agent_api::plans::PlanIdentity> {
        Box::pin(async move {
            Ok(bilibi_agent_api::plans::PlanIdentity {
                scope: self.0.clone(),
                credential_binding: "assignment-bounds-proof".into(),
            })
        })
    }
}

struct Proof {
    db: Db,
    boundary: Arc<MutationBoundary>,
    grants: Arc<Grants>,
    admin: Scope,
}
impl Proof {
    async fn assign(
        &self,
        user: i64,
        role: i64,
        assigned: bool,
        bounds: &Assignment,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                self.admin.mutation_context("authz.assignment.proof"),
            )
            .await?;
        self.grants
            .assign_role_with_in(&mut tx, &self.admin, user, role, assigned, bounds)
            .await?;
        tx.commit().await
    }
    async fn read(&self, scope: &Scope, company: i64, allowed: bool) -> Result<()> {
        let client = self.db.get().await?;
        let result = bilibi_company::authorize_work_target(&**client, scope, company, VIEW).await;
        assert_eq!(
            result.is_ok(),
            allowed,
            "company {company}, user {}",
            scope.user()
        );
        if !allowed {
            assert!(matches!(result, Err(Error::Forbidden)));
        }
        assert_eq!(
            authz::decide_in_company(&**client, scope, VIEW, Some(company))
                .await?
                .allowed,
            allowed
        );
        assert_eq!(
            authz::effective_capabilities_in_company(&**client, scope, scope.user(), Some(company))
                .await?
                .allowed
                .contains(VIEW),
            allowed
        );
        Ok(())
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_ASSIGNMENT_PROOF").as_deref() != Ok("disposable") {
        return Err(
            "Set BILIBI_ASSIGNMENT_PROOF=disposable on a fresh migrated/seeded database.".into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), &std::env::var("APP_ORIGIN")?)?;
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    grants
        .reconcile()
        .await
        .map_err(|e| format!("reconciliation: {e}"))?;
    let users = app
        .services
        .get::<bilibi_user::UserService>("core/user.accounts")?;
    println!("Fixture installed and role vocabulary reconciled.");
    let client = db.get().await?;
    let admin = auth.authenticate(&**client, 1, 4, String::new()).await?;
    let first = auth.authenticate(&**client, 1, 1, String::new()).await?;
    let second = auth.authenticate(&**client, 1, 7, String::new()).await?;
    for person in [
        first
            .actor()
            .person()
            .ok_or("missing fixture Person")?
            .get(),
        second
            .actor()
            .person()
            .ok_or("missing fixture Person")?
            .get(),
    ] {
        for company in [1, 2] {
            // The fixture already owns one desk each. A mutation with no
            // affected row is refused by MutationBoundary, so add only missing desks.
            let worker = if first.actor().person().is_some_and(|id| id.get() == person) {
                &first
            } else {
                &second
            };
            if !bilibi_company::reachable(&**client, worker, company.try_into()?).await? {
                users
                    .set_desk(&admin, company, person, true)
                    .await
                    .map_err(|e| format!("desk company {company}, Person {person}: {e}"))?;
            }
        }
    }
    let first = auth.authenticate(&**client, 1, 1, String::new()).await?;
    let second = auth.authenticate(&**client, 1, 7, String::new()).await?;
    let reader: i64 = client
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=1 AND code='engagement_reader'",
            &[],
        )
        .await?
        .get(0);
    let global: i64 = client
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=1 AND code='unscoped_reader'",
            &[],
        )
        .await?
        .get(0);
    let core_admin: i64 = client
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=1 AND code='core_admin'",
            &[],
        )
        .await?
        .get(0);
    drop(client);
    let proof = Proof {
        db: db.clone(),
        boundary: app.services.get("base/database.mutations")?,
        grants: grants.clone(),
        admin,
    };
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string();
    let mut bounds = Assignment {
        company_id: Some(1),
        valid_from: Some(today.clone()),
        valid_until: Some(today),
        purpose: "Verified appointment; records inspection".into(),
    };
    assert!(proof
        .assign(first.user(), reader, true, &Assignment::default())
        .await
        .is_err());
    assert!(proof
        .assign(first.user(), core_admin, true, &bounds)
        .await
        .is_err());
    assert!(proof
        .assign(first.user(), global, true, &bounds)
        .await
        .is_err());
    let mut other = bounds.clone();
    other.company_id = Some(100);
    assert!(proof
        .assign(first.user(), reader, true, &other)
        .await
        .is_err());
    println!(
        "Refused missing company/purpose, scoped grant-all, tenant-only keys and another tenant."
    );
    let indefinite = Assignment {
        company_id: Some(1),
        purpose: "Indefinite appointment; explicit revocation".into(),
        ..Assignment::default()
    };
    proof
        .assign(first.user(), reader, true, &indefinite)
        .await?;
    proof.read(&first, 1, true).await?;
    println!("Both dates are optional; a company engagement may start now with no end date.");
    proof.assign(first.user(), reader, true, &bounds).await?;
    proof.read(&first, 1, true).await?;
    proof.read(&first, 2, false).await?;
    proof.read(&second, 1, false).await?;
    other = bounds.clone();
    other.company_id = Some(2);
    proof.assign(second.user(), reader, true, &other).await?;
    proof.read(&second, 2, true).await?;
    proof.read(&second, 1, false).await?;
    let client = db.get().await?;
    let other_current = auth
        .reprove_work_company(&**client, &second, 1.try_into()?)
        .await?;
    drop(client);
    let registry = app
        .services
        .get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let answer = registry
        .read(
            &db,
            &other_current,
            COMPANY_READ,
            &serde_json::json!({"company_id":2}),
        )
        .await?;
    assert_eq!(answer["result"]["company_id"], 2);
    assert_eq!(other_current.current_company().map(|id| id.get()), Some(1));
    assert!(registry
        .read(
            &db,
            &second,
            COMPANY_READ,
            &serde_json::json!({"company_id":1})
        )
        .await
        .is_err());
    println!("Operation reads use their explicit company without changing the selected desk.");
    let plans = app
        .services
        .get::<bilibi_agent_api::plans::Plans>("base/agent_api.plans")?;
    for (company, valid) in [(2, true), (1, false)] {
        let review = plans
            .preview(
                &ProofIdentity(other_current.clone()),
                &[bilibi_agent_api::plans::PlannedOperation {
                    key: COMPANY_REVIEW.into(),
                    input: serde_json::json!({"company_id":company}),
                }],
            )
            .await?;
        assert_eq!(review["valid"], valid, "plan preview, company {company}");
        if !valid {
            assert_eq!(
                review["results"][0]["error"]["code"], "missing_capability",
                "plan preview, company {company}"
            );
        }
    }
    println!("Plan previews decide the grant against the operation's company: company 2 passes, company 1 refuses.");
    println!("Two people with desks in both companies retain different company grants.");
    proof.assign(first.user(), reader, true, &other).await?;
    proof.assign(first.user(), reader, false, &bounds).await?;
    proof.read(&first, 1, false).await?;
    proof.read(&first, 2, true).await?;
    proof.assign(first.user(), reader, true, &bounds).await?;
    bounds.valid_from = Some("9999-01-01".into());
    bounds.valid_until = Some("9999-12-31".into());
    proof.assign(first.user(), reader, true, &bounds).await?;
    proof.read(&first, 1, false).await?;
    bounds.valid_from = Some("2000-01-01".into());
    bounds.valid_until = Some("2000-01-01".into());
    proof.assign(first.user(), reader, true, &bounds).await?;
    proof.read(&first, 1, false).await?;
    println!(
        "Exact-company revocation, future start and inclusive end dates recheck retained scopes."
    );
    grants
        .set_direct(&proof.admin, second.user(), VIEW, Some(false))
        .await?;
    proof.read(&second, 2, false).await?;
    grants
        .set_direct(&proof.admin, second.user(), VIEW, None)
        .await?;
    proof.read(&second, 2, true).await?;
    proof
        .assign(first.user(), global, true, &Assignment::default())
        .await?;
    let client = db.get().await?;
    assert!(
        authz::decide_in_company(&**client, &first, GLOBAL, None)
            .await?
            .allowed
    );
    assert!(
        authz::decide_in_company(&**client, &first, GLOBAL, Some(2))
            .await?
            .allowed
    );
    assert!(
        !authz::effective_capabilities_in_company(&**client, &second, second.user(), None)
            .await?
            .allowed
            .contains(VIEW)
    );
    let rows = authz::roles::assignments(&**client, &proof.admin, first.user()).await?;
    assert!(rows
        .iter()
        .any(|r| r.role.id == reader && r.bounds.company_id == Some(1) && !r.effective));
    assert!(rows
        .iter()
        .any(|r| r.role.id == reader && r.bounds.company_id == Some(2) && r.effective));
    let actions: i64 = client
        .query_one(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='authz.principal_role'",
            &[],
        )
        .await?
        .get(0);
    assert!(actions > 0);
    println!("Direct deny wins; unscoped grants keep tenant reach; expired assignments remain inspectable; {actions} audited assignment changes.");
    let context_role: i64 = client
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=1 AND code='context_reader'",
            &[],
        )
        .await?
        .get(0);
    drop(client);
    grants
        .set_direct(
            &proof.admin,
            first.user(),
            "admin.authz.role.update",
            Some(true),
        )
        .await?;
    grants
        .set_direct(
            &proof.admin,
            first.user(),
            "admin.authz.system-principal.grant",
            Some(true),
        )
        .await?;
    let client = db.get().await?;
    let delegated = auth
        .reprove_work_company(&**client, &first, 2.try_into()?)
        .await?;
    drop(client);
    assert!(matches!(
        grants
            .set_direct(&delegated, second.user(), VIEW, Some(true))
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        grants
            .assign_role(&delegated, second.user(), context_role, true)
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        grants
            .set_system(&delegated, "reference-engagement-reader", VIEW, Some(true))
            .await,
        Err(Error::Forbidden)
    ));
    let mut client = db.get().await?;
    let mut tx = proof
        .boundary
        .begin(
            &mut client,
            delegated.mutation_context("authz.identity_policy.proof"),
        )
        .await?;
    assert!(matches!(
        grants
            .configure_identity_roles_in(
                &mut tx,
                &delegated,
                2.try_into()?,
                "reference-company-identity",
                &[context_role]
            )
            .await,
        Err(Error::Forbidden)
    ));
    drop(tx);
    drop(client);
    proof.read(&second, 1, false).await?;
    println!("Company-limited authority cannot become an unscoped role, direct grant, provider mapping or named-principal grant.");

    let mut client = db.get().await?;
    let mut tx = proof
        .boundary
        .begin(
            &mut client,
            proof
                .admin
                .mutation_context("authz.provider_assignment.proof"),
        )
        .await?;
    grants
        .configure_identity_roles_in(
            &mut tx,
            &proof.admin,
            1.try_into()?,
            "reference-assignment-status",
            &[global],
        )
        .await?;
    grants
        .apply_identity_roles_in(&mut tx, &first, "reference-assignment-status", &[global])
        .await?;
    tx.commit().await?;
    drop(client);
    for (from, until, effective) in [
        (None, Some("2000-01-01"), false),
        (Some("9999-01-01"), None, false),
        (None, None, true),
    ] {
        proof
            .assign(
                first.user(),
                global,
                true,
                &Assignment {
                    valid_from: from.map(str::to_owned),
                    valid_until: until.map(str::to_owned),
                    ..Assignment::default()
                },
            )
            .await?;
        let client = db.get().await?;
        let manual = authz::roles::assignments(&**client, &proof.admin, first.user()).await?;
        assert_eq!(
            manual
                .iter()
                .find(|a| a.role.id == global)
                .unwrap()
                .effective,
            effective,
            "manual assignment status must not inherit provider membership"
        );
        assert!(
            authz::roles::provider_assignments(&**client, &proof.admin, first.user())
                .await?
                .iter()
                .any(|role| role.id == global)
        );
        assert!(authz::decide(&**client, &first, GLOBAL).await?.allowed);
    }
    println!("Expired and future manual assignments stay inactive beside an effective provider grant; current manual assignments remain current.");

    Ok(())
}
