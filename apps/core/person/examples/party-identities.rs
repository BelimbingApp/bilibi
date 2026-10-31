//! Real owner-directory proof on an empty, migrated disposable database.
//! Extends Bilimbi's company naming seam to fixed Company and Person codes.
use bilibi_database::{pool, Error};
use bilibi_party_ids::{OrganisationId, PartyId, PersonId};
use bilibi_person::{PersonInput, PersonKind, PersonService};
use bilibi_tenancy::{Authentication, PartyProofs, PARTY_CODE_LOOKUP_LIMIT};
use std::error::Error as Failure;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
#[tokio::main]
async fn main() -> Result<(), Box<dyn Failure>> {
    if std::env::var("BILIBI_PARTY_PROOF").as_deref() != Ok("disposable") {
        return Err(
            "BILIBI_PARTY_PROOF=disposable and an empty disposable database are required".into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:45482")?;
    app.services
        .get::<bilibi_step_up::StepUp>(bilibi_step_up::SERVICE)?
        .set_enforced(false);
    bilibi_walkthrough_fixture::install(&db).await?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let client = db.get().await?;
    let manager = auth.authenticate(&**client, 1, 1, String::new()).await?;
    let admin = auth.authenticate(&**client, 1, 4, String::new()).await?;
    let other = auth.authenticate(&**client, 2, 5, String::new()).await?;
    let people = app.services.get::<PersonService>(bilibi_person::SERVICE)?;
    // Core kinds may legitimately share a code. The directory retains the kind.
    let person = people
        .create(
            &manager,
            &PersonInput {
                name: "Company 010".into(),
                kind: PersonKind::Human,
                email: Some("private@example.test".into()),
                phone: Some("+60123456789".into()),
            },
        )
        .await?;
    assert_eq!(person.code, "company_010");
    let parties = app.services.get::<PartyProofs>("base/tenancy.parties")?;
    let query = vec![
        person.code.clone(),
        "unknown-party".into(),
        person.code.clone(),
    ];
    let found = parties
        .parties_by_codes(&**client, &manager, &query)
        .await?;
    assert_eq!(found.len(), 2);
    assert!(found
        .iter()
        .any(|p| p.record_kind() == "company" && p.record_id() == 10 && p.code == person.code));
    assert!(found.iter().any(|p| p.record_kind() == "person"
        && p.record_id() == person.id.get()
        && p.name == "Company 010"));
    grants
        .set_direct(&admin, manager.user(), "admin.company.view", Some(false))
        .await?;
    grants
        .set_direct(&admin, manager.user(), bilibi_person::VIEW, Some(false))
        .await?;
    assert!(matches!(
        bilibi_person::read_person(&**client, &manager, person.id).await,
        Err(Error::Forbidden)
    ));
    let company = parties
        .party_identity(
            &**client,
            &manager,
            PartyId::Organisation(OrganisationId::new(10)?),
        )
        .await?;
    let named = parties
        .party_identity(&**client, &manager, PartyId::Person(person.id))
        .await?;
    assert_eq!(
        (company.code.as_str(), named.code.as_str()),
        ("company_010", "company_010")
    );
    assert_eq!(
        parties
            .parties_by_codes(&**client, &manager, &query)
            .await?,
        found
    );
    assert!(parties
        .parties_by_codes(&**client, &other, &query)
        .await?
        .is_empty());
    assert!(matches!(
        parties
            .party_identity(&**client, &other, PartyId::Person(person.id))
            .await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        parties
            .party_identity(&**client, &manager, PartyId::Person(PersonId::new(999999)?))
            .await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        parties
            .parties_by_codes(
                &**client,
                &manager,
                &vec!["code".into(); PARTY_CODE_LOOKUP_LIMIT + 1]
            )
            .await,
        Err(Error::Invalid)
    ));
    assert!(matches!(
        parties
            .parties_by_codes(&**client, &manager, &[String::new()])
            .await,
        Err(Error::Invalid)
    ));
    assert!(parties
        .parties_by_codes(&**client, &manager, &[])
        .await?
        .is_empty());
    let large: Vec<_> = (0..PARTY_CODE_LOOKUP_LIMIT)
        .map(|n| format!("missing_{n}"))
        .collect();
    assert!(parties
        .parties_by_codes(&**client, &manager, &large)
        .await?
        .is_empty());
    grants
        .set_direct(&admin, manager.user(), bilibi_person::VIEW, None)
        .await?;
    let retired = people.retire(&manager, person.id, person.version).await?;
    let identity = parties
        .party_identity(&**client, &manager, PartyId::Person(retired.id))
        .await?;
    assert_eq!(identity.status, "retired");
    assert_eq!(identity.code, person.code);
    println!("PASS: Company/Person code collision keeps typed references; identity readable with Core views denied; personal contacts still refused; unknown/cross-tenant IDs refused; lookup deduplicates and accepts 10000 codes, rejects 10001; retired identity keeps its code");
    Ok(())
}
