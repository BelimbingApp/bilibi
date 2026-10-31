# Private artifacts (A1, A2)

Use `base/artifacts.store` and `Artifacts::for_owner(scope, company, &ConcreteOwner)`
for sensitive uploaded/generated documents. Implement `Owner` in the business
module and declare a dependency on `base/artifacts`. Select the concrete adapter
in server code; never select one by a request parameter. Its stable `owner_id`
and Rust type name are persisted together. Renaming either needs an explicit
owner-reviewed data migration. There is no Base query of Company tables.

Every callback must prove a live company in the tenant, current actor authority,
and the live subject for that operation. `Purge` has no reference and requires
company maintenance access. Base reauthenticates the sealed account on every
operation and refuses changed identity or lost desk access to the bound company. The file company may differ from the selected desk when the person still has access there; the owner still authorizes every action. System-principal and approved privacy actions retain their existing authority.
The owner validates upload MIME/content
and scanning policy; `content_type` is descriptive metadata only.

```rust,ignore
let documents = artifacts.for_owner(&scope, company_id, &DocumentOwner);
let metadata = documents.put(&mut client,
    Reference { subject: record_id, kind: "evidence".into() },
    &bytes, "application/pdf").await?;
let document = documents.read(&mut client, &metadata.id).await?;
documents.delete(&mut client, &metadata.id).await?;
let report = documents.recover(&mut client, 100).await?;
```

These methods require a plain PostgreSQL `Client`, outside any business
transaction. Reservations and tombstones must commit before filesystem work.
No file path, filename or content enters audit payloads. Metadata returned to an
owner contains no storage path. Reads hold a row lock, recheck permission and
expiry, validate 0600 regular-file mode, byte count and SHA-256, and commit audit
evidence before bytes escape. Owners' authenticated download routes must call
`read` every time and return attachments with `Cache-Control: private, no-store`
and `X-Content-Type-Options: nosniff`. A1 adds no public download route or screen.

Operator Settings owns these installation values, gated by
`admin.system.artifacts.manage` and explicit platform operator identity:

- `artifacts.storage_root` (Private document folder): the server's private `data/artifacts` folder, saved by `seed-production`. Installed servers use `BILIBI_INSTANCE_DIR/data/artifacts`; a local checkout uses `<working directory>/data/artifacts`.
- `artifacts.retention_days`: 2557 days (7 years) unless an administrator sets another value. This is the declared default; the seed does not store it.
- `artifacts.max_bytes`: initially 10 MiB, maximum 512 MiB.
- `artifacts.purge_batch_size` (100), `artifacts.purge_retry_minutes` (60) and
  `artifacts.purge_max_attempts` (5): retention purge pacing, see below.

The first document creates the built-in default folder (`data` and `data/artifacts` under the instance folder) with 0700 mode. A folder an administrator has chosen is never created: if it is missing, the save is refused with a message naming the folder and the Private document folder setting. Existing folders must already have safe permissions; Bilibi never changes them. The seed preserves every saved administrator value, including an explicitly cleared policy. Administrators can change both settings in Global settings. Files use exclusive random UUID names,
0600 mode and `O_NOFOLLOW`. Root components cannot be symlinks or named
public/static/assets. The root and parents must be controlled by trusted OS
operators; never expose them through a proxy/static server. Root changes affect
new artifacts only. Keep old roots accessible until their records are purged.
Back up metadata and the durable private volume together.

Publication commits an audited reservation, locks it, writes/syncs bytes and the
directory, rechecks authority, then publishes. If writing, permission, SQL or
audit fails, the inaccessible reservation remains tracked. An ambiguous commit
may have published: never blindly remove bytes after a database error.

Deletion commits a tombstone first. Physical cleanup then holds the row lock,
unlinks/syncs the directory and marks the tombstone purged in an audited commit.
A failed unlink leaves a retryable tombstone. If the purge commit fails after
unlink, retry accepts an absent file and commits the evidence. Purged metadata
remains; database guards forbid hard deletion, authority substitution and
lifecycle reversal.

