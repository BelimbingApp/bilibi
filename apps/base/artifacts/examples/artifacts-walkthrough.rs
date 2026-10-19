//! Manual running-service walkthrough. Explicit disposable DB only; prints real
//! outcomes and durable state for inspection. No assertions or test harness.
use bilibi_artifacts::{Artifacts, Operation, Owner, Reference};
use bilibi_database::{pool, MutationBoundary};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio_postgres::Client;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Tickets {
    root: String,
    fail_write: AtomicUsize,
}
impl Owner for Tickets {
    fn owner_id(&self) -> &'static str {
        "walkthrough.documents"
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let subject = reference.map(|r| r.subject.as_str()).unwrap_or("ticket-1");
            let row = client.query_opt("SELECT 1 FROM walkthrough_artifact_tickets WHERE subject=$1 AND tenant_id=$2 AND company_id=$3 AND open AND account_id=$4", &[&subject,&scope.tenant(),&company.get(),&scope.user()]).await?;
            row.ok_or(bilibi_database::Error::Forbidden)?;
            // A fixture-only filesystem outage at the second Create check,
            // after the durable reservation and before the writer opens bytes.
            if operation == Operation::Create
                && self.fail_write.load(Ordering::SeqCst) > 0
                && self.fail_write.fetch_add(1, Ordering::SeqCst) == 2
            {
                std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o500))
                    .map_err(|_| bilibi_database::Error::Internal)?;
            }
            Ok(())
        })
    }
}
// Same logical owner ID, different adapter type: cannot borrow its authority.
// Owner is 'static, so the substitute uses its own independent concrete type.
struct Substitute;
impl Owner for Substitute {
    fn owner_id(&self) -> &'static str {
        "walkthrough.documents"
    }
    fn authorize<'a>(
        &'a self,
        _: &'a dyn ProofClient,
        _: &'a Scope,
        _: OrganisationId,
        _: Operation,
        _: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn reference() -> Reference {
    Reference {
        subject: "ticket-1".into(),
        kind: "evidence".into(),
    }
}
fn show(label: &str, result: std::result::Result<impl std::fmt::Debug, impl std::fmt::Display>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
async fn inventory(client: &Client, root: &str) -> Result<()> {
    for row in client.query("SELECT public_id,ready_at IS NOT NULL,deleted_at IS NOT NULL,purged_at IS NOT NULL FROM base_artifacts ORDER BY id",&[]).await? {
        let id: String = row.get(0);
        println!("ledger {id}: ready={} deleted={} purged={} bytes_present={}",row.get::<_,bool>(1),row.get::<_,bool>(2),row.get::<_,bool>(3),std::path::Path::new(root).join(&id).exists());
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_ARTIFACTS_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    let root = std::env::var("BILIBI_ARTIFACTS_DEMO_ROOT")?;
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let store = services.get::<Artifacts>("base/artifacts.store")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?;
    let peer = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?;
    let other = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?;
    let company = OrganisationId::new(1).map_err(|_| "invalid company")?;
    let owner = Tickets {
        root: root.clone(),
        fail_write: AtomicUsize::new(0),
    };
    let documents = store.for_owner(&actor, company, &owner);
    if std::env::args().nth(1).as_deref() == Some("recover") {
        show(
            "separate-process recovery",
            documents.recover(&mut client, 100).await,
        );
        inventory(&client, &root).await?;
        return Ok(());
    }
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    client.batch_execute("CREATE TABLE IF NOT EXISTS walkthrough_artifact_tickets(subject text PRIMARY KEY,tenant_id bigint,company_id bigint,account_id bigint,open boolean); INSERT INTO walkthrough_artifact_tickets VALUES('ticket-1',1,1,1,true) ON CONFLICT(subject) DO UPDATE SET open=true; INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING;").await?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    for (key, value) in [
        ("artifacts.storage_root", json!(root)),
        ("artifacts.retention_days", json!(30)),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("artifacts.walkthrough.configure"),
            )
            .await?;
        settings
            .put(&mut tx, &actor, Target::Global, key, value)
            .await?;
        tx.commit().await?;
    }
    println!("== private publication and integrity");
    let original = documents
        .put(&mut client, reference(), b"private receipt", "text/plain")
        .await?;
    println!("published {}", original.id);
    show(
        "authorized read",
        documents
            .read(&mut client, &original.id)
            .await
            .map(|d| (d.metadata.id, d.bytes.len())),
    );
    show(
        "same-tenant unauthorized actor",
        store
            .for_owner(&peer, company, &owner)
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    show(
        "cross-tenant read",
        store
            .for_owner(&other, company, &owner)
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    show(
        "same-owner-id substitute adapter",
        store
            .for_owner(&actor, company, &Substitute)
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    client
        .execute("UPDATE walkthrough_artifact_tickets SET open=false", &[])
        .await?;
    show(
        "owner revoked after publication",
        documents
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    client
        .execute("UPDATE walkthrough_artifact_tickets SET open=true", &[])
        .await?;
    let path = std::path::Path::new(&root).join(&original.id);
    std::fs::write(&path, b"changed receipt")?; // same byte count, different digest
    show(
        "tampered content",
        documents
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    std::fs::write(&path, b"private receipt")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    show(
        "permissive file mode",
        documents
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    // Fixture clock ages only new reservations, without modifying the immutable
    // production ledger. This makes recovery immediately observable.
    client.batch_execute("CREATE FUNCTION walkthrough_artifact_age() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.created_at=clock_timestamp()-interval '2 hours'; RETURN NEW; END $$; CREATE TRIGGER walkthrough_artifact_age BEFORE INSERT ON base_artifacts FOR EACH ROW EXECUTE FUNCTION walkthrough_artifact_age();").await?;
    println!("== filesystem failure after durable reservation");
    owner.fail_write.store(1, Ordering::SeqCst);
    show(
        "writer root becomes unavailable",
        documents
            .put(
                &mut client,
                reference(),
                b"failed FS publication",
                "text/plain",
            )
            .await,
    );
    owner.fail_write.store(0, Ordering::SeqCst);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    println!("== database/audit failure after synced file write");
    client.batch_execute("CREATE FUNCTION walkthrough_artifact_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='artifacts.publish' THEN RAISE EXCEPTION 'fixture publication audit outage'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_artifact_fail BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION walkthrough_artifact_fail();").await?;
    show(
        "publication audit rollback",
        documents
            .put(
                &mut client,
                reference(),
                b"failed DB publication",
                "text/plain",
            )
            .await,
    );
    client.batch_execute("DROP TRIGGER walkthrough_artifact_fail ON base_audit_actions; DROP FUNCTION walkthrough_artifact_fail(); DROP TRIGGER walkthrough_artifact_age ON base_artifacts; DROP FUNCTION walkthrough_artifact_age();").await?;
    println!("== read audit failure returns no document");
    client.batch_execute("CREATE FUNCTION walkthrough_artifact_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='artifacts.read' THEN RAISE EXCEPTION 'fixture read audit outage'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_artifact_fail BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION walkthrough_artifact_fail();").await?;
    show(
        "read with unavailable audit",
        documents
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    client.batch_execute("DROP TRIGGER walkthrough_artifact_fail ON base_audit_actions; DROP FUNCTION walkthrough_artifact_fail();").await?;
    println!("== unlink failure after committed tombstone");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500))?;
    show(
        "delete with unavailable root",
        documents.delete(&mut client, &original.id).await,
    );
    show(
        "tombstoned read",
        documents
            .read(&mut client, &original.id)
            .await
            .map(|d| d.bytes.len()),
    );
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    println!("== database failure after successful unlink");
    let unlinked = documents
        .put(
            &mut client,
            reference(),
            b"purge rollback document",
            "text/plain",
        )
        .await?;
    client.batch_execute("CREATE FUNCTION walkthrough_artifact_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='artifacts.purge' THEN RAISE EXCEPTION 'fixture purge audit outage'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_artifact_fail BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION walkthrough_artifact_fail();").await?;
    show(
        "purge audit rollback after unlink",
        documents.delete(&mut client, &unlinked.id).await,
    );
    client.batch_execute("DROP TRIGGER walkthrough_artifact_fail ON base_audit_actions; DROP FUNCTION walkthrough_artifact_fail();").await?;
    client
        .execute("UPDATE walkthrough_artifact_tickets SET open=false", &[])
        .await?;
    show(
        "maintenance denied before candidate selection",
        documents.recover(&mut client, 100).await,
    );
    client
        .execute("UPDATE walkthrough_artifact_tickets SET open=true", &[])
        .await?;
    // A ready control document must survive recovery.
    let control = documents
        .put(
            &mut client,
            reference(),
            b"ready control document",
            "text/plain",
        )
        .await?;
    println!("control artifact {}", control.id);
    println!("== pending durable states; run again with recover");
    inventory(&client, &root).await?;
    Ok(())
}
