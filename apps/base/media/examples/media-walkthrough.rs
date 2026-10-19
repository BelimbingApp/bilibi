//! Manual service walkthrough against an explicitly supplied disposable DB.
//! No test harness or assertions; prints storage outcomes for inspection.
//!
//! `prepare` (default) configures a private disk, stores a PNG and a note,
//! shows rejected uploads, derivative and private-tree rules, subject owners
//! and token refusals, then prints signed stream paths for a running app.
//! `delete <id>` removes an asset tree in a separate process.
use bilibi_database::{pool, MutationBoundary, Result as DbResult};
use bilibi_media::{
    Derivative, Media, NewAsset, Placement, Signer, Subject, SubjectAction, SubjectOwner, Upload,
    SUBJECTS,
};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

/// Tiny valid 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Fixture owner, not a production adapter: only note 1 in tenant 1 is live.
struct Notes;
impl SubjectOwner for Notes {
    fn authorize<'a>(
        &'a self,
        _client: &'a dyn ProofClient,
        scope: &'a Scope,
        action: SubjectAction,
        subject_id: &'a str,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            if scope.tenant() == 1 && subject_id == "1" && action != SubjectAction::Discard {
                Ok(())
            } else {
                Err(bilibi_database::Error::Forbidden)
            }
        })
    }
}
static NOTES: Notes = Notes;
inventory::submit! { Contribution::new(SUBJECTS, "walkthrough.note", Provenance { module: "base/media", source: file!(), line: line!() }, &Subject { owner: &NOTES }) }

fn show(label: &str, result: Result<impl std::fmt::Debug, impl std::fmt::Display>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}