`recover` authorizes maintenance before selecting an ordered bounded batch for
this tenant/company/concrete owner, then reauthorizes deletion per candidate.
It reconciles pending tombstones and unpublished reservations older than one
hour. Rechecking under the row lock protects a writer that published after
candidate selection. Filesystem operations are synchronous and bounded while
that lock is held: cancellation cannot leave a background writer creating bytes
after recovery releases the reservation. Tokio multithread runtimes use `block_in_place` to let other tasks run; use
a dedicated worker/runtime thread when using a single-thread runtime. Recovery reports individual errors and keeps failed
rows eligible; callers own retry pacing and operational alerts. `recover` skips
documents held for an operator (see Retention purge).

## Simple PDFs (A3)

Use `artifacts::pdf::render` for text and table documents rather than a
module-local PDF serializer, and store through `Owned::generate_pdf` or
`Owned::store_pdf` rather than calling `put` with unchecked bytes. HTML
printing and signed render URLs are below (A4); AES-256 passwords and queued
rendering are in the last sections (A5).

Implement `PdfOwner` (an `Owner` extension) on the same trusted adapter. `generate_pdf`
authorizes `Create` first, then calls `render_pdf(scope, company, reference,
data)`. The owner selects what the document contains from its own records and
the request `data`, formats dates and exact numbers into strings, and returns
complete PDF bytes. `generate_pdf` validates them, then `put` stores them as
`application/pdf` (reserve, publish, reauthorize, audit). Nothing is rendered
for an unauthorized actor, and refused output stores nothing.

```rust,ignore
impl PdfOwner for DocumentOwner {
    fn render_pdf<'a>(&'a self, scope: &'a Scope, company: OrganisationId,
        reference: &'a Reference, data: &'a Value) -> RenderFuture<'a> {
        Box::pin(async move {
            pdf::render(&Document {
                title: "Record summary".into(),
                metadata: Metadata { author: Some("Document owner".into()), ..Default::default() },
                blocks: vec![
                    Block::Text("Facts selected by the owning module.".into()),
                    Block::Table { headers: vec!["Record".into(), "Value".into()],
                        rows: vec![vec!["A".into(), "12.50".into()]] },
                    Block::PageBreak,
                ],
            })
        })
    }
}
let metadata = documents.generate_pdf(&mut client, reference, &json!({})).await?;
```

Layout is fixed and matches Bilimbi byte for byte: A4, built-in Courier 10 pt, 85
characters per line, 48 lines per page, equal-width columns (1–20) that wrap
without truncation, repeating table headers, rows that fit a page kept together
(larger rows continue across pages), blank lines preserved, tabs as spaces, CR/CRLF
as newlines. Explicit breaks never leave an empty trailing page. At most 2000
pages per document; split larger reports. Body text is printable ASCII plus Latin-1
U+00A0–U+00FF; anything else is `Error::UnsupportedText`, never substituted.
Metadata (title, `author`, `subject`, `keywords`) supports Unicode. Invalid shapes
return `Error::InvalidDocument`; errors never echo contents. Output is
deterministic: no timestamps, IDs or environment data, and content operands are
escaped so text cannot inject PDF syntax.

`pdf::validate` runs on every PDF before storage, whoever made it: `%PDF-` header,
`%%EOF` trailer and a `startxref` offset landing on an xref table or indirect
object. It is not a complete validator; the producing renderer owns content. Size
and retention policy are the existing `artifacts.max_bytes` and
`artifacts.retention_days` settings.


## Records-grade retention and legal hold (A2)

Documents a law requires the business to keep (statements, invoice scans,
contracts) are stored with a retain-until instead of an expiry. The owner derives
the time from its own obligation; Base never guesses a period. Every call below
needs an owner `Operation::Retain` grant, except `retention_history`, which needs
`Read`. Retention is irreversible, so grant it to people trusted with that
commitment.

```rust,ignore
let metadata = documents.put_retained(&mut client, reference, &bytes, "application/pdf",
    Retention { retain_until, legal_hold: false, reason: Some("Income Tax Act 1967 s.82".into()) }).await?;
documents.retain(&mut client, &id, later, Some("contract renewed")).await?;
documents.place_hold(&mut client, &id, Some("case 12")).await?;
documents.release_hold(&mut client, &id, None).await?;
let events = documents.retention_history(&client, &id).await?; // oldest first
```

