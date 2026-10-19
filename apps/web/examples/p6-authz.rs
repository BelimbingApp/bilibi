//! Running-app authorization walkthrough. Use only a disposable migrated
//! database: this command changes demo grants and temporarily appoints tenant 1
//! operator. No test runner or mock services. Its grant changes are step-up
//! operations a console cannot give a code for, so it first turns the
//! fixture tenant's policy off for them (`console_grants`). Run with
//! DATABASE_URL and BILIBI_AUTHZ_DEMO=disposable cargo run -p bilibi-web
//! --example p6-authz.
use bilibi_base::{
    authz::{
        self, fields, grants::Grants, CapabilityDefinition as C, RoleDefinition,
        SystemPrincipalDefinition, SystemScope, VocabularyContribution, VOCABULARY,
    },
    module_registry::{Contribution, Provenance},
    tenancy::{Authentication, Scope},
};
use bilibi_database::{pool, Result};
use serde_json::json;
use tokio_postgres::Client;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

inventory::submit! { Contribution::new(VOCABULARY,"p6.runtime",Provenance {module:"base/authz",source:file!(),line:line!()}, &VocabularyContribution {
    domains:&["reference"],verbs:&["view"],
    capabilities:&[C::tenant("reference.records.view"), C::tenant("reference.extended.view"), C::platform("reference.platform.view")],
    roles:&[RoleDefinition {code:"manager",name:"manager",description:"Extension of the fixture manager role",grant_all:false,capabilities:&["reference.extended.view"]}],system_principals:&[SystemPrincipalDefinition{name:"reference-reader",capabilities:&["reference.records.view","reference.platform.view"]}],
}) }
async fn show(client: &Client, scope: &Scope, key: &str) -> Result<()> {
    let d = authz::decide(client, scope, key).await?;
    println!(
        "{} / {key}: {} ({})",
        scope.user_name,
        if d.allowed { "allow" } else { "deny" },
        d.reason
    );
    Ok(())
}
async fn system(client: &Client, scope: &SystemScope, key: &str) -> Result<()> {
    let d = authz::decide_system(client, scope, key).await?;
    println!(
        "system {:?} / {key}: {} ({})",
        scope.principal(),
        if d.allowed { "allow" } else { "deny" },
        d.reason
    );
    Ok(())
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_AUTHZ_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_AUTHZ_DEMO=disposable for this fixture walkthrough".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35566")?;
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    grants.reconcile().await?;
    let client = db.get().await?;
    let client: &Client = &client;
    let manager = auth.authenticate(client, 1, 1, String::new()).await?;
    let admin = auth.authenticate(client, 1, 4, String::new()).await?;
    // Base Step-up enforces everywhere; nobody is at this console to give a code.
    bilibi_walkthrough_fixture::console_grants(&app.services, &admin).await?;
    let viewer = auth.authenticate(client, 1, 2, String::new()).await?;
    println!("== Reconciliation preserved demo role IDs and assignments");
    for row in client.query("SELECT id,tenant_id,name,code,is_system FROM roles WHERE id BETWEEN 1 AND 5 ORDER BY id",&[]).await? {
        println!("role {} tenant {}: {} -> {} (system={})",row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,String>(2),row.get::<_,String>(3),row.get::<_,bool>(4));
    }
    println!("== Contributed extension of the fixture manager role");
    show(client, &manager, "reference.extended.view").await?;
    println!("== Live roles, direct allows and deny precedence");
    show(client, &manager, "companies.update").await?;
    show(client, &viewer, "companies.update").await?;
    grants
        .set_direct(&admin, viewer.user(), "companies.update", Some(true))
        .await?;
    show(client, &viewer, "companies.update").await?;
    grants
        .set_direct(&admin, admin.user(), "companies.update", Some(false))
        .await?;
    show(client, &admin, "companies.update").await?;
    grants.reconcile().await?;
    println!("after reconciliation (direct deny preserved):");
    show(client, &admin, "companies.update").await?;
    grants
        .set_direct(&admin, admin.user(), "companies.update", None)
        .await?;
    grants
        .set_direct(&admin, viewer.user(), "companies.update", None)
        .await?;
    show(client, &admin, "uninstalled.resource.view").await?;
    println!("== Custom role grant, assignment and revocation");
    let row=client.query_one("INSERT INTO roles(tenant_id,name) VALUES(1,'P6 reference') ON CONFLICT(tenant_id,name) DO UPDATE SET name=EXCLUDED.name RETURNING id",&[]).await?;
    let custom: i64 = row.get(0);
    grants
        .set_role_capability(&admin, custom, "reference.records.view", true)
        .await?;
    grants
        .assign_role(&admin, manager.user(), custom, true)
        .await?;
    show(client, &manager, "reference.records.view").await?;
    grants
        .assign_role(&admin, manager.user(), custom, false)
        .await?;
    show(client, &manager, "reference.records.view").await?;
    println!("== General masking, including admin + viewer roles");
    grants.assign_role(&admin, admin.user(), 2, true).await?;
    let mut company = json!({"name":"Example","email":"private@example.test","revenue":100});
    fields::mask_for(client, &admin, "companies", &mut company).await?;
    println!("admin holding viewer: {company}");
    println!(
        "attempt with restricted null: {:?}",
        fields::refuse_for(client, &admin, "companies", &json!({"email":null})).await
    );
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,2,'reference_records','private_note') ON CONFLICT DO NOTHING",&[]).await?;
    let mut records =
        json!([{"id":1,"private_note":"never print"},{"id":2,"private_note":"never print"}]);
    fields::mask_for(client, &admin, "reference_records", &mut records).await?;
    println!("generalized table projection: {records}");
    grants.assign_role(&admin, admin.user(), 2, false).await?;
    println!("== Explicit operator identity, independent of grant_all");
    client.execute("DELETE FROM platform_operator", &[]).await?;
    show(client, &admin, "reference.platform.view").await?;
    client
        .execute(
            "INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1)",
            &[],
        )
        .await?;
    show(client, &admin, "reference.platform.view").await?;
    show(client, &manager, "reference.platform.view").await?;
    grants
        .set_direct(&admin, admin.user(), "reference.platform.view", Some(false))
        .await?;
    show(client, &admin, "reference.platform.view").await?;
    grants
        .set_direct(&admin, admin.user(), "reference.platform.view", None)
        .await?;
    println!("== Anonymous and declared system identities");
    let anonymous = SystemScope::rehydrate(client, 1, None).await?;
    system(client, &anonymous, "companies.view").await?;
    let named = SystemScope::rehydrate(client, 1, Some("reference-reader")).await?;
    let other = SystemScope::rehydrate(client, 2, Some("reference-reader")).await?;
    system(client, &named, "reference.records.view").await?;
    grants
        .set_system(
            &admin,
            "reference-reader",
            "reference.records.view",
            Some(true),
        )
        .await?;
    system(client, &named, "reference.records.view").await?;
    system(client, &other, "reference.records.view").await?;
    system(client, &named, "companies.view").await?;
    grants
        .set_system(
            &admin,
            "reference-reader",
            "reference.records.view",
            Some(false),
        )
        .await?;
    system(client, &named, "reference.records.view").await?;
    grants
        .set_system(&admin, "reference-reader", "reference.records.view", None)
        .await?;
    system(client, &named, "reference.records.view").await?;
    println!(
        "undeclared identity: {:?}",
        SystemScope::rehydrate(client, 1, Some("uninstalled-worker")).await
    );
    println!("== Audit failure prevents grant commit");
    client.batch_execute("CREATE OR REPLACE FUNCTION p6_refuse_action() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='authz.principal_capability.set' THEN RAISE EXCEPTION 'P6 demonstration refuses action capture'; END IF; RETURN NEW; END $$; CREATE TRIGGER p6_refuse_action BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION p6_refuse_action()").await?;
    let failed = grants
        .set_direct(&admin, viewer.user(), "companies.update", Some(true))
        .await;
    // Remove the fixture before inspecting the result, even on unexpected success.
    client
        .batch_execute(
            "DROP TRIGGER p6_refuse_action ON base_audit_actions; DROP FUNCTION p6_refuse_action()",
        )
        .await?;
    println!("grant write refused: {}", failed.is_err());
    show(client, &viewer, "companies.update").await?;
    client.execute("DELETE FROM platform_operator", &[]).await?;
    for row in client.query("SELECT event,count(*) FROM base_audit_actions WHERE event LIKE 'authz.%' GROUP BY event ORDER BY event",&[]).await? {
        println!("audit {}: {}",row.get::<_,String>(0),row.get::<_,i64>(1));
    }
    client.execute("DELETE FROM role_capabilities WHERE tenant_id IN (1,2) AND capability LIKE 'reference.%'", &[]).await?;
    client
        .execute(
            "DELETE FROM field_restrictions WHERE tenant_id=1 AND table_id='reference_records'",
            &[],
        )
        .await?;
    println!("Walkthrough complete; demo overrides removed, decision/audit evidence retained.");
    Ok(())
}
