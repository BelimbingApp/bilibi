//! Manual K15 walkthrough against an explicitly disposable, freshly migrated
//! database: the Core bindings behind Base's company picker, locale
//! bootstrap, company-owned roles and private files. Real app services and
//! walkthrough-fixture sessions, no assertions: it prints each outcome for
//! inspection. It installs the fixture, archives company 4, files a
//! walkthrough address and stores files under `BILIBI_K15_DEMO_ROOT`, so
//! never point it at a shared database.
//!
//! ```sh
//! BILIBI_COMPANY_DEMO=disposable BILIBI_K15_DEMO_ROOT=/tmp/k15-files \
//!   cargo run -p bilibi-company --example k15-owner-binders --locked
//! ```
use bilibi_artifacts::{Artifacts, Reference};
use bilibi_authz::{
    grants::Grants,
    roles::{self, NewRole, Query, Sort},
};
use bilibi_company::{
    CompanyService, ATTACHMENT_SUBJECT as COMPANY_SUBJECT, FILES as COMPANY_FILES, SERVICE,
};
use bilibi_database::{pool, MutationBoundary};
use bilibi_locale::Locales;
use bilibi_media::{AttachmentSubject, Media, Rules, Upload};
use bilibi_party_ids::OrganisationId;
use bilibi_person::{ATTACHMENT_SUBJECT as PERSON_SUBJECT, FILES as PERSON_FILES};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{PartyProofs, Scope};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Error = Box<dyn std::error::Error>;

fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}
fn names(companies: &[bilibi_tenancy::NamedOrganisation]) -> Vec<String> {
    companies
        .iter()
        .map(|c| {
            format!(
                "{}#{}{}",
                c.name,
                c.id.get(),
                if c.archived { " (archived)" } else { "" }
            )
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::var("BILIBI_COMPANY_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_COMPANY_DEMO=disposable for this walkthrough".into());
    }
    let root = std::env::var("BILIBI_K15_DEMO_ROOT")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35573")?;
    let companies = app.services.get::<CompanyService>(SERVICE)?;
    let parties = app.services.get::<PartyProofs>("base/tenancy.parties")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    let locales = app.services.get::<Locales>("base/locale.locales")?;
    let settings = app.services.get::<Settings>("base/settings.store")?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let media = app.services.get::<Media>("base/media.store")?;
    let artifacts = app.services.get::<Artifacts>("base/artifacts.store")?;
    async fn login(app: &bilibi_web::App, email: &str) -> Result<Scope, Error> {
        let login = app
            .user
            .login(email, bilibi_walkthrough_fixture::PASSWORD)
            .await?;
        Ok(app.security.guard(&login.token).await?)
    }
    let admin = login(&app, "admin@example.test").await?;
    let manager = login(&app, "manager@example.test").await?;
    let viewer = login(&app, "viewer@example.test").await?;
    let other = login(&app, "manager@other.example.test").await?;
    // Reconciliation gives the fixture roles their declared module grants.
    grants.reconcile().await?;
    let mut client = db.get().await?;
    // Re-runs start from the seeded state.
    client.batch_execute("UPDATE companies SET status='active' WHERE tenant_id=1 AND id=4; DELETE FROM roles WHERE code LIKE 'k15_%'; DELETE FROM address_attachments WHERE tenant_id=1 AND address_id IN (SELECT id FROM addresses WHERE label='K15 walkthrough'); DELETE FROM addresses WHERE label='K15 walkthrough'; DELETE FROM base_settings WHERE key LIKE 'ui.locale%' AND scope_type='global'; DELETE FROM base_settings WHERE key='localization.timezone' AND scope_type='company'").await?;
    let four = OrganisationId::new(4)?;
    show(
        "archive company 4 (admin)",
        companies
            .archive_company(&admin, 4, Some("K15 walkthrough"))
            .await
            .map(|c| c.status),
    );

    println!("== organisation directory (settings picker, role picker)");
    let sql: &tokio_postgres::Client = &client;
    let in_scope = parties.organisations_in_scope(sql, &admin).await?;
    show(
        "in_scope(admin): count, first three, archived",
        (
            in_scope.len(),
            names(&in_scope[..3]),
            names(
                &in_scope
                    .iter()
                    .filter(|c| c.archived)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
        ),
    );
    const SETTINGS_MANAGE: &str = "base.settings.company.manage";
    let selectable = parties
        .selectable_organisations(sql, &admin, SETTINGS_MANAGE)
        .await?;
    show(
        "selectable(admin, settings.company.manage): count, archived offered",
        (selectable.len(), selectable.iter().any(|c| c.archived)),
    );
    show(
        "selectable(manager, settings.company.manage)",
        parties
            .selectable_organisations(sql, &manager, SETTINGS_MANAGE)
            .await
            .map(|c| c.len()),
    );
    show(
        "selectable(viewer, settings.company.manage)",
        parties
            .selectable_organisations(sql, &viewer, SETTINGS_MANAGE)
            .await
            .map(|c| c.len()),
    );
    show(
        "target(admin, company 2)",
        parties
            .organisation_target(sql, &admin, OrganisationId::new(2)?, SETTINGS_MANAGE)
            .await
            .map(|c| names(&[c])),
    );
    show(
        "target(admin, company 4 archived)",
        parties
            .organisation_target(sql, &admin, four, SETTINGS_MANAGE)
            .await
            .map(|c| names(&[c])),
    );
    show(
        "target(admin, company 100 of tenant 2)",
        parties
            .organisation_target(sql, &admin, OrganisationId::new(100)?, SETTINGS_MANAGE)
            .await
            .map(|c| names(&[c])),
    );
    show(
        "target(viewer, company 2)",
        parties
            .organisation_target(sql, &viewer, OrganisationId::new(2)?, SETTINGS_MANAGE)
            .await
            .map(|c| names(&[c])),
    );
    show("settings: write company 2 timezone (admin)", {
        let mut tx = boundary
            .begin(&mut client, admin.mutation_context("settings.company.save"))
            .await?;
        let result = settings
            .put(
                &mut tx,
                &admin,
                Target::Company(OrganisationId::new(2)?),
                "localization.timezone",
                json!("Asia/Kuala_Lumpur"),
            )
            .await;
        tx.commit().await?;
        result
    });
    show("settings: write company 4 (archived) timezone (admin)", {
        let mut tx = boundary
            .begin(&mut client, admin.mutation_context("settings.company.save"))
            .await?;
        let result = settings
            .put(
                &mut tx,
                &admin,
                Target::Company(four),
                "localization.timezone",
                json!("Asia/Kuala_Lumpur"),
            )
            .await;
        tx.rollback().await?;
        result
    });
    let sql: &tokio_postgres::Client = &client;
    show(
        "settings: company 2 timezone resolves",
        settings
            .get(
                sql,
                &admin,
                Target::Company(OrganisationId::new(2)?),
                "localization.timezone",
            )
            .await
            .map(|r| (r.value, r.overridden)),
    );

    println!("== locale bootstrap from the operator's primary company address");
    show(
        "resolve_global before any address",
        locales
            .resolve_global(sql, &admin, None)
            .await
            .map(|r| (r.locale, r.source, r.inferred_country)),
    );
    client.batch_execute("INSERT INTO geonames_countries(iso,iso3,iso_numeric,country,continent,languages,currency_code) VALUES('CH','CHE','756','Switzerland','EU','de-CH,fr-CH,it-CH,rm','CHF') ON CONFLICT DO NOTHING; INSERT INTO addresses(tenant_id,label,country_iso) VALUES(1,'K15 walkthrough','CH'); INSERT INTO address_attachments(tenant_id,address_id,organisation_id,is_primary) SELECT 1,id,1,true FROM addresses WHERE label='K15 walkthrough'").await?;
    let sql: &tokio_postgres::Client = &client;
    show(
        "resolve_global with a Swiss primary address",
        locales
            .resolve_global(sql, &admin, None)
            .await
            .map(|r| (r.locale, r.source, r.inferred_country, r.persisted)),
    );
    show(
        "resolve(manager) follows the installation",
        locales.resolve(sql, &manager, None).await.map(|r| r.locale),
    );
    show("adopt_inferred (operator)", {
        let bootstrap = app
            .services
            .get::<bilibi_address::AddressService>(bilibi_address::SERVICE)?
            .platform_operator_bootstrap()
            .await?;
        match bootstrap {
            Some(bootstrap) => {
                let mut tx = boundary
                    .begin(&mut client, admin.mutation_context("locale.adopt"))
                    .await?;
                let adopted = locales.adopt_inferred(&mut tx, &admin, &bootstrap).await;
                tx.commit().await?;
                adopted.map(|r| r.map(|r| (r.locale, r.persisted)))
            }
            None => Ok(None),
        }
    });
    let sql: &tokio_postgres::Client = &client;
    show(
        "resolve_global after adoption",
        locales
            .resolve_global(sql, &admin, None)
            .await
            .map(|r| (r.locale, r.source, r.persisted)),
    );

    println!("== company-owned custom roles");
    let role = |company_id: Option<i64>, code: &str| NewRole {
        company_id,
        name: format!("K15 {code}"),
        code: format!("k15_{code}"),
        description: String::new(),
    };
    let describe = |outcome: bilibi_authz::grants::CreatedRole| match outcome {
        bilibi_authz::grants::CreatedRole::Created(id) => format!("created role {id}"),
        bilibi_authz::grants::CreatedRole::Refused(errors) => format!(
            "refused company_id={:?} name={:?} code={:?}",
            errors.company_id, errors.name, errors.code
        ),
    };
    show(
        "create (admin, company 2)",
        grants
            .create_role(&admin, &role(Some(2), "owned"))
            .await
            .map(describe),
    );
    show(
        "create (admin, no company)",
        grants
            .create_role(&admin, &role(None, "unowned"))
            .await
            .map(describe),
    );
    show(
        "create (admin, company 4 archived)",
        grants
            .create_role(&admin, &role(Some(4), "archived"))
            .await
            .map(describe),
    );
    show(
        "create (admin, company 100 of tenant 2)",
        grants
            .create_role(&admin, &role(Some(100), "cross"))
            .await
            .map(describe),
    );
    let sql: &tokio_postgres::Client = &client;
    let visible: Vec<i64> = parties
        .organisations_in_scope(sql, &admin)
        .await?
        .iter()
        .map(|c| c.id.get())
        .collect();
    let page = roles::list(
        sql,
        &admin,
        &visible,
        &Query {
            search: "k15".into(),
            sort: Sort::Name,
            descending: false,
            page: 1,
            page_size: 25,
        },
    )
    .await?;
    show(
        "roles list search k15: (name, company_id)",
        page.entries
            .iter()
            .map(|r| (r.name.clone(), r.company_id))
            .collect::<Vec<_>>(),
    );
    show(
        "roles list with no visible company hides owned custom roles",
        roles::list(
            sql,
            &admin,
            &[],
            &Query {
                search: "k15".into(),
                sort: Sort::Name,
                descending: false,
                page: 1,
                page_size: 25,
            },
        )
        .await
        .map(|p| p.total_entries),
    );
    show(
        "fixture roles after reconciliation: (name, is_system, company_id)",
        roles::list(
            sql,
            &admin,
            &visible,
            &Query {
                search: "manager".into(),
                sort: Sort::Name,
                descending: false,
                page: 1,
                page_size: 25,
            },
        )
        .await
        .map(|p| {
            p.entries
                .iter()
                .map(|r| (r.name.clone(), r.is_system, r.company_id))
                .collect::<Vec<_>>()
        }),
    );

    println!("== private files: Media attachment subjects");
    {
        let mut tx = boundary
            .begin(&mut client, admin.mutation_context("k15.configure"))
            .await?;
        for (key, value) in [
            ("media.disks", json!({"local": root})),
            ("media.attachment_disk", json!("local")),
            ("artifacts.storage_root", json!(root)),
            ("artifacts.retention_days", json!(30)),
        ] {
            settings
                .put(&mut tx, &admin, Target::Global, key, value)
                .await?;
        }
        tx.commit().await?;
    }
    let upload = || Upload {
        filename: "evidence.txt".into(),
        bytes: b"K15 private file\n".to_vec(),
    };
    let company_subject = |id: &'static str| AttachmentSubject {
        kind: COMPANY_SUBJECT,
        id,
    };
    let person_subject = |id: &'static str| AttachmentSubject {
        kind: PERSON_SUBJECT,
        id,
    };
    let uploaded = media
        .upload_attachment(
            &mut client,
            &manager,
            company_subject("2"),
            upload(),
            Rules::default(),
        )
        .await;
    show(
        "upload to company 2 (manager, tenant-wide reach)",
        uploaded
            .as_ref()
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );
    show(
        "upload to company 2 (viewer: view only)",
        media
            .upload_attachment(
                &mut client,
                &viewer,
                company_subject("2"),
                upload(),
                Rules::default(),
            )
            .await
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );
    show(
        "upload to company 4 (archived)",
        media
            .upload_attachment(
                &mut client,
                &manager,
                company_subject("4"),
                upload(),
                Rules::default(),
            )
            .await
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );
    show(
        "upload to company 100 (tenant 2) as tenant 1 manager",
        media
            .upload_attachment(
                &mut client,
                &manager,
                company_subject("100"),
                upload(),
                Rules::default(),
            )
            .await
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );
    show(
        "upload to company 1 as tenant 2 manager",
        media
            .upload_attachment(
                &mut client,
                &other,
                company_subject("1"),
                upload(),
                Rules::default(),
            )
            .await
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );
    if let Ok(attachment) = &uploaded {
        let sql: &tokio_postgres::Client = &client;
        show(
            "download company 2 attachment (viewer may view)",
            media
                .download_attachment(sql, &viewer, &attachment.public_id)
                .await
                .map(|f| (f.filename, f.bytes.len()))
                .map_err(|e| e.to_string()),
        );
        show(
            "download company 2 attachment (tenant 2 manager)",
            media
                .download_attachment(sql, &other, &attachment.public_id)
                .await
                .map(|f| f.filename)
                .map_err(|e| e.to_string()),
        );
    }
    show(
        "upload to person 2 (manager)",
        media
            .upload_attachment(
                &mut client,
                &manager,
                person_subject("2"),
                upload(),
                Rules::default(),
            )
            .await
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );
    show(
        "upload to person 2 (viewer)",
        media
            .upload_attachment(
                &mut client,
                &viewer,
                person_subject("2"),
                upload(),
                Rules::default(),
            )
            .await
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );
    show(
        "upload to person 999 (missing)",
        media
            .upload_attachment(
                &mut client,
                &manager,
                person_subject("999"),
                upload(),
                Rules::default(),
            )
            .await
            .map(|a| a.state)
            .map_err(|e| e.to_string()),
    );

    println!("== private files: Artifacts owners");
    let one = OrganisationId::new(1)?;
    let reference = |subject: &str| Reference {
        subject: subject.into(),
        kind: "evidence".into(),
    };
    let documents = artifacts.for_owner(&manager, one, &COMPANY_FILES);
    show(
        "company document under company 1 (manager's own)",
        documents
            .put(&mut client, reference("1"), b"K15 document", "text/plain")
            .await
            .map(|m| (m.id, m.content_type))
            .map_err(|e| e.to_string()),
    );
    show(
        "company document whose reference names company 3",
        documents
            .put(&mut client, reference("3"), b"K15 document", "text/plain")
            .await
            .map(|m| m.id)
            .map_err(|e| e.to_string()),
    );
    show(
        "company document (viewer)",
        artifacts
            .for_owner(&viewer, one, &COMPANY_FILES)
            .put(&mut client, reference("1"), b"K15 document", "text/plain")
            .await
            .map(|m| m.id)
            .map_err(|e| e.to_string()),
    );
    show(
        "company document under archived company 4 (admin)",
        artifacts
            .for_owner(&admin, four, &COMPANY_FILES)
            .put(&mut client, reference("4"), b"K15 document", "text/plain")
            .await
            .map(|m| m.id)
            .map_err(|e| e.to_string()),
    );
    let person_documents = artifacts.for_owner(&manager, one, &PERSON_FILES);
    show(
        "person 1 document filed under company 1 (manager)",
        person_documents
            .put(
                &mut client,
                reference("1"),
                b"K15 person document",
                "text/plain",
            )
            .await
            .map(|m| (m.id, m.content_type))
            .map_err(|e| e.to_string()),
    );
    show(
        "person 999 document (missing person)",
        person_documents
            .put(
                &mut client,
                reference("999"),
                b"K15 person document",
                "text/plain",
            )
            .await
            .map(|m| m.id)
            .map_err(|e| e.to_string()),
    );
    show(
        "person document (viewer)",
        artifacts
            .for_owner(&viewer, one, &PERSON_FILES)
            .put(
                &mut client,
                reference("1"),
                b"K15 person document",
                "text/plain",
            )
            .await
            .map(|m| m.id)
            .map_err(|e| e.to_string()),
    );
    show(
        "person document filed under archived company 4 (admin)",
        artifacts
            .for_owner(&admin, four, &PERSON_FILES)
            .put(
                &mut client,
                reference("1"),
                b"K15 person document",
                "text/plain",
            )
            .await
            .map(|m| m.id)
            .map_err(|e| e.to_string()),
    );
    let sql: &tokio_postgres::Client = &client;
    let stored = sql
        .query(
            "SELECT owner_id,owner_adapter,company_id,subject FROM base_artifacts WHERE tenant_id=1 AND owner_id LIKE 'core.%' ORDER BY id",
            &[],
        )
        .await?;
    show(
        "stored artifacts (owner_id, adapter, company, subject)",
        stored
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, i64>(2),
                    r.get::<_, String>(3),
                )
            })
            .collect::<Vec<_>>(),
    );
    Ok(())
}