A records-grade document has `expires_at: None`; reads keep working before and
after retain-until, and `purge_expired` never selects it. `delete` answers
`Error::LegalHold` while a hold stands and `Error::Retained` before retain-until
(each refusal commits an `artifacts.delete_refused` audit action); after
retain-until an explicit, `Delete`-authorized call deletes it and nothing deletes
it automatically. `retain` makes an ordinary document records-grade (its expiry is
gone for good, even if already expired but not yet purged) or moves retain-until
later: equal or earlier is `RetentionNotExtended`. Retain-until is a `SystemTime`
that must lie in the future (at most 200 years) and a reason is at most 255
characters and never document content, else `InvalidRetention`. A hold needs a
retained document (`NotRetained`); placing one twice is `AlreadyOnHold` and
releasing a missing one is `NotOnHold`. Retention at creation is applied in the
publication transaction, so a failed upload stays an ordinary reservation that
`recover` can tombstone.

The database guards the same rules for any client (`base_artifacts_retention_guard`,
`migrations/002-retention.sql`): retain-until cannot be cleared or moved earlier, a
retained row cannot be tombstoned, purged or removed while on hold or before
retain-until, exactly one of expiry and retain-until is set, and a hold needs
retain-until. Each change writes one row to `base_artifact_retention_events`
(`retained`, `retention_extended`, `hold_placed`, `hold_released`) with the
retain-until and hold state after it, the reason, and the attributed actor (user,
agent or system principal, plus any impersonator). PostgreSQL stamps `recorded_at`;
updates, deletes and truncates are refused, so history survives tombstones. The
change is also an `artifacts.<event>` audit action and a refused audit log refuses
the change.

### Retention purge

`purge_expired` authorizes `Purge` (nil reference), then considers an ordered
batch of this owner/company's expired ordinary documents and unfinished deletes,
rechecking `Delete` per document. A failed or refused purge records its attempt
count, last error code and time (`artifacts.purge_failed`); retention skips that
document until `artifacts.purge_retry_minutes` passes, so later expired documents
are still reached. At `artifacts.purge_max_attempts` it records
`artifacts.purge_held` and the batch excludes it. `list_purge_holds` lists held
documents; `retry_purge` releases the hold (`artifacts.purge_released`) and purges
now, and `resolve_purge` records bytes an operator removed out of band
(`artifacts.purge_resolved`), refusing with `BytesPresent` while the file exists
or while a hold or retain-until still protects the document. All three need
`Purge`. The owning module wires `purge_expired` into its own schedule and
alerting on `failed`; Base enumerates no companies.

### Data-subject requests

**`subject_documents(client, scope, owner, subject)` lists one subject's live documents of an owner** in every company of the tenant, keeping those the owner lets the scope `Read`, however many ([ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md)).

- An approved request carries a sealed `base/personal_data` grant: `disclosed_documents` and `for_disclosure(.., &Disclosure, subject).read` read that subject's documents; `erasable_documents` and `for_erasure(.., &Erasure, subject).delete` delete them (live, or deleted but not purged). The owner is not asked for that subject's documents; the account must still be live, retention and legal holds still refuse, and every step is audited. An erasure's subject must be the grant's person ID under a declared person-keyed kind, or one the owner's `erasure_subject` confirms is that person's (default: none).
- `PdfOptions::retained(Retention)` publishes a rendered PDF records-grade, as `put_retained` does.

### Scheduled sweep across tenants

