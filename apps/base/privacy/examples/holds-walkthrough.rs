//! Manual running-service walkthrough of legal holds (ADR 05). Explicit
//! disposable DB only; prints real outcomes and durable state for inspection.
//! No assertions or test harness. Run once, then again with `report` from a
//! fresh process. It also races two placements of one hold and places a hold
//! while an erasure's hold guard is open.
use bilibi_database::pool;
use bilibi_database::Db;
use bilibi_module_registry::Services;
use bilibi_party_ids::PersonId;
use bilibi_personal_data::Holds;
use bilibi_privacy::{HoldTarget, Privacy, SERVICE};
use bilibi_tenancy::Authentication;
use bilibi_tenancy::Scope;
use std::sync::Arc;
use tokio_postgres::Client;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn show<T: std::fmt::Debug, E: std::fmt::Display>(label: &str, result: std::result::Result<T, E>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
/// Places a hold on its own connection, so two can run at once.
async fn place(
    privacy: &Privacy,
    db: &Db,
    scope: &Scope,
    target: HoldTarget,
    reason: &str,
) -> String {
    let mut client = match db.get().await {
        Ok(client) => client,
        Err(error) => return format!("no connection — {error}"),
    };
    match privacy.place_hold(&mut client, scope, target, reason).await {
        Ok(hold) => format!("placed hold {}", hold.id),
        Err(error) => format!("refused — {error}"),
    }
}
/// Raw SQL from "another client": the database guards must hold without the facade.
async fn raw(client: &Client, label: &str, sql: &str) {
    match client.batch_execute(sql).await {
        Ok(()) => println!("{label}: ok (changed)"),
        Err(error) => println!(
            "{label}: refused by database — {}",
            error.as_db_error().map_or("unavailable", |d| d.message())
        ),
    }
}
async fn report(client: &Client) -> Result<()> {
    for row in client
        .query(
            "SELECT id,tenant_id,person_id,record_kind,record_id,reason,placed_by,released_at IS NOT NULL,released_by,release_reason \
             FROM base_privacy_holds ORDER BY id",
            &[],
        )
        .await?
    {
        println!(
            "hold {} tenant={} person={:?} record={:?}:{:?} reason={:?} placed_by={} released={} released_by={:?} release_reason={:?}",
            row.get::<_, i64>(0), row.get::<_, i64>(1), row.get::<_, Option<i64>>(2), row.get::<_, Option<String>>(3),
            row.get::<_, Option<i64>>(4), row.get::<_, String>(5), row.get::<_, i64>(6), row.get::<_, bool>(7),
            row.get::<_, Option<i64>>(8), row.get::<_, Option<String>>(9)
        );
    }
    for row in client
        .query(
            "SELECT hold_id,event,reason,actor_id,recorded_at IS NOT NULL FROM base_privacy_hold_events ORDER BY id",
            &[],
        )
        .await?
    {
        println!(
            "event hold={} {:<8} reason={:?} actor={} stamped={}",
            row.get::<_, i64>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, i64>(3),
            row.get::<_, bool>(4)
        );
    }
    for row in client
        .query(
            "SELECT auditable_id,event,actor_type||':'||actor_id FROM base_audit_mutations WHERE auditable_type='base/privacy.hold' ORDER BY id",
            &[],
        )
        .await?
    {
        println!(
            "audit mutation hold={} {} actor={}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_PRIVACY_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide("bootstrap.composition", Arc::new(bilibi_web::composition()))?;
    services.install(&modules)?;
    let privacy = services.get::<Privacy>(SERVICE)?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    if std::env::args().nth(1).as_deref() == Some("report") {
        println!("== separate-process report: holds, events and audit");
        return report(&client).await;
    }
    // The tenant 2 manager may read and manage holds, so isolation shows as an
    // empty list and a missing hold rather than a refusal.
    client
        .batch_execute(
            "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(2,5,'privacy.hold.view'),(2,5,'privacy.hold.manage') ON CONFLICT DO NOTHING",
        )
        .await?;
    let admin = authentication
        .authenticate(&**client, 1, 4, "walkthrough".into())
        .await?;
    let viewer = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?;
    let other = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?;
    let person = PersonId::new(1).map_err(|_| "invalid person")?;

    println!("== mounted stores");
    println!("stores: {:?}", privacy.stores().collect::<Vec<_>>());

    println!("== place: person hold, record hold, duplicate, bad reason, no capability");
    let on_person = privacy
        .place_hold(
            &mut client,
            &admin,
            HoldTarget::Person { person },
            "Litigation case 12",
        )
        .await?;
    println!("placed {on_person:?}");
    let on_record = privacy
        .place_hold(
            &mut client,
            &admin,
            HoldTarget::Record {
                kind: "person".into(),
                id: 2,
            },
            "Regulator inquiry",
        )
        .await?;
    println!("placed {on_record:?}");
    show(
        "second hold on the same person",
        privacy
            .place_hold(&mut client, &admin, HoldTarget::Person { person }, "again")
            .await,
    );
    show(
        "empty reason",
        privacy
            .place_hold(
                &mut client,
                &admin,
                HoldTarget::Record {
                    kind: "person".into(),
                    id: 3,
                },
                "   ",
            )
            .await,
    );
    show(
        "viewer places a hold",
        privacy
            .place_hold(
                &mut client,
                &viewer,
                HoldTarget::Record {
                    kind: "person".into(),
                    id: 3,
                },
                "no capability",
            )
            .await,
    );

    println!("== what an erasure sees: only the records it asks about");
    let holds = Holds::acquire(1, Some(person)).await?;
    println!(
        "person 1 erasure: person hold={:?} person:2 held={:?} person:3 held={:?}",
        holds.person_hold().await?,
        holds.record("person", 2).await?,
        holds.record("person", 3).await?
    );
    holds.release().await?;
    let holds = Holds::acquire(1, PersonId::new(9).ok()).await?;
    println!(
        "person 9 erasure: person hold={:?} person:2 held={:?} person:3 held={:?}",
        holds.person_hold().await?,
        holds.record("person", 2).await?,
        holds.record("person", 3).await?
    );
    holds.release().await?;

    println!("== two placements of the same hold at once");
    let target = HoldTarget::Record {
        kind: "person".into(),
        id: 4,
    };
    let (first, second) = tokio::join!(
        place(&privacy, &db, &admin, target.clone(), "Race A"),
        place(&privacy, &db, &admin, target.clone(), "Race B")
    );
    println!("first: {first}; second: {second}");

    println!("== a hold placed while an erasure runs waits for it");
    let guard = Holds::acquire(1, PersonId::new(3).ok()).await?;
    println!(
        "erasure guard open; person 3 held now: {:?}",
        guard.person_hold().await?
    );
    let started = std::time::Instant::now();
    let placing = {
        let (privacy, db, admin) = (privacy.clone(), db.clone(), admin.clone());
        tokio::spawn(async move {
            let outcome = place(
                &privacy,
                &db,
                &admin,
                HoldTarget::Person {
                    person: PersonId::new(3).expect("person"),
                },
                "Placed during erasure",
            )
            .await;
            (outcome, std::time::Instant::now())
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    println!(
        "after 1.5 s the placement finished: {}",
        placing.is_finished()
    );
    guard.release().await?;
    let released = std::time::Instant::now();
    let (outcome, done) = placing.await?;
    println!(
        "erasure committed and released its guard; placement: {outcome}, finished {} ms after the release (waited {} ms)",
        done.saturating_duration_since(released).as_millis(),
        done.saturating_duration_since(started).as_millis()
    );
    let next = Holds::acquire(1, PersonId::new(3).ok()).await?;
    println!(
        "the next erasure of person 3 sees the hold: {:?}",
        next.person_hold().await?
    );
    next.release().await?;

    println!("== release, release twice, list, history");
    show(
        "release person hold",
        privacy
            .release_hold(&mut client, &admin, on_person.id, "Case closed")
            .await
            .map(|h| (h.id, h.released_at.is_some())),
    );
    show(
        "release it again",
        privacy
            .release_hold(&mut client, &admin, on_person.id, "again")
            .await
            .map(|h| h.id),
    );
    let holds = Holds::acquire(1, Some(person)).await?;
    println!(
        "person 1 after release: person hold={:?}",
        holds.person_hold().await?
    );
    holds.release().await?;
    let page = privacy.holds(&**client, &admin, "", 0, 2).await?;
    println!("list tenant 1, first page of 2: total {}", page.total);
    for hold in page.holds {
        println!(
            "  hold {} {:?} released={}",
            hold.id,
            hold.target,
            hold.released_at.is_some()
        );
    }
    let page = privacy.holds(&**client, &admin, "person", 2, 2).await?;
    println!(
        "list tenant 1 searching person, second page: total {}, ids {:?}",
        page.total,
        page.holds.iter().map(|h| h.id).collect::<Vec<_>>()
    );
    println!(
        "history {}: {:?}",
        on_person.id,
        privacy
            .hold_history(&**client, &admin, on_person.id)
            .await?
            .iter()
            .map(|e| (e.event.as_str(), e.reason.as_str(), e.actor))
            .collect::<Vec<_>>()
    );

    println!("== tenant isolation");
    show(
        "tenant 2 lists holds",
        privacy
            .holds(&**client, &other, "", 0, 50)
            .await
            .map(|h| h.total),
    );
    show(
        "tenant 2 releases a tenant 1 hold",
        privacy
            .release_hold(&mut client, &other, on_record.id, "not mine")
            .await
            .map(|h| h.id),
    );
    show(
        "viewer lists holds",
        privacy
            .holds(&**client, &viewer, "", 0, 50)
            .await
            .map(|h| h.total),
    );

    println!("== database guards without the facade");
    let id = on_record.id;
    raw(
        &client,
        "change a hold's reason",
        &format!("UPDATE base_privacy_holds SET reason='edited' WHERE id={id}"),
    )
    .await;
    raw(
        &client,
        "delete a hold",
        &format!("DELETE FROM base_privacy_holds WHERE id={id}"),
    )
    .await;
    raw(
        &client,
        "un-release a hold",
        &format!(
            "UPDATE base_privacy_holds SET released_at=NULL,released_by=NULL,release_reason=NULL WHERE id={}",
            on_person.id
        ),
    )
    .await;
    raw(
        &client,
        "edit a hold event",
        "UPDATE base_privacy_hold_events SET reason='edited'",
    )
    .await;
    raw(
        &client,
        "delete hold events",
        "DELETE FROM base_privacy_hold_events",
    )
    .await;
    raw(
        &client,
        "truncate hold events",
        "TRUNCATE base_privacy_hold_events",
    )
    .await;
    raw(
        &client,
        "hold with two targets",
        "INSERT INTO base_privacy_holds(tenant_id,person_id,record_kind,record_id,reason,placed_by,placed_at) VALUES(1,5,'x',1,'r',4,now())",
    )
    .await;
    Ok(())
}
