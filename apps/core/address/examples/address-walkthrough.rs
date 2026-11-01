//! Manual behavior inspection through real app services on a disposable DB
//! with the Geonames sample imported. No unit tests, mocks or assertions.
use bilibi_address::{
    AddressInput, AddressKind, AddressService, AttachmentInput, AttachmentUpdate, Auto,
    CalendarDate, EffectiveDates, Location, OrganisationId, PartyId, Search, Update, WriteError,
    SERVICE,
};
use bilibi_person::{PersonInput, PersonKind, PersonService};
use std::fmt::Debug;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
type Failure = Box<dyn std::error::Error>;

fn show(label: &str, value: impl Debug) {
    println!("{label}: {value:?}");
}
/// A write's outcome without dumping the whole record.
fn outcome<T>(label: &str, result: &Result<T, WriteError>, summary: impl Fn(&T) -> String) {
    match result {
        Ok(value) => println!("{label}: ok {}", summary(value)),
        Err(WriteError::Invalid(errors)) => println!(
            "{label}: refused Invalid {:?}",
            errors
                .iter()
                .map(|e| format!("{} {}", e.field, e.message))
                .collect::<Vec<_>>()
        ),
        Err(error) => println!("{label}: refused {error:?}"),
    }
}
fn organisation(id: i64) -> Result<PartyId, Failure> {
    Ok(PartyId::Organisation(OrganisationId::new(id)?))
}
fn date(value: &str) -> Result<CalendarDate, Failure> {
    Ok(value.to_owned().try_into()?)
}
fn place(country: &str, admin1: &str, postcode: &str, locality: &str) -> Location {
    Location {
        country_iso: country.into(),
        admin1_code: admin1.into(),
        postcode: postcode.into(),
        locality: locality.into(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_ADDRESS_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_ADDRESS_DEMO=disposable for a migrated disposable DB with the Geonames sample imported".into());
    }
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:43110")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let addresses = app.services.get::<AddressService>(SERVICE)?;
    let people = app.services.get::<PersonService>(bilibi_person::SERVICE)?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let login = |email| app.user.login(email, password);
    let manager = app
        .security
        .guard(&login("manager@example.test").await?.token)
        .await?;
    let staff = app
        .security
        .guard(&login("staff@example.test").await?.token)
        .await?;
    let viewer = app
        .security
        .guard(&login("viewer@example.test").await?.token)
        .await?;
    let other = app
        .security
        .guard(&login("manager@other.example.test").await?.token)
        .await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    show(
        "Geonames sample present",
        addresses.geonames().presence().await?,
    );

    println!("\n== Normalization ==");
    let messy = AddressInput {
        label: Some("  K11 Head Office  ".into()),
        line1: Some(" 1 Jalan Ampang ".into()),
        line2: Some("   ".into()),
        postcode: Some(" 50000 ".into()),
        locality: Some("Kuala Lumpur".into()),
        country_iso: Some(" my ".into()),
        admin1_code: Some("MY.14".into()),
        parse_confidence: Some("0.875".into()),
        phone: Some("+60 3 1234 5678".into()),
        ..AddressInput::default()
    };
    show(
        "normalized input",
        addresses.validate(&messy).await.map(|v| v.input().clone()),
    );
    for (label, input) in [
        (
            "unknown country",
            AddressInput {
                country_iso: Some("ZZ".into()),
                ..AddressInput::default()
            },
        ),
        (
            "division of another country",
            AddressInput {
                country_iso: Some("SG".into()),
                admin1_code: Some("MY.14".into()),
                ..AddressInput::default()
            },
        ),
        (
            "division without country",
            AddressInput {
                admin1_code: Some("MY.14".into()),
                ..AddressInput::default()
            },
        ),
        (
            "confidence above 1 and unknown status",
            AddressInput {
                parse_confidence: Some("1.5".into()),
                verification_status: Some("checked".into()),
                ..AddressInput::default()
            },
        ),
    ] {
        outcome(label, &addresses.validate(&input).await, |_| String::new());
    }
    show(
        "unknown input field refused",
        serde_json::from_value::<AddressInput>(
            serde_json::json!({"label":"x","addressable_type":"App\\Company"}),
        )
        .map(|_| ())
        .map_err(|e| e.to_string()),
    );

    println!("\n== Location cascade (LocationSuggestion) ==");
    let start = place("", "", "", "");
    let (picked, auto) = addresses
        .suggest(&place("my", "", "", ""), &start, Auto::default())
        .await?;
    show("country chosen", (&picked, auto));
    let (filled, auto) = addresses
        .suggest(&place("MY", "", "50000", ""), &picked, auto)
        .await?;
    show("postcode fills division + single locality", (&filled, auto));
    let (moved, auto2) = addresses
        .suggest(
            &place("MY", &filled.admin1_code, "10000", &filled.locality),
            &filled,
            auto,
        )
        .await?;
    show("new postcode replaces suggested values", (&moved, auto2));
    let (typed, auto3) = addresses
        .suggest(
            &place("MY", &moved.admin1_code, "10000", "Tanjung Tokong"),
            &moved,
            auto2,
        )
        .await?;
    show("typed locality is no longer a suggestion", (&typed, auto3));
    let (kept, _) = addresses
        .suggest(
            &place("MY", &typed.admin1_code, "99999", "Tanjung Tokong"),
            &typed,
            auto3,
        )
        .await?;
    show("unknown postcode keeps the typed locality", &kept);
    let (cleared, _) = addresses
        .suggest(
            &place("SG", &kept.admin1_code, &kept.postcode, &kept.locality),
            &kept,
            Auto::default(),
        )
        .await?;
    show("country change clears dependents", &cleared);
    let options = addresses.options(&place("MY", "", "4", "Sha")).await?;
    show("options: divisions", options.admin1.len());
    show("options: postcode completions", &options.postcodes);
    show("options: locality completions", &options.localities);
    show(
        "empty reference country answers nothing (type it yourself)",
        addresses
            .options(&place("JP", "", "100", ""))
            .await?
            .postcodes,
    );

    println!("\n== Address CRUD ==");
    let mut changes = addresses.subscribe();
    let created = addresses.create(&manager, &messy).await;
    outcome("manager create", &created, |a| {
        format!("#{} {:?}", a.id, a.address)
    });
    let created = created?;
    show("change published after commit", changes.try_recv());
    outcome(
        "viewer create",
        &addresses.create(&viewer, &messy).await,
        |a| a.id.to_string(),
    );
    let page = addresses
        .search(
            &manager,
            &Search {
                q: Some("jalan".into()),
                ..Search::default()
            },
        )
        .await?;
    show(
        "search 'jalan'",
        (
            page.total,
            page.entries.iter().map(|a| a.display()).collect::<Vec<_>>(),
        ),
    );
    show(
        "page size 101 refused",
        addresses
            .search(
                &manager,
                &Search {
                    page_size: 101,
                    ..Search::default()
                },
            )
            .await
            .map(|p| p.total),
    );
    let edit = |version, label: &str| Update {
        version,
        address: AddressInput {
            label: Some(label.into()),
            ..created.address.clone()
        },
    };
    let staffed = addresses
        .update(&staff, created.id, &edit(created.version, "K11 HQ"))
        .await;
    outcome("staff update", &staffed, |a| {
        format!("v{} {:?}", a.version, a.address.label)
    });
    outcome(
        "stale update",
        &addresses
            .update(&manager, created.id, &edit(created.version, "Stale"))
            .await,
        |a| a.version.to_string(),
    );
    let current = staffed?;
    show(
        "other tenant read",
        addresses.read(&other, created.id).await.map(|a| a.id),
    );

    println!("\n== Field restriction (phone) ==");
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) SELECT 1,id,'addresses','phone' FROM roles WHERE tenant_id=1 AND code='staff' ON CONFLICT DO NOTHING", &[]).await?;
    let hidden = addresses.read(&staff, created.id).await?;
    show(
        "staff sees phone",
        (&hidden.address.phone, &hidden.restricted),
    );
    let relabel = Update {
        version: current.version,
        address: AddressInput {
            label: Some("K11 Head Office".into()),
            phone: None,
            ..current.address.clone()
        },
    };
    let relabelled = addresses.update(&staff, created.id, &relabel).await?;
    show(
        "staff relabel",
        (&relabelled.address.label, relabelled.version),
    );
    show(
        "staff edit without the hidden phone keeps it",
        addresses.read(&manager, created.id).await?.address.phone,
    );
    client
        .execute(
            "DELETE FROM field_restrictions WHERE tenant_id=1 AND table_id='addresses'",
            &[],
        )
        .await?;

    println!("\n== Organisation and Person attachments ==");
    let operator_company = organisation(1)?;
    let company2 = organisation(2)?;
    // The walkthrough fixture already marks tenant 1 the operator; keep the
    // explicit marker so a reused disposable database gets it too.
    client
        .execute(
            "INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING",
            &[],
        )
        .await?;
    show(
        "locale bootstrap before any operator address",
        addresses.platform_operator_bootstrap().await?,
    );
    let hq = addresses
        .create_and_attach(
            &manager,
            operator_company,
            &AddressInput {
                label: Some("Operator HQ".into()),
                line1: Some("1 Market Street".into()),
                locality: Some("Kuala Lumpur".into()),
                country_iso: Some("MY".into()),
                verification_status: Some("verified".into()),
                ..AddressInput::default()
            },
            &AttachmentInput {
                kinds: vec![
                    AddressKind::Headquarters,
                    AddressKind::Billing,
                    AddressKind::Headquarters,
                ],
                is_primary: true,
                priority: 1,
                ..AttachmentInput::default()
            },
        )
        .await;
    outcome("create and attach to operator company", &hq, |a| {
        format!(
            "address #{} source={:?} status={:?}; attachment #{} kinds={:?} from={:?}",
            a.address.id,
            a.address.address.source,
            a.address.address.verification_status,
            a.attachment.id,
            a.attachment.attachment.kinds,
            a.attachment.attachment.dates.from()
        )
    });
    let hq = hq?;
    let facts = addresses.platform_operator_bootstrap().await?;
    show("platform-operator locale bootstrap facts", &facts);
    show(
        "inferred installation locale",
        facts.as_ref().and_then(bilibi_locale::infer),
    );
    let branch = addresses
        .attach(
            &manager,
            created.id,
            operator_company,
            &AttachmentInput {
                kinds: vec![AddressKind::Branch],
                priority: 5,
                ..AttachmentInput::default()
            },
        )
        .await?;
    outcome(
        "attach again to the same company",
        &addresses
            .attach(
                &manager,
                created.id,
                operator_company,
                &AttachmentInput::default(),
            )
            .await,
        |a| a.id.to_string(),
    );
    outcome(
        "attach to another tenant's company",
        &addresses
            .attach(
                &manager,
                created.id,
                organisation(100)?,
                &AttachmentInput::default(),
            )
            .await,
        |a| a.id.to_string(),
    );
    outcome(
        "viewer attach (needs company update)",
        &addresses
            .attach(&viewer, created.id, company2, &AttachmentInput::default())
            .await,
        |a| a.id.to_string(),
    );
    let shared = addresses
        .attach(&manager, created.id, company2, &AttachmentInput::default())
        .await?;
    let person = people
        .create(
            &manager,
            &PersonInput {
                name: "K11 Siti Contact".into(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
        )
        .await?;
    let person_link = addresses
        .attach(
            &manager,
            created.id,
            PartyId::Person(person.id),
            &AttachmentInput {
                kinds: vec![AddressKind::Other],
                dates: EffectiveDates::new(Some(date("2026-01-01")?), Some(date("2026-12-31")?))?,
                ..AttachmentInput::default()
            },
        )
        .await?;
    show("person attachment dates", &person_link.attachment.dates);
    let summary = |list: &[bilibi_address::AttachedAddress]| {
        list.iter()
            .map(|a| {
                format!(
                    "{} (primary={} priority={} kinds={:?})",
                    a.address.display(),
                    a.attachment.attachment.is_primary,
                    a.attachment.attachment.priority,
                    a.attachment.attachment.kinds
                )
            })
            .collect::<Vec<_>>()
    };
    show(
        "operator company addresses, primary first",
        summary(&addresses.attached(&manager, operator_company).await?),
    );
    show(
        "operator company primary address",
        addresses
            .primary(&manager, operator_company)
            .await?
            .map(|a| a.address.display()),
    );
    show(
        "Person addresses",
        summary(
            &addresses
                .attached(&manager, PartyId::Person(person.id))
                .await?,
        ),
    );
    show(
        "addresses available to company 2",
        addresses
            .available(&manager, company2, &None)
            .await?
            .iter()
            .map(|a| a.display())
            .collect::<Vec<_>>(),
    );
    let detail = addresses.detail(&manager, created.id).await?;
    show(
        "detail location names",
        (&detail.country_name, &detail.admin1_name),
    );
    show(
        "detail owners",
        detail
            .owners
            .iter()
            .map(|o| (o.attachment.owner, o.name.clone(), o.read_only))
            .collect::<Vec<_>>(),
    );
    outcome(
        "stale attachment edit",
        &addresses
            .update_attachment(
                &manager,
                branch.id,
                &AttachmentUpdate {
                    version: branch.version + 1,
                    attachment: AttachmentInput::default(),
                },
            )
            .await,
        |a| a.version.to_string(),
    );
    outcome(
        "make branch primary",
        &addresses
            .update_attachment(
                &manager,
                branch.id,
                &AttachmentUpdate {
                    version: branch.version,
                    attachment: AttachmentInput {
                        kinds: vec![AddressKind::Branch, AddressKind::Shipping],
                        is_primary: true,
                        priority: 0,
                        ..AttachmentInput::default()
                    },
                },
            )
            .await,
        |a| format!("v{} {:?}", a.version, a.attachment),
    );
    show(
        "primary after change",
        addresses
            .primary(&manager, operator_company)
            .await?
            .map(|a| a.address.display()),
    );

    println!("\n== Archived company ==");
    client
        .execute(
            "UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;
    let current = addresses.read(&manager, created.id).await?;
    outcome(
        "edit address an archived company uses",
        &addresses
            .update(&manager, created.id, &edit(current.version, "Frozen"))
            .await,
        |a| a.version.to_string(),
    );
    outcome(
        "detach from archived company",
        &addresses.detach(&manager, shared.id, shared.version).await,
        |_| String::new(),
    );
    show(
        "archived company addresses stay readable",
        summary(&addresses.attached(&manager, company2).await?),
    );
    show(
        "detail marks the archived owner read-only",
        addresses
            .detail(&manager, created.id)
            .await?
            .owners
            .iter()
            .map(|o| (o.name.clone(), o.read_only))
            .collect::<Vec<_>>(),
    );
    client
        .execute(
            "UPDATE companies SET status='active' WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;
    println!("\n== Delete ==");
    let current = addresses.read(&manager, created.id).await?;
    outcome(
        "delete while attached",
        &addresses
            .delete(&manager, created.id, current.version)
            .await,
        |_| String::new(),
    );
    for link in addresses.detail(&manager, created.id).await?.owners {
        let detached = addresses
            .detach(&manager, link.attachment.id, link.attachment.version)
            .await;
        outcome(&format!("detach {}", link.name), &detached, |_| {
            String::new()
        });
    }
    println!("\n== Retired Person ==");
    let leaver = people
        .create(
            &manager,
            &PersonInput {
                name: "K11 Former Contact".into(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
        )
        .await?;
    let leaver_link = addresses
        .attach(
            &manager,
            hq.address.id,
            PartyId::Person(leaver.id),
            &AttachmentInput::default(),
        )
        .await?;
    let retired = people.retire(&manager, leaver.id, leaver.version).await?;
    outcome(
        "attach to retired Person",
        &addresses
            .attach(
                &manager,
                created.id,
                PartyId::Person(retired.id),
                &AttachmentInput::default(),
            )
            .await,
        |a| a.id.to_string(),
    );
    outcome(
        "detach from retired Person",
        &addresses
            .detach(&manager, leaver_link.id, leaver_link.version)
            .await,
        |_| String::new(),
    );
    show(
        "retired Person addresses stay readable",
        summary(
            &addresses
                .attached(&manager, PartyId::Person(retired.id))
                .await?,
        ),
    );

    println!("\n== Delete once unattached ==");
    outcome(
        "staff delete (no delete capability)",
        &addresses.delete(&staff, created.id, current.version).await,
        |_| String::new(),
    );
    outcome(
        "manager delete once unattached",
        &addresses
            .delete(&manager, created.id, current.version)
            .await,
        |_| String::new(),
    );
    show(
        "deleted address read",
        addresses.read(&manager, created.id).await.map(|a| a.id),
    );
    show(
        "deleted row retained (soft delete)",
        client
            .query_one(
                "SELECT deleted_at IS NOT NULL FROM addresses WHERE id=$1",
                &[&created.id.get()],
            )
            .await?
            .get::<_, bool>(0),
    );
    show(
        "audit rows (address mutations, attachment mutations, address actions)",
        client
            .query_one(
                "SELECT (SELECT count(*) FROM base_audit_mutations WHERE auditable_type='address'), (SELECT count(*) FROM base_audit_mutations WHERE auditable_type='address_attachment'), (SELECT count(*) FROM base_audit_actions WHERE event LIKE 'address.%')",
                &[],
            )
            .await
            .map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2))),
    );
    Ok(())
}