`purge_expired` needs a person in the document's tenant and company, so the
expired bytes of a company nobody works in again would stay. An owner's
scheduled job (Schedule's `scheduler` principal) calls
`Artifacts::sweep_expired(client, &owner)` instead: one bounded batch of that
owner's expired or deleted ordinary documents in every tenant, with the same
batch size, retry interval and attempt limit. Each document is purged in its
own tenant's audited transaction as the `artifacts-sweeper` system principal,
which holds `artifacts.document.purge` in every live tenant through a standing
grant (a tenant may deny it; its bytes then wait for a person's purge). A
tenant that refuses the sweeper is left out for the rest of the run after its
first refusal, and the batch is filled from other tenants, so a refusing
tenant's older documents never starve another tenant's. It
never takes a retained or held document, records a failure as
`artifacts.purge_failed` (and `artifacts.purge_held` at the limit) like any
purge, and answers `purged` and `failed` for the job to report by code. Base
Bulk Data's `base/bulk_data-file-sweep` is the first caller.

## HTML PDFs (A4)

Use `Owned::render_inline` / `render_view` (`src/render.rs`) to print an owner's
HTML template, rather than a module-local browser, `wkhtmltopdf` call or your
own render route. `Owned::store_pdf` remains the only way to store bytes you made
elsewhere. Implement `HtmlOwner` (an `Owner` extension) on the trusted adapter:
`render_html(scope, company, reference, data)` selects only the data the actor may
see and returns a complete HTML document. Styles, images and fonts must be
embedded (`data:` URLs). Never print user-supplied HTML or URLs, and never log it.

```rust,ignore
let metadata = documents.render_inline(&mut client, reference, &json!({})).await?;
let metadata = documents.render_view(&mut client, reference, &json!({})).await?;
```

Both authorize `Create` before the template runs, print with an isolated Chromium
and store through `store_pdf` (validate, reserve, publish, reauthorize, audit). A
refused or failed render stores nothing. `render_inline` gives the browser the HTML
directly with no network at all. `render_view` has it fetch the page from this
app's single-use signed route, `GET /pdf/render/{token}` (`RENDER_ROUTE`), so the
template runs in the app process; that needs `pdf.render_origin`.

Claims never leave the process. The token (`id.HMAC`, per-boot key, 24 random
bytes) only names the waiting `render_view` call; the route takes no session,
actor, template or data. The call reauthorizes the live actor, then runs the
template. An unknown, forged, expired, replayed or refused token is an empty 404
and a template failure an empty 500, always with `RENDER_HEADERS` (no-store,
noindex, nosniff, a CSP that allows only embedded styles, images and fonts). The
token is a transient capability, never a download link: do not log these paths at a
proxy. Restart discards pending tokens; the caller retries with a fresh URL. In
a multi-node deployment `/pdf/render/*` must reach the node that issued the token.
The Web host mounts the route (`apps/web/src/pdf.rs`) and refuses modules that
overlap it. Nothing else in a template can reach the route: it only answers once.

Resource policy (`src/engine.rs`): the browser talks over a private DevTools pipe
(no debugging port, Node or npm package), keeps its sandbox, runs with scripts and
downloads disabled, and may make one network request, the signed document. Every
other request (subresources, frames, redirects, `file:`, other hosts) is refused
by a DevTools Fetch policy, name resolution is disabled except for the origin and
all other traffic is sent to a dead proxy, so not even a speculative connection
leaves. A template that navigates its own page away (meta refresh, redirect) fails
the render instead of printing a blank page. Profiles and requests live in random
0700 directories; the browser's process group is killed and the directory removed
on success, failure, timeout and cancellation. Tool output is discarded and callers
see only fixed errors (`PdfEngineUnavailable`, `PdfFailed`, `PdfTimeout`,
`RenderOrigin`, `RenderTokensFull`). Crashes can leave `bilibi-pdf-*` temporary
directories: include them in the host's private temp cleanup.

Operator Settings (same capability as the artifact settings): `pdf.chromium.binary`
(bare name searched on `PATH`, or absolute; default `chromium`), `pdf.render_origin`
(http(s) origin the browser can reach, such as `http://127.0.0.1:3000`; empty
disables signed-URL printing), `pdf.signed_url_ttl_seconds` (60),
`pdf.render_timeout_seconds` (30), `pdf.max_render_tokens` (100),
`pdf.paper.format` (`A4`) and `pdf.paper.print_background` (true). Rendered size is
bounded by `artifacts.max_bytes`. The walkthrough ran with Google Chrome 154 and the
Snap Chromium. Password protection and queued rendering are the next sections.

`examples/html-pdf-walkthrough.rs` starts the real app on three local ports and
real Chromium against a disposable database (see the variables above, plus
`BILIBI_PDF_DEMO_OUT` and `BILIBI_PDF_CHROMIUM`). It prints both render modes, a
hostile template whose every outside resource it counts at a probe port, replayed
and altered tokens, expiry, permission revoked in flight and before publication,
and each refusal. It writes `inline.pdf` and `view.pdf` for inspection.

## Password-protected PDFs (A5)

Every PDF writer goes through `Owned::persist_pdf(client, reference, bytes,
&PdfOptions)`; `render_inline_with` / `render_view_with` (and the queued handler)
call it, rather than `store_pdf` plus a module-local encryption step. It validates,
authorizes `Create`, optionally encrypts, then stores through `put`, so a protected
request never stores an unprotected copy: if protection is unavailable or fails
nothing is written. `protect_with_password(client, &source, &user, owner)` stores an
encrypted copy of an existing artifact and leaves the source unchanged. Both return
a `PdfArtifact` (artifact metadata plus template/data version, producer and
production time; `to_map()` has no path or credential). A protected artifact's
data version gains ` (encrypted)`.

Passwords are `Secret` values, not strings: they must be 1-127 bytes of printable
UTF-8 (the R6 handler ignores bytes past 127, so a longer password is refused rather
than silently weakened), and a `Secret` cannot be printed, cloned or serialized. An
empty password is therefore never "no protection"; build the options without one.
An omitted owner password falls back to the user password.

```rust,ignore
let options = PdfOptions::default().template_version("statement-v1").data_version("run-17")
    .password(Secret::new(password)?, None);
let artifact = documents.render_inline_with(&mut client, reference, &data, &options).await?;
let copy = documents.protect_with_password(&mut client, &artifact, &user, Some(&owner)).await?;
```

Encryption runs `qpdf` (10.6 or newer; the walkthrough used 12.3.2) with a
`--job-json-file`, so passwords never appear in argv or the environment. The plaintext
PDF, the job file and the encrypted output live in a random `0700` directory
(`bilibi-pdf-*`, files `0600`) that is removed on success, failure, timeout and
cancellation; qpdf leads its own process group, which is killed on every exit; its
output is discarded and callers see only `PdfProtectionUnavailable`,
`PdfProtectionFailed` or `PdfTimeout`. Output is checked as a PDF and against
`artifacts.max_bytes`. Unlike Bilimbi this needs no Node.js. Operator Settings (same
capability): `pdf.qpdf.binary` (bare name on `PATH` or absolute; default `qpdf`) and
`pdf.qpdf.timeout_seconds` (60, 1-600). `Artifacts::protection_available` says whether
the executable resolves, for a caller deciding whether to offer protection.

## Queued rendering and completion (A5)

An owner renders in the background by contributing its own queue worker and
binding this module's handler. Three pieces, all in the owner's module:

1. `impl QueuedPdfOwner for ConcreteOwner` (it also implements `PdfOwner` and
   `HtmlOwner`; return `Error::Invalid` from the render method a mode never uses):
   `worker_id`, `pdf_request` and `pdf_rendered`.
2. Contributions: `Contribution::new(WORKERS, WORKER_ID, prov, &RENDER_WORKER)` and
   `Contribution::new(HANDLERS, WORKER_ID, prov, &HandlerBinding { service, capability })`
   with the owner's own capability (the actor must hold it on every attempt) and
   the same module provenance.
3. A service whose factory returns `artifacts.pdf_handler(Arc::new(owner))`
   (`Arc<Artifacts>` from `base/artifacts.store`) as the bound `HandlerService`.

`documents.enqueue_pdf(&mut tx, &reference, request_id)` authorizes `Create` and
enqueues in the owner's own `MutationTransaction`, so the owner's request row, the
job and the audit evidence commit or roll back together. Only `company_id`,
`subject`, `kind` and `request_id` enter the job: `validate_render_job` is an exact
allowlist that refuses a password, data, HTML or actor rather than stripping it. The
actor is the one Queue captured and re-proves on every attempt; the owner is fixed by
the worker ID chosen in trusted code. Queue and audit rows hold references and fixed
failure codes only (`pdf_forbidden`, `pdf_request_failed`, `pdf_render_failed`,
`pdf_completion_failed`, ...), never data or errors.

Per attempt the handler re-authorizes `Create`, calls `pdf_request` (the owner
resolves current data, versions, mode `Inline`/`View`/`Document`, routing metadata
and any password from its own secure store, with the `Execution` in hand), renders
and stores through `persist_pdf`, then calls `pdf_rendered` with a `Rendered`
(`request_id`, `job_id`, `PdfArtifact`, metadata: no input data, no password). A
refusal cancels the job; anything else retries up to 3 attempts and then discards.
Delivery is at least once, completion included: a failing callback retries the whole
job and the retry stores another artifact (retention owns cleanup), so key the
business effect on `request_id` and do callback writes inside `execution.begin(..)`,
which fences them against an expired or cancelled lease. A render holds one pool
connection for its duration; `View` needs the Web host's `/pdf/render` route and
`pdf.render_origin`, so a private host without HTTP renders `Inline` or `Document`.
No Domain owner is wired yet: the production worker catalog stays empty.

`examples/queued-pdf-walkthrough.rs` is a running proof, not a test: against a
disposable database it links a specimen owner and runs the real executor, Chromium and
qpdf (see the variables above plus `BILIBI_PDF_QPDF`). It enqueues protected inline,
signed-route and document renders and shows the job arguments, the completion events,
`qpdf --show-encryption` (R=6, AESv3), `pdftotext` refused without and accepted with
the password, an unprotected control, a completion callback that commits then fails
(retry, one idempotent business effect), permission revoked in flight (cancelled, nothing stored),
qpdf missing (retries then discards, nothing stored), a hanging and a failing qpdf
(group killed, no leftover directory), `protect_with_password`, password rules, peer
refusals, the argument allowlist, and a scan of every table for the password (only
the owner's store holds it).

No unit tests. `examples/artifacts-walkthrough.rs` (A1) and
`examples/retention-walkthrough.rs` (A2) are explicit disposable-DB walkthroughs
of real facade calls, raw-SQL guard attempts and injected DB/filesystem outages,
with output for manual inspection. They run no assertions and use no test harness.

Run the walkthrough after migrating an isolated database:

```sh
export DATABASE_URL=postgres://bilibi@127.0.0.1:55481/bilibi_a1
export BILIBI_DISPOSABLE_ARTIFACTS_DEMO=yes
export BILIBI_ARTIFACTS_DEMO_ROOT=/absolute/private/task-directory
cargo run -p bilibi-artifacts --example artifacts-walkthrough
cargo run -p bilibi-artifacts --example artifacts-walkthrough -- recover
```

The first command leaves durable failed reservations/tombstones and a ready
control document. The second starts a fresh process, reconciles the failures
(including a successful unlink whose audit commit failed), and preserves the
control. Repeat `recover` to see an empty successful batch. Fixture clock and
failure triggers are confined to the explicitly opted-in disposable database.

`examples/pdf-walkthrough.rs` does the same for PDFs. With only
`BILIBI_PDF_DEMO_OUT=/absolute/dir` it writes the deterministic fixture PDFs and
prints the renderer refusals (`-- fixtures`); with the disposable-DB variables
above it also generates, stores and reads back an owner-selected PDF and shows
unauthorized, cross-tenant, non-PDF and truncated-output refusals leaving no
artifact behind.

Run the retention walkthrough the same way. It publishes records-grade documents,
exercises holds and refusals, tries raw SQL against the guards, then breaks and
restores storage to show purge retries, an operator hold and recovery. It waits
about 14 seconds for a short retain-until to pass. `report` prints the durable
ledger and retention events from a fresh process:

```sh
cargo run -p bilibi-artifacts --example retention-walkthrough
cargo run -p bilibi-artifacts --example retention-walkthrough -- report
```