#[tokio::main]
async fn main() -> DbResult<()> {
    if std::env::var("BILIBI_DISPOSABLE_MEDIA_DEMO").as_deref() != Ok("yes") {
        return Err(bilibi_database::Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let root =
        std::env::var("BILIBI_MEDIA_DEMO_ROOT").map_err(|_| bilibi_database::Error::Invalid)?;
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| bilibi_database::Error::Invalid)?)?;
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
    let media = services.get::<Media>("base/media.store")?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?;
    let other_tenant = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?;
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("delete") {
        let id: i64 = args
            .get(2)
            .and_then(|id| id.parse().ok())
            .ok_or(bilibi_database::Error::Invalid)?;
        show(
            "delete (other tenant)",
            media.delete_asset(&mut client, &other_tenant, id).await,
        );
        show("delete", media.delete_asset(&mut client, &actor, id).await);
        show(
            "get after delete",
            media.get_asset(&**client, &actor, id).await,
        );
        return Ok(());
    }

    // Operator setup. Production never appoints an operator implicitly.
    client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING", &[]).await?;
    std::fs::create_dir_all(&root).map_err(|_| bilibi_database::Error::Internal)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| bilibi_database::Error::Internal)?;
    for (key, value) in [
        ("media.disks", json!({"local": root})),
        ("media.stream_ttl_seconds", json!(300)),
        ("media.max_bytes", json!(10_485_760)),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("media.walkthrough.configure"),
            )
            .await?;
        settings
            .put(&mut tx, &actor, Target::Global, key, value)
            .await?;
        tx.commit().await?;
    }
    println!(
        "== storage: disk root mode {:o}",
        std::fs::metadata(&root)
            .map_err(|_| bilibi_database::Error::Internal)?
            .permissions()
            .mode()
            & 0o777
    );

    println!("== accepted uploads");
    let png = media
        .put_upload(
            &mut client,
            &actor,
            "local",
            "walkthrough",
            Upload {
                filename: "../../evil/pixel.png".into(),
                bytes: PNG.to_vec(),
            },
            Some(json!({"purpose":"demo"})),
        )
        .await;
    show(
        "png upload",
        png.as_ref().map(|a| {
            (
                a.id,
                a.original_filename.clone(),
                a.mime_type.clone(),
                a.file_size,
            )
        }),
    );
    let note = media
        .put_upload(
            &mut client,
            &actor,
            "local",
            "walkthrough",
            Upload {
                filename: "note.txt".into(),
                bytes: b"Quarterly figures: private until a signed link is issued.\n".to_vec(),
            },
            None,
        )
        .await;
    show(
        "text upload",
        note.as_ref()
            .map(|a| (a.id, a.original_filename.clone(), a.mime_type.clone())),
    );
    let png = png.map_err(|_| bilibi_database::Error::Internal)?;
    let note = note.map_err(|_| bilibi_database::Error::Internal)?;

    println!("== rejected uploads (nothing may be stored)");
    let before: i64 = client
        .query_one("SELECT count(*) FROM base_media_assets", &[])
        .await?
        .get(0);
    let attacks: [(&str, &str, &[u8]); 6] = [
        ("html page", "page.html", b"<!doctype html><html><body><script>alert(1)</script></body></html>"),
        ("html named .png", "photo.png", b"<html><head><title>x</title></head><body onload=alert(1)>hi</body></html>"),
        ("svg with script", "logo.svg", b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"),
        ("svg without declaration", "logo.png", b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"></svg>"),
        ("xml document", "data.xml", b"<?xml version=\"1.0\"?><root><item>1</item></root>"),
        ("empty file", "empty.png", b""),
    ];
    for (label, filename, bytes) in attacks {
        let result = media
            .put_upload(
                &mut client,
                &actor,
                "local",
                "walkthrough",
                Upload {
                    filename: filename.into(),
                    bytes: bytes.to_vec(),
                },
                None,
            )
            .await;
        show(label, result.map(|a| a.id));
    }
    show(
        "unsafe directory",
        media
            .put_upload(
                &mut client,
                &actor,
                "local",
                "../escape",
                Upload {
                    filename: "a.txt".into(),
                    bytes: b"x".to_vec(),
                },
                None,
            )
            .await
            .map(|a| a.id),
    );
    show(
        "unknown disk",
        media
            .put_upload(
                &mut client,
                &actor,
                "nowhere",
                "walkthrough",
                Upload {
                    filename: "a.txt".into(),
                    bytes: b"x".to_vec(),
                },
                None,
            )
            .await
            .map(|a| a.id),
    );
    show(
        "external disk",
        media
            .put_upload(
                &mut client,
                &actor,
                "external",
                "walkthrough",
                Upload {
                    filename: "a.txt".into(),
                    bytes: b"x".to_vec(),
                },
                None,
            )
            .await
            .map(|a| a.id),
    );
    {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("media.walkthrough.limit"),
            )
            .await?;
        settings
            .put(
                &mut tx,
                &actor,
                Target::Global,
                "media.max_bytes",
                json!(16),
            )
            .await?;
        tx.commit().await?;
    }
    show(
        "over media.max_bytes=16",
        media
            .put_upload(
                &mut client,
                &actor,
                "local",
                "walkthrough",
                Upload {
                    filename: "big.txt".into(),
                    bytes: vec![b'a'; 64],
                },
                None,
            )
            .await
            .map(|a| a.id),
    );
    {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("media.walkthrough.limit"),
            )
            .await?;
        settings
            .delete(&mut tx, &actor, Target::Global, "media.max_bytes")
            .await?;
        tx.commit().await?;
    }
    let after: i64 = client
        .query_one("SELECT count(*) FROM base_media_assets", &[])
        .await?
        .get(0);
    let files = std::fs::read_dir(format!("{root}/tenants/1/walkthrough"))
        .map(|d| d.count())
        .unwrap_or(0);
    println!("rows before={before} after={after}; files on disk={files}");

    // libmagic cannot tell script from prose, and a GIF header can front markup.
    // Both are accepted for what libmagic says they are; safety is in delivery.
    println!("== accepted, but only ever served contained");
    let script_text = media.put_upload(&mut client, &actor, "local", "walkthrough", Upload { filename: "app.js".into(), bytes: b"function go(){ document.cookie; }\nvar x = new XMLHttpRequest();\nwindow.go = go;\n".to_vec() }, None).await;
    show(
        "script-like text",
        script_text.as_ref().map(|a| (a.id, a.mime_type.clone())),
    );
    let polyglot = media
        .put_upload(
            &mut client,
            &actor,
            "local",
            "walkthrough",
            Upload {
                filename: "banner.gif".into(),
                bytes: b"GIF89a<script>alert(1)</script>".to_vec(),
            },
            None,
        )
        .await;
    show(
        "gif/script polyglot",
        polyglot.as_ref().map(|a| (a.id, a.mime_type.clone())),
    );

    println!("== derivative and private-tree rules");
    let thumb = media
        .put_derivative_bytes(
            &mut client,
            &actor,
            Derivative {
                parent_id: png.id,
                kind: "thumbnail",
            },
            Placement {
                disk: "local",
                key: "tenants/1/walkthrough/thumb-1.png",
            },
            PNG.to_vec(),
            None,
        )
        .await;
    show(
        "derivative",
        thumb.as_ref().map(|a| (a.id, a.parent_id, a.kind.clone())),
    );
    show(
        "derivative named original",
        media
            .put_derivative_bytes(
                &mut client,
                &actor,
                Derivative {
                    parent_id: png.id,
                    kind: "original",
                },
                Placement {
                    disk: "local",
                    key: "tenants/1/walkthrough/thumb-2.png",
                },
                PNG.to_vec(),
                None,
            )
            .await
            .map(|a| a.id),
    );
    show(
        "derivative of other tenant's asset",
        media
            .put_derivative_bytes(
                &mut client,
                &other_tenant,
                Derivative {
                    parent_id: png.id,
                    kind: "thumbnail",
                },
                Placement {
                    disk: "local",
                    key: "tenants/2/walkthrough/thumb.png",
                },
                PNG.to_vec(),
                None,
            )
            .await
            .map(|a| a.id),
    );
    show(
        "derivatives of png",
        media
            .list_derivatives(&**client, &actor, png.id)
            .await
            .map(|d| d.len()),
    );
    // An owner registering bytes it placed itself, marked private (the attachment
    // lifecycle does this for real; see attachments-walkthrough).
    let private_key = "tenants/1/private-evidence/receipt.txt";
    let evidence = format!("{root}/tenants/1/private-evidence");
    std::fs::create_dir_all(&evidence).map_err(|_| bilibi_database::Error::Internal)?;
    std::fs::set_permissions(&evidence, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| bilibi_database::Error::Internal)?;
    let secret_path = format!("{root}/{private_key}");
    if !Path::new(&secret_path).exists() {
        std::fs::write(&secret_path, b"private attachment bytes\n")
            .map_err(|_| bilibi_database::Error::Internal)?;
        std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| bilibi_database::Error::Internal)?;
    }
    let private = {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("media.walkthrough.private"),
            )
            .await?;
        let taken = tx
            .sql()
            .query_opt(
                "SELECT id FROM base_media_assets WHERE storage_key=$1",
                &[&private_key],
            )
            .await?;
        let asset = if taken.is_some() {
            None
        } else {
            Some(
                media
                    .register_original(
                        &mut tx,
                        &actor,
                        Placement {
                            disk: "local",
                            key: private_key,
                        },
                        NewAsset {
                            original_filename: Some("receipt.txt".into()),
                            mime_type: Some("text/plain".into()),
                            file_size: Some(25),
                            metadata: None,
                            private: true,
                        },
                    )
                    .await,
            )
        };
        tx.commit().await?;
        asset
    };
    let private_id = match private {
        Some(Ok(asset)) => asset.id,
        _ => client
            .query_one(
                "SELECT id FROM base_media_assets WHERE storage_key=$1",
                &[&private_key],
            )
            .await?
            .get(0),
    };
    show(
        "stream link for private-bound asset",
        media.stream_url(&**client, &actor, private_id).await,
    );
    show(
        "delete private-bound asset",
        media.delete_asset(&mut client, &actor, private_id).await,
    );
    show(
        "derivative of private-bound asset",
        media
            .put_derivative_bytes(
                &mut client,
                &actor,
                Derivative {
                    parent_id: private_id,
                    kind: "thumbnail",
                },
                Placement {
                    disk: "local",
                    key: "tenants/1/walkthrough/thumb-3.png",
                },
                PNG.to_vec(),
                None,
            )
            .await
            .map(|a| a.id),
    );
    println!("private-bound asset id={private_id}");

    println!("== external disk");
    let external = {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("media.walkthrough.external"),
            )
            .await?;
        let taken = tx
            .sql()
            .query_opt(
                "SELECT id FROM base_media_assets WHERE storage_key='avatar-1'",
                &[],
            )
            .await?;
        let asset = if taken.is_some() {
            None
        } else {
            Some(
                media
                    .register_original(
                        &mut tx,
                        &actor,
                        Placement {
                            disk: "external",
                            key: "avatar-1",
                        },
                        NewAsset {
                            metadata: Some(json!({"public_url":"https://cdn.example.test/a.png"})),
                            ..NewAsset::default()
                        },
                    )
                    .await,
            )
        };
        tx.commit().await?;
        asset
    };
    let external_id = match external {
        Some(Ok(asset)) => asset.id,
        _ => client
            .query_one(
                "SELECT id FROM base_media_assets WHERE storage_key='avatar-1'",
                &[],
            )
            .await?
            .get(0),
    };
    show(
        "external display_url",
        media.display_url(&**client, &actor, external_id).await,
    );
    show(
        "external stream_url",
        media.stream_url(&**client, &actor, external_id).await,
    );

    println!("== tenant isolation");
    show(
        "tenant 2 reads tenant 1 asset",
        media
            .get_asset(&**client, &other_tenant, png.id)
            .await
            .map(|a| a.id),
    );
    show(
        "tenant 2 asks for a link",
        media.stream_url(&**client, &other_tenant, png.id).await,
    );

    println!("== signed stream tokens (in process)");
    let link = media
        .stream_url(&**client, &actor, png.id)
        .await
        .map_err(|_| bilibi_database::Error::Internal)?;
    let token = link.split("token=").nth(1).unwrap_or("").to_string();
    show(
        "valid token",
        media
            .read_signed_asset(&**client, png.id, &token)
            .await
            .map(|f| (f.filename.clone(), f.mime_type.clone(), f.bytes.len())),
    );
    show(
        "same token, other asset id",
        media
            .read_signed_asset(&**client, note.id, &token)
            .await
            .map(|f| f.bytes.len()),
    );
    let mut tampered = token.clone();
    tampered.pop();
    tampered.push(if token.ends_with('0') { '1' } else { '0' });
    show(
        "tampered token",
        media
            .read_signed_asset(&**client, png.id, &tampered)
            .await
            .map(|f| f.bytes.len()),
    );
    let forged = token.replacen("v1.1.", "v1.2.", 1);
    show(
        "tenant swapped in token",
        media
            .read_signed_asset(&**client, png.id, &forged)
            .await
            .map(|f| f.bytes.len()),
    );
    let signer = Signer::from_hex(std::env::var("BILIBI_MEDIA_SIGNING_KEY").ok().as_deref());
    let expired = signer
        .sign(png.id, 1, 1)
        .map_err(|_| bilibi_database::Error::Internal)?;
    show(
        "expired token",
        media
            .read_signed_asset(&**client, png.id, &expired)
            .await
            .map(|f| f.bytes.len()),
    );
    show(
        "no token",
        media
            .read_signed_asset(&**client, png.id, "")
            .await
            .map(|f| f.bytes.len()),
    );
    let note_link = media
        .stream_url(&**client, &actor, note.id)
        .await
        .map_err(|_| bilibi_database::Error::Internal)?;
    let note_token = note_link.split("token=").nth(1).unwrap_or("");
    let delivery = media
        .read_signed_asset(&**client, note.id, note_token)
        .await;
    if let Ok(file) = delivery {
        let d = file.delivery(false);
        println!(
            "note delivery: content-type={} {:?}",
            d.content_type, d.headers
        );
    }

    println!("== subject owners");
    show(
        "note 1, tenant 1, download",
        media
            .authorize_subject(
                &**client,
                &actor,
                "walkthrough.note",
                SubjectAction::Download,
                "1",
            )
            .await,
    );
    show(
        "note 2, tenant 1, download",
        media
            .authorize_subject(
                &**client,
                &actor,
                "walkthrough.note",
                SubjectAction::Download,
                "2",
            )
            .await,
    );
    show(
        "note 1, tenant 2, download",
        media
            .authorize_subject(
                &**client,
                &other_tenant,
                "walkthrough.note",
                SubjectAction::Download,
                "1",
            )
            .await,
    );
    show(
        "unregistered subject",
        media
            .authorize_subject(
                &**client,
                &actor,
                "orders.order",
                SubjectAction::Download,
                "1",
            )
            .await,
    );

    println!("== audit evidence");
    for row in client.query("SELECT event,auditable_type,count(*) FROM base_audit_mutations WHERE auditable_type='base/media/asset' GROUP BY 1,2 ORDER BY 1", &[]).await? {
        println!("mutations: {} {} x{}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, i64>(2));
    }
    let leaked: i64 = client.query_one("SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/media/asset' AND (new_values::text LIKE '%tenants/%' OR old_values::text LIKE '%tenants/%')", &[]).await?.get(0);
    println!("audit rows containing a storage key: {leaked}");

    println!("== for the running app");
    println!(
        "png_id={} note_id={} private_id={private_id}",
        png.id, note.id
    );
    println!("png_link={link}");
    for (label, asset) in [("script_link", &script_text), ("polyglot_link", &polyglot)] {
        if let Ok(asset) = asset {
            println!(
                "{label}={}",
                media
                    .stream_url(&**client, &actor, asset.id)
                    .await
                    .map_err(|_| bilibi_database::Error::Internal)?
            );
        }
    }
    println!(
        "note_link={}",
        media
            .stream_url(&**client, &actor, note.id)
            .await
            .map_err(|_| bilibi_database::Error::Internal)?
    );
    println!(
        "expired_link=/media/assets/{}/stream?token={expired}",
        png.id
    );
    println!(
        "private_forged_link=/media/assets/{private_id}/stream?token={}",
        signer
            .sign(private_id, 1, u64::MAX / 2)
            .map_err(|_| bilibi_database::Error::Internal)?
    );
    Ok(())
}
