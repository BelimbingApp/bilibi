//! Manual caller-connection proof using real app services and the walkthrough
//! fixture in a disposable database.
//! No mocks or unit-test runner; failures stop the walkthrough with an error.
use bilibi_address::{billing_address, AddressInput, AddressKind, AddressService, AttachmentInput};
use bilibi_company::{
    assign_relationship, create, create_matches, current_relationship, detail, duplicates,
    preview_create, relationship_role_for_code, CompanyService, CreateOutcome, DuplicateQuery,
    NewCompany, TypeOutcome,
};
use bilibi_database::{pool, Error, MutationBoundary};
use bilibi_party::{CalendarDate, EffectiveDates, PartyId, RelationshipInput};
use bilibi_tenancy::{PartyProofs, Scope};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Barrier, task::JoinSet};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
type Failure = Box<dyn std::error::Error>;

fn prove(label: &str, passed: bool) -> Result<(), Failure> {
    if !passed {
        return Err(format!("FAIL: {label}").into());
    }
    println!("PASS: {label}");
    Ok(())
}
fn day(value: &str) -> Result<CalendarDate, Failure> {
    Ok(value.to_owned().try_into()?)
}
fn period(from: Option<&str>, to: Option<&str>) -> Result<EffectiveDates, Failure> {
    Ok(EffectiveDates::new(
        from.map(day).transpose()?,
        to.map(day).transpose()?,
    )?)
}
async fn login(app: &bilibi_web::App, email: &str) -> Result<Scope, Failure> {
    let login = app
        .user
        .login(email, bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    Ok(app.security.guard(&login.token).await?)
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_CALLER_READS").as_deref() != Ok("disposable") {
        return Err(
            "set BILIBI_CALLER_READS=disposable on a freshly migrated disposable DB".into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://127.0.0.1:43231")?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let manager = login(&app, "manager@example.test").await?;
    let viewer = login(&app, "viewer@example.test").await?;
    let admin = login(&app, "admin@example.test").await?;
    let other = login(&app, "manager@other.example.test").await?;
    let source = manager.current_company().ok_or("no fixture company")?;
    let companies = app
        .services
        .get::<CompanyService>(bilibi_company::SERVICE)?;
    let addresses = app
        .services
        .get::<AddressService>(bilibi_address::SERVICE)?;
    let mutations = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let proofs = app.services.get::<PartyProofs>("base/tenancy.parties")?;
    let mut client = db.get().await?;
    bilibi_company::ensure_default_relationship_roles(&**client, manager.tenant()).await?;
    let role = relationship_role_for_code(&**client, &manager, "customer")
        .await?
        .ok_or("customer role missing")?;
    prove(
        "unknown role code returns None",
        relationship_role_for_code(&**client, &manager, "walkthrough_missing")
            .await?
            .is_none(),
    )?;
    prove(
        "company detail keeps role masks on a ReadClient",
        detail(&**client, &viewer, source.get()).await?.0.email == bilibi_company::MASK,
    )?;
    prove(
        "company detail refuses another tenant",
        matches!(
            detail(&**client, &other, source.get()).await,
            Err(Error::NotFound)
        ),
    )?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let new = NewCompany {
        name: format!("Caller reads {stamp}"),
        code: Some(format!("caller-{stamp}")),
        tax_id: Some("TAX-CALLER".into()),
        registration_number: Some("REG-CALLER".into()),
        email: Some("caller@example.test".into()),
        ..NewCompany::default()
    };
    let preview = preview_create(&**client, &manager, &new)
        .await?
        .map_err(|errors| format!("preview refused: {errors:?}"))?;
    prove(
        "preview defaults and trims the same company fields as create",
        preview.company.name == new.name
            && preview.company.code == new.code.clone().unwrap()
            && preview.company.status == "active",
    )?;
    for (label, input, field, message) in [
        (
            "blank name",
            NewCompany::default(),
            "name",
            "can't be blank",
        ),
        (
            "unknown parent",
            NewCompany {
                parent_id: Some(i64::MAX),
                ..new.clone()
            },
            "parent_id",
            "does not exist",
        ),
        (
            "cross-tenant parent",
            NewCompany {
                // The walkthrough fixture owns company 100 in tenant 2.
                parent_id: Some(100),
                ..new.clone()
            },
            "parent_id",
            "does not exist",
        ),
        (
            "unknown legal type",
            NewCompany {
                legal_entity_type_id: Some(i64::MAX),
                ..new.clone()
            },
            "legal_entity_type_id",
            "does not exist",
        ),
        (
            "unknown jurisdiction",
            NewCompany {
                jurisdiction: Some("ZZ".into()),
                ..new.clone()
            },
            "jurisdiction",
            "must be a valid country ISO code",
        ),
    ] {
        let errors = preview_create(&**client, &manager, &input)
            .await?
            .unwrap_err();
        prove(
            label,
            errors
                .iter()
                .any(|e| e.field == field && e.message == message),
        )?;
    }
    let generated = preview_create(
        &**client,
        &manager,
        &NewCompany {
            name: "  Preview Normalization  ".into(),
            ..NewCompany::default()
        },
    )
    .await?
    .map_err(|errors| format!("generated preview: {errors:?}"))?;
    prove(
        "preview generates the code without reserving it",
        generated.company.name == "Preview Normalization" && generated.company.code == "preview",
    )?;
    let mut tx = mutations
        .begin(&mut client, manager.mutation_context("company.create"))
        .await?;
    let CreateOutcome::Created(created) = create(&mut tx, &manager, &new).await? else {
        return Err("company create refused".into());
    };
    let matching = NewCompany {
        code: Some(format!("match-{stamp}")),
        ..new.clone()
    };
    let preview = preview_create(tx.sql(), &manager, &matching)
        .await?
        .map_err(|errors| format!("matching preview: {errors:?}"))?;
    prove(
        "preview matches uncommitted company on caller transaction",
        preview.matches.candidates.as_ref().is_some_and(|rows| {
            rows.iter().any(|c| {
                c.id == created.id
                    && c.score == 1.0
                    && c.matched_facts.contains(&"registration_number".into())
            })
        }),
    )?;
    let taken = preview_create(tx.sql(), &manager, &new).await?.unwrap_err();
    prove(
        "preview sees uncommitted code collision",
        taken
            .iter()
            .any(|e| e.field == "code" && e.message == "has already been taken"),
    )?;
    let observer = db.get().await?;
    let outside = preview_create(&**observer, &manager, &matching)
        .await?
        .map_err(|errors| format!("outside preview: {errors:?}"))?;
    prove(
        "other connection cannot match uncommitted company",
        outside
            .matches
            .candidates
            .as_ref()
            .is_some_and(|rows| rows.iter().all(|c| c.id != created.id)),
    )?;
    drop(observer);
    let settings = app
        .services
        .get::<bilibi_settings::Settings>("base/settings.store")?;
    settings
        .put(
            &mut tx,
            &manager,
            bilibi_settings::Target::Tenant,
            "company.duplicates.on_create",
            serde_json::json!("require_reason"),
        )
        .await?;
    prove(
        "preview stays advisory under an uncommitted require-reason policy",
        preview_create(tx.sql(), &manager, &matching).await?.is_ok(),
    )?;
    prove(
        "create rechecks uncommitted duplicate policy on the same transaction",
        matches!(create(&mut tx, &manager, &matching).await?, CreateOutcome::Refused(errors)
            if errors.iter().any(|e| e.field == "create_anyway")),
    )?;
    let reviewed = NewCompany {
        create_anyway: Some(bilibi_company::CreateAnyway {
            candidate_ids: preview
                .matches
                .candidates
                .as_ref()
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect(),
            reason: "Reviewed independent company".into(),
        }),
        ..matching.clone()
    };
    prove(
        "reviewed candidate ids and reason allow the composed create",
        matches!(
            create(&mut tx, &manager, &reviewed).await?,
            CreateOutcome::Created(_)
        ),
    )?;
    let target = PartyId::Organisation(created.organisation_id()?);
    prove(
        "detail sees company facts before commit",
        detail(tx.sql(), &manager, created.id).await?.0.tax_id == "TAX-CALLER",
    )?;
    let input = RelationshipInput {
        source_organisation_id: source,
        target,
        role_id: role.id,
        dates: period(None, None)?,
    };
    let TypeOutcome::Saved(link) = assign_relationship(&mut tx, &manager, &proofs, &input).await?
    else {
        return Err("relationship assign refused".into());
    };
    prove(
        "relationship read sees uncommitted assignment",
        current_relationship(tx.sql(), &manager, source, target, role.id)
            .await?
            .is_some_and(|r| r.id == link.id),
    )?;
    let observer = db.get().await?;
    prove(
        "another connection cannot see uncommitted company",
        matches!(
            detail(&**observer, &manager, created.id).await,
            Err(Error::NotFound)
        ),
    )?;
    tx.rollback().await?;
    prove(
        "rollback removes composed company, link and audit mutations",
        observer.query_one(
            "SELECT NOT EXISTS(SELECT 1 FROM companies WHERE tenant_id=$1 AND id=$2) AND NOT EXISTS(SELECT 1 FROM company_relationships WHERE tenant_id=$1 AND id=$3) AND NOT EXISTS(SELECT 1 FROM base_audit_mutations WHERE tenant_id=$1 AND ((auditable_type='company' AND auditable_id=$2::text) OR (auditable_type='company.relationship' AND auditable_id=$3::text)))",
            &[&manager.tenant(), &created.id, &link.id.get()],
        ).await?.get(0),
    )?;
    drop(observer);
    drop(client);

    let CreateOutcome::Created(created) = companies.create_company(&manager, &new).await? else {
        return Err("persistent company create refused".into());
    };
    let company = created.organisation_id()?;
    let owner = PartyId::Organisation(company);
    let input = RelationshipInput {
        target: owner,
        ..input
    };
    let TypeOutcome::Saved(link) = companies.assign_relationship(&manager, &input).await? else {
        return Err("persistent relationship refused".into());
    };
    let client = db.get().await?;
    let vendor = relationship_role_for_code(&**client, &manager, "vendor")
        .await?
        .ok_or("vendor role missing")?;
    let today: String = client
        .query_one("SELECT CURRENT_DATE::text", &[])
        .await?
        .get(0);
    drop(client);
    let TypeOutcome::Saved(dated) = companies
        .assign_relationship(
            &manager,
            &RelationshipInput {
                role_id: vendor.id,
                dates: period(Some("9999-01-01"), None)?,
                ..input.clone()
            },
        )
        .await?
    else {
        return Err("future relationship refused".into());
    };
    let client = db.get().await?;
    prove(
        "future relationship is not current",
        current_relationship(&**client, &manager, source, owner, vendor.id)
            .await?
            .is_none(),
    )?;
    drop(client);
    let dated = companies
        .update_relationship(
            &manager,
            dated.id,
            &bilibi_company::RelationshipUpdate {
                version: dated.version,
                dates: period(None, Some("1900-01-01"))?,
            },
        )
        .await?;
    let client = db.get().await?;
    prove(
        "ended relationship is not current",
        current_relationship(&**client, &manager, source, owner, vendor.id)
            .await?
            .is_none(),
    )?;
    drop(client);
    companies
        .update_relationship(
            &manager,
            dated.id,
            &bilibi_company::RelationshipUpdate {
                version: dated.version,
                dates: period(Some(&today), Some(&today))?,
            },
        )
        .await?;
    let client = db.get().await?;
    prove(
        "relationship includes both endpoints on a one-day interval",
        current_relationship(&**client, &manager, source, owner, vendor.id)
            .await?
            .is_some(),
    )?;
    drop(client);
    let primary = addresses
        .create_and_attach(
            &manager,
            owner,
            &AddressInput {
                line1: Some("Primary office".into()),
                phone: Some("1234".into()),
                ..AddressInput::default()
            },
            &AttachmentInput {
                is_primary: true,
                dates: period(Some("2000-01-01"), Some("2099-12-31"))?,
                ..AttachmentInput::default()
            },
        )
        .await?;
    let billing = addresses
        .create_and_attach(
            &manager,
            owner,
            &AddressInput {
                line1: Some("Billing office".into()),
                phone: Some("5678".into()),
                ..AddressInput::default()
            },
            &AttachmentInput {
                kinds: vec![AddressKind::Billing],
                dates: period(Some("2024-01-01"), Some("2024-12-31"))?,
                ..AttachmentInput::default()
            },
        )
        .await?;
    let client = db.get().await?;
    for date in ["2024-01-01", "2024-12-31"] {
        prove(
            &format!("billing interval includes {date}"),
            billing_address(&**client, &manager, owner, &day(date)?)
                .await?
                .is_some_and(|a| a.address.id == billing.address.id),
        )?;
    }
    for date in ["2023-12-31", "2025-01-01"] {
        prove(
            &format!("primary fallback outside billing interval on {date}"),
            billing_address(&**client, &manager, owner, &day(date)?)
                .await?
                .is_some_and(|a| a.address.id == primary.address.id),
        )?;
    }
    prove(
        "no address after every interval expires",
        billing_address(&**client, &manager, owner, &day("2100-01-01")?)
            .await?
            .is_none(),
    )?;
    prove(
        "billing owner refuses another tenant",
        matches!(
            billing_address(&**client, &other, owner, &day("2024-06-01")?).await,
            Err(Error::NotFound)
        ),
    )?;
    prove(
        "relationship refuses another tenant",
        current_relationship(&**client, &other, source, owner, role.id)
            .await?
            .is_none(),
    )?;
    prove(
        "relationship direction matters",
        current_relationship(
            &**client,
            &manager,
            company,
            PartyId::Organisation(source),
            role.id,
        )
        .await?
        .is_none(),
    )?;
    drop(client);

    let billing_primary = addresses
        .create_and_attach(
            &manager,
            owner,
            &AddressInput {
                line1: Some("Primary billing office".into()),
                phone: Some("9999".into()),
                ..AddressInput::default()
            },
            &AttachmentInput {
                kinds: vec![AddressKind::Billing],
                is_primary: true,
                priority: 99,
                dates: period(Some("2024-06-01"), Some("2024-06-30"))?,
            },
        )
        .await?;
    let client = db.get().await?;
    prove(
        "primary billing wins over a lower-priority billing address",
        billing_address(&**client, &manager, owner, &day("2024-06-01")?)
            .await?
            .is_some_and(|a| a.address.id == billing_primary.address.id),
    )?;
    drop(client);

    let restrictions = app
        .services
        .get::<bilibi_authz::restrictions::FieldRestrictions>("base/authz.field_restrictions")?;
    restrictions.put(&admin, "addresses", "phone", &[2]).await?;
    let client = db.get().await?;
    let masked = billing_address(&**client, &viewer, owner, &day("2024-06-01")?)
        .await?
        .ok_or("no masked address")?;
    prove(
        "billing phone is removed and marked restricted",
        masked.address.address.phone.is_none()
            && masked.address.restricted.contains(&"phone".to_owned()),
    )?;
    drop(client);

    let matching = NewCompany {
        code: Some(format!("match-{stamp}")),
        ..new.clone()
    };
    let client = db.get().await?;
    let preview = preview_create(&**client, &manager, &matching)
        .await?
        .map_err(|errors| format!("persistent preview: {errors:?}"))?;
    let service = companies.create_matches(&manager, &matching).await?;
    let found = duplicates(&**client, &manager, &DuplicateQuery::from(&matching)).await?;
    prove(
        "public preview, standalone lookup and page service return identical candidates",
        serde_json::to_value(&preview.matches.candidates)?
            == serde_json::to_value(&service.candidates)?
            && serde_json::to_value(&found)?
                == serde_json::to_value(service.candidates.as_ref().unwrap())?,
    )?;
    drop(client);
    grants
        .set_direct(&admin, manager.user(), bilibi_company::LIST, Some(false))
        .await?;
    let client = db.get().await?;
    let hidden = preview_create(&**client, &manager, &matching)
        .await?
        .map_err(|errors| format!("hidden preview: {errors:?}"))?;
    prove(
        "create-only caller learns likely duplicates without candidate details",
        hidden.matches.likely && hidden.matches.candidates.is_none(),
    )?;
    prove(
        "standalone duplicate lookup requires list capability",
        matches!(
            duplicates(&**client, &manager, &DuplicateQuery::from(&matching)).await,
            Err(Error::Forbidden)
        ),
    )?;
    drop(client);
    grants
        .set_direct(&admin, manager.user(), bilibi_company::LIST, None)
        .await?;
    grants
        .set_direct(&admin, manager.user(), bilibi_company::CREATE, Some(false))
        .await?;
    let client = db.get().await?;
    prove(
        "direct create deny refuses preview and create matching",
        matches!(
            preview_create(&**client, &manager, &matching).await,
            Err(Error::Forbidden)
        ) && matches!(
            create_matches(&**client, &manager, &matching).await,
            Err(Error::Forbidden)
        ),
    )?;
    drop(client);
    grants
        .set_direct(&admin, manager.user(), bilibi_company::CREATE, None)
        .await?;
    restrictions
        .put(&admin, "companies", "registration_number", &[1])
        .await?;
    let client = db.get().await?;
    let restricted = preview_create(&**client, &manager, &matching)
        .await?
        .unwrap_err();
    prove(
        "restricted create field remains a page-compatible field refusal",
        restricted.iter().any(|e| e.field == "registration_number"),
    )?;
    drop(client);
    restrictions
        .remove(&admin, "companies", "registration_number")
        .await?;
    let registry = app
        .services
        .get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let bilibi_agent_api::Handler::SafeWrite(handler) = registry.operations["core.company.create"]
        .definition
        .handler
    else {
        return Err("company create operation is not a write".into());
    };

    // Every task holds a connection before any read begins. A nested pool
    // acquisition would exhaust the pool and the deadline would fail this run.
    let count = db.status().max_size;
    let barrier = Arc::new(Barrier::new(count));
    let mut tasks = JoinSet::new();
    for _ in 0..count {
        let (db, barrier, scope) = (db.clone(), barrier.clone(), manager.clone());
        let role = role.id;
        let matching = matching.clone();
        tasks.spawn(async move {
            let mut client = db.get().await?;
            let tx = client.transaction().await?;
            barrier.wait().await;
            let read: &dyn bilibi_tenancy::ReadClient = &*tx;
            let preview = preview_create(read, &scope, &matching).await?.map_err(|_| Error::Invalid)?;
            let candidates = create_matches(read, &scope, &matching).await?;
            let found = duplicates(read, &scope, &DuplicateQuery::from(&matching)).await?;
            if !preview.matches.likely || !candidates.likely || found.is_empty() {
                return Err(Error::Invalid);
            }
            let input = serde_json::json!({"name":matching.name,"code":matching.code,
                "registration_number":matching.registration_number,"email":matching.email,"tax_id":matching.tax_id});
            let output = handler.preview(&*tx, &scope, &input).await.map_err(|_| Error::Invalid)?
                .envelope("core.company.create");
            if output["result"]["duplicate_candidates"] != serde_json::to_value(preview.matches.candidates).map_err(|_| Error::Invalid)? {
                return Err(Error::Invalid);
            }
            detail(read, &scope, company.get()).await?;
            relationship_role_for_code(read, &scope, "customer")
                .await?
                .ok_or(Error::NotFound)?;
            current_relationship(read, &scope, source, owner, role)
                .await?
                .ok_or(Error::NotFound)?;
            let date =
                CalendarDate::try_from("2024-06-01".to_owned()).map_err(|_| Error::Invalid)?;
            billing_address(read, &scope, owner, &date)
                .await?
                .ok_or(Error::NotFound)?;
            tx.rollback().await?;
            Ok::<_, Error>(())
        });
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(result) = tasks.join_next().await {
            result??;
        }
        Ok::<_, Failure>(())
    })
    .await??;
    println!("PASS: {count} concurrent transactions at pool size {count}, all reads and public/agent create previews completed");

    companies
        .retire_relationship_role(&manager, role.id, role.version, None)
        .await?;
    let client = db.get().await?;
    prove(
        "retired role remains resolvable and existing relationship remains current",
        relationship_role_for_code(&**client, &manager, "customer")
            .await?
            .is_some_and(|r| r.retired())
            && current_relationship(&**client, &manager, source, owner, role.id)
                .await?
                .is_some(),
    )?;
    drop(client);
    companies
        .unlink_relationship(&manager, link.id, link.version)
        .await?;
    let client = db.get().await?;
    prove(
        "unlinked relationship is no longer current",
        current_relationship(&**client, &manager, source, owner, role.id)
            .await?
            .is_none(),
    )?;
    drop(client);
    grants
        .set_direct(&admin, manager.user(), bilibi_company::VIEW, Some(false))
        .await?;
    grants
        .set_direct(&admin, manager.user(), "admin.address.view", Some(false))
        .await?;
    let client = db.get().await?;
    prove(
        "direct deny refuses company facts, role and relationship reads",
        matches!(
            detail(&**client, &manager, company.get()).await,
            Err(Error::Forbidden)
        ) && matches!(
            relationship_role_for_code(&**client, &manager, "customer").await,
            Err(Error::Forbidden)
        ) && matches!(
            current_relationship(&**client, &manager, source, owner, role.id).await,
            Err(Error::Forbidden)
        ),
    )?;
    prove(
        "direct deny refuses billing read",
        matches!(
            billing_address(&**client, &manager, owner, &day("2024-06-01")?).await,
            Err(Error::Forbidden)
        ),
    )?;
    drop(client);
    grants
        .set_direct(&admin, manager.user(), bilibi_company::VIEW, None)
        .await?;
    grants
        .set_direct(&admin, manager.user(), "admin.address.view", None)
        .await?;
    println!(
        "Inspect company {} and its addresses in the running app.",
        company.get()
    );
    Ok(())
}
