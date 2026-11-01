//! Shared Software reads and reviewed installs.
use crate::{bound, SoftwareError, CARD_URI, MANAGE, STEP_UP, VIEW};
use bilibi_agent_api::{
    App, Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, APPS, DEFINITIONS,
};
use bilibi_database::MutationTransaction;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::collections::BTreeMap;

impl From<SoftwareError> for AgentError {
    fn from(error: SoftwareError) -> Self {
        match error {
            SoftwareError::Field { field, message } => {
                AgentError::InvalidInput(BTreeMap::from([(field, vec![message])]))
            }
            SoftwareError::Backend(error) => AgentError::Backend(error),
        }
    }
}

enum Read {
    Status,
    Catalogue,
    Releases,
    Open,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/software"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let software = bound()?;
            let value = match self {
                Self::Status => software.status(scope).await?,
                Self::Catalogue => software.catalogue(scope).await?,
                Self::Releases => {
                    software
                        .releases_with(
                            scope,
                            input["refresh"].as_bool() == Some(true),
                            input["prerelease"].as_bool() == Some(true),
                        )
                        .await?
                }
                Self::Open => software.open(scope, input).await?,
            };
            Ok(value.into())
        })
    }
}

enum Write {
    Install,
    Undo,
    Go,
    Cancel,
    Github,
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/software"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let software = bound()?;
            let mut review = match self {
                Self::Install => software.review_install(scope, input).await?,
                Self::Undo => software.review_undo(scope, input).await?,
                Self::Go => software.review_go(scope, input).await?,
                Self::Cancel => software.review_cancel(scope, input).await?,
                Self::Github => software.review_github(scope, input).await?,
            };
            if !matches!(self, Self::Cancel) {
                let methods = software.step_up.enrolled_methods(scope).await?;
                review.facts["two_factor_enabled"] = json!(!methods.is_empty());
                review.facts["two_factor_methods"] = methods
                    .iter()
                    .map(|m| json!({"id": m.id, "label": m.label, "length": m.code_length, "numeric": m.numeric}))
                    .collect();
                review.facts["two_factor_setup_url"] = json!(format!(
                    "{}{}",
                    software.origin,
                    bilibi_ui::StepUp::new(false, "/system/software?tab=releases").setup_href()
                ));
            }
            Ok(review.facts.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let software = bound()?;
            let value = match self {
                Self::Install => software.install_in(tx, scope, input).await?,
                Self::Undo => software.undo_in(tx, scope, input).await?,
                Self::Go => software.go_in(tx, scope, input).await?,
                Self::Cancel => software.cancel_in(tx, scope, input).await?,
                Self::Github => software.github_in(tx, scope, input).await?,
            };
            Ok(value.into())
        })
    }
}

fn empty_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn install_schema() -> Value {
    json!({"type":"object","properties":{
        "bundle":{"type":"string","maxLength":32,"description":"The name of a release file uploaded here, like upload-3 (base.software.releases lists them)."},
        "asset":{"type":"integer","minimum":1,"description":"The GitHub file number from base.software.releases."},
        "repository_id":{"type":"integer","minimum":1,"description":"The GitHub repository number listed with that file."},
        "release_id":{"type":"integer","minimum":1,"description":"The GitHub release number listed with that file."},
        "tag":{"type":"string","maxLength":200,"description":"The setup tag listed with that file."},
        "target":{"type":"string","maxLength":80,"description":"The computer kind listed with that file, such as x86_64-unknown-linux-gnu."},
        "digest":{"type":"string","maxLength":80,"description":"The file's check code from base.software.releases, 64 hex characters."},
        "staging":{"type":"object","required":["instance","asset","job","checked_at","outcome","checks"],"description":"Agent-reported Staging checks, retained in the Production review. Failed reports refuse promotion. Asset differences are shown for the owner to review. Not a signed server receipt.","properties":{
            "instance":{"type":"object","required":["origin","label","role"],"properties":{"origin":{"type":"string","maxLength":2048},"label":{"type":"string","maxLength":200},"role":{"type":"string","enum":["staging"]}}},
            "asset":{"type":"object","description":"The installed_asset.github identity from Staging status. Omit setup_commit when it is null.","required":["repository_id","release_id","asset_id","tag","target","archive_sha256"],"properties":{
                "repository":{"type":"string","maxLength":140},"repository_id":{"type":"integer","minimum":1},"release_id":{"type":"integer","minimum":1},"asset_id":{"type":"integer","minimum":1},
                "tag":{"type":"string","maxLength":200},"target":{"type":"string","maxLength":80},"archive_sha256":{"type":"string","maxLength":64},"program_sha256":{"type":"string","maxLength":64},"setup_commit":{"type":"string","maxLength":64}
            }},
            "job":{"type":"integer","minimum":1},"checked_at":{"type":"string","maxLength":40,"description":"UTC RFC 3339 time."},
            "outcome":{"type":"string","enum":["passed","failed"]},
            "checks":{"type":"array","items":{"type":"object","required":["name","outcome"],"properties":{"name":{"type":"string","maxLength":500},"outcome":{"type":"string","enum":["passed","failed"]}}}}
        }},
        "switch":{"type":"string","enum":["auto","ask"]}
    }})
}
fn github_schema() -> Value {
    json!({"type":"object","properties":{
        "repository":{"type":"string","maxLength":140,"description":"The GitHub repository as owner/name, like your-company/my-bilibi."},
        "repository_id":{"type":"integer","minimum":1,"description":"The GitHub repository number to accept. Leave it out to accept the number GitHub reports now."},
        "token":{"type":"string","maxLength":255,"writeOnly":true,"description":"A fine-grained token that can only read this repository. Leave it out to keep the saved token. Never shown back."},
        "clear_token":{"type":"boolean","description":"true removes the saved read token."},
        "refresh_minutes":{"type":"integer","minimum":1,"maximum":1440,"description":"How many minutes to wait between GitHub checks. 15 checks about four times an hour."},
        "accept":{"type":"boolean","description":"true remembers the GitHub repository number and allows installs from it."}
    }})
}
fn releases_schema() -> Value {
    json!({"type":"object","properties":{
        "refresh":{"type":"boolean","description":"true asks GitHub now, unless GitHub is limiting this server. Otherwise the saved list is used when it is still fresh."},
        "prerelease":{"type":"boolean","description":"true includes files GitHub marks as a pre-release. Drafts are never listed."}
    }})
}
fn releases_output() -> Value {
    json!({"type":"object","description":"instance (origin, role, label); installed_asset (local_release, program_sha256, github with repository_id, release_id, asset_id, tag, target, archive_sha256, program_sha256, setup_commit); uploads; github (repository, repository_id, accepted, target, releases with release_id, asset_id, tag, digest, target, installed; or message)."})
}
fn job_schema() -> Value {
    json!({"type":"object","properties":{
        "job":{"type":"integer","minimum":1,"description":"The job number from base.software.status. Left out: the one that applies."}
    }})
}
fn open_schema() -> Value {
    json!({"type":"object","properties":{
        "propose":{"type":"object","description":"Domains or Extensions to highlight in the catalogue, by id. The catalogue supplies commands for your developer; choosing entries does not start an install.","properties":{
            "domains":{"type":"array","items":{"type":"string","maxLength":64}},
            "extensions":{"type":"array","items":{"type":"string","maxLength":64}}}}
    }})
}
fn open_output() -> Value {
    json!({"type":"object","description":"Everything base.software.status answers, plus catalogue on every instance, proposal (found and unknown ids), next_step (build locally, then install here), and card.resource."})
}
fn status_output() -> Value {
    json!({"type":"object","description":"instance (origin, role, label), installed_asset (local_release, program_sha256, github exact identity as in releases), installed (platform, domains, extensions from the running program), plugins, uploads (release files waiting here: bundle, release, from), job (the one waiting or running, or the last one, with its steps and whether cancel is available), undo (available, job, restores_backup, backup_at), jobs (recent), backups (at and bytes for recent jobs; file only for a server owner), backup_rule (how many backups are kept and what Undo restores)."})
}
fn catalogue_output() -> Value {
    json!({"type":"object","description":"available, url, running_platform, platform versions, domains and extensions with descriptions, repositories, versions, fits_running, tested_with_running, recommended_version, compatibility, official, status (installed, not_installed, update_available), latest_version, warning, local_steps and next_step; plugins and tested sets. When not available: message."})
}
fn review_output() -> Value {
    json!({"type":"object","description":"The review: summary, the change list, warnings, the estimate, what happens, the steps, and the step-up operation key (base.software.change). The commit answers the queued job with its id, state and steps."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "base/software", Provenance { module: "base/software", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key:"base.software.status", title:"Software status", summary:"See what this server runs, follow an install or undo, and read backup times, sizes and the retention rule; owners also see the files.", keywords:&["software","status","build","install","domain","version","job","undo"], capability:VIEW, input:empty_schema, output:status_output, handler:Handler::Read(&Read::Status), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.open", title:"Manage domains and extensions", summary:"Open the Software card to see installed modules, find more modules, follow updates and backups, or undo an install. Includes developer commands for the requested modules.", keywords:&["software","card","open","propose","domain","build"], capability:VIEW, input:open_schema, output:open_output, handler:Handler::Read(&Read::Open), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.catalogue", title:"Available domains and extensions", summary:"Find business modules (Domains) and adaptations or connections (Extensions). See which versions support this Bilibi and whether checks covered the exact code running here. Includes commands for a developer to build and test on their computer.", keywords:&["software","catalogue","domain","version","release","tested"], capability:VIEW, input:empty_schema, output:catalogue_output, handler:Handler::Read(&Read::Catalogue), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.releases", title:"Releases available to install", summary:"List exact GitHub assets, what is installed, and uploaded files waiting to install.", keywords:&["software","release","install","source","download","upload","github"], capability:VIEW, input:releases_schema, output:releases_output, handler:Handler::Read(&Read::Releases), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.install", title:"Install a tested release", summary:"Install a GitHub setup file (asset) or an uploaded file (bundle). Include the Staging report when promoting the same asset. Review the changes first. Confirmation requires the server owner's authenticator code.", keywords:&["software","install","release","staging","production","upload","github"], capability:MANAGE, input:install_schema, output:review_output, handler:Handler::SafeWrite(&Write::Install), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.undo", title:"Undo the last software change", summary:"Return to the previous program. Review which backup Undo restores and what data would be lost. Confirmation requires the server owner's authenticator code.", keywords:&["software","undo","rollback","previous","restore"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Undo), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.go", title:"Confirm installation", summary:"Let a copied install go ahead when it was started with switch ask: the server then pauses for about a minute to back up, update, switch and check. Confirmation requires the server owner's authenticator code.", keywords:&["software","go","ahead","switch","pause","install","ask"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Go), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.cancel", title:"Cancel the software job", summary:"Stop the job waiting or running. A waiting job is removed; a running one stops before the pause. Too late once the pause has started.", keywords:&["software","cancel","stop","job"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Cancel), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.github", title:"GitHub release source", summary:"Set the GitHub repository this server installs setup files from, the optional read token, and how often to check. Accept the repository before installing. Confirmation requires the server owner's authenticator code.", keywords:&["software","github","release","repository","token"], capability:MANAGE, input:github_schema, output:review_output, handler:Handler::SafeWrite(&Write::Github), channels:Channels::Every, step_up:&[STEP_UP] },
    ],
    guides: &[Guide { key:"base.software", title:"Discover and install software", summary:"Find modules, prepare tested updates on your computer, install them with backups, undo or cancel.", keywords:&["software","domain","extension","build","install","catalogue","undo"], capability:VIEW,
        body:"Start with base.software.status to see what this server runs. Read base.software.catalogue or open the Software card with propose.domains or propose.extensions for what the person asked for. Each entry says what it does, which version supports this Bilibi, whether checks covered the exact code running here, and local_steps. Ask your developer to add the modules, build Bilibi on their computer, and test it before installing here. In that local checkout, run the first local_steps command, which clones the official repository at the recommended commit into apps/domains/<id> or apps/extensions/<id>, then run python3 scripts/discover-modules.py, ./scripts/build-assets.sh --check, cargo metadata --format-version 1 > /dev/null to resolve the mounted Cargo.lock, python3 scripts/discover-modules.py --pin, cargo build --release --locked -p bilibi-web, and ./target/release/bilibi-web status. Run the composition locally and check it in the browser. Use docs/server-install.md to prepare its release file. Servers never build software. The public BelimbingProject/bilibi-app-maker can build the pinned composition in GitHub Actions and publish a complete setup archive. For installation read base.software.releases, then plan base.software.install with bundle (a file uploaded here) or asset (a setup file from the GitHub repository named in the settings, with repository_id, release_id, tag, target and digest from that same list). Uploading a release file still works. Install and undo back up, switch, check and switch back automatically on failure through the installer. Install, undo, go and GitHub settings require the owner's current authenticator code every time. Follow jobs and recorded backups with status. Show each backup's time and size, the file when it is given, and the backup_rule sentence once. An install with switch ask waits after copying: plan base.software.go to proceed. Cancel is available until the pause starts. Only a server owner can change software, because an install affects every company on this server. For test on Staging then Production, use two separately authenticated connections. Identify each by origin and role, never label alone. Install the exact repository_id, release_id, asset_id, tag, target and archive digest on Staging with its own reviewed plan and fresh 2FA. Wait for completion; compare installed_asset.github and run relevant business workflows. Retain the Staging instance, job, asset identity (including program_sha256 and setup_commit when recorded; omit a null setup_commit from the report), UTC checked_at, outcome and named checks. Keep failed results and stop promotion on failure. Re-read Production status and releases immediately before making a separate Production plan for that exact asset; include the report as staging so the owner sees the checks, backup and pause wording. Obtain separate consent and a fresh code; never precommit Production in a long Staging plan. After completion compare the installed identity again. The review shows whether this is the same file, the same setup tag and commit built for a different computer type, or a different setup; it says so when the setup could not be checked. These comparisons inform the owner and never refuse an install. A tag or local release number alone proves nothing. Staging reports are agent supplied, not server verified receipts. Manual installs do not require a Staging report. Never give an agent the authenticator seed or GitHub token." }],
}) }

/// How an agent works with the card, for the plugin package's skill.
const CARD_SKILL: &str = "# Software discovery and installation\n\nCall open_software to find business features (Domains) and adaptations or connections (Extensions), inspect this instance or install a tested release. Pass propose.domains or propose.extensions with the requested catalogue ids; they highlight entries, never request a server build. The card gives a description, compatible version, records of checks on the exact code and exact local_steps. Ask your developer to add the modules, build Bilibi on their computer, and test it before installing here. Follow those steps in a local checkout, run the result and check it in the browser. Servers never build. Use the public BelimbingProject/bilibi-app-maker to build and publish pinned setup archives in GitHub Actions. A server can install a setup file from the GitHub repository named in its settings. Uploading a release file still works. For a tested release, the person reviews and confirms install or undo in the card with their authenticator code. Do not plan or commit those writes yourself while the card is open. Follow the job and explain its outcome. If the host cannot show cards, read base.software.catalogue and status for discovery, then plan install or undo and show the full review before the person confirms. Every install, undo and go-ahead asks for a fresh code on every instance. Only a server owner may install software because it affects every company. For test on Staging then Production, use two separately authenticated connections. Identify each by origin and role, never label alone. Install the exact repository_id, release_id, asset_id, tag, target and archive digest on Staging with its own reviewed plan and fresh 2FA. Wait for completion; compare installed_asset.github and run relevant business workflows. Retain the Staging instance, job, asset identity (including program_sha256 and setup_commit when recorded; omit a null setup_commit from the report), UTC checked_at, outcome and named checks. Keep failed results and stop promotion on failure. Re-read Production status and releases immediately before making a separate Production plan for that exact asset; include the report as staging so the owner sees the checks, backup and pause wording. Obtain separate consent and a fresh code; never precommit Production in a long Staging plan. After completion compare the installed identity again. The review shows whether this is the same file, the same setup tag and commit built for a different computer type, or a different setup; it says so when the setup could not be checked. These comparisons inform the owner and never refuse an install. A tag or local release number alone proves nothing. Staging reports are agent supplied, not server verified receipts. Manual installs do not require a Staging report. Never give an agent the authenticator seed or GitHub token.\n";

inventory::submit! { Contribution::new(APPS, "base/software", Provenance { module: "base/software", source: file!(), line: line!() }, &App {
    uri: CARD_URI,
    tool: "open_software",
    title: "Manage domains and extensions",
    description: "Open Software: find business features (Domains) and adaptations or connections (Extensions), compatible versions, records of checks on the exact code and local build steps; see what this server runs; install a tested release or undo with the owner's code. Pass propose.domains or propose.extensions to highlight requested entries. Servers never build software.",
    source: bilibi_agent_api::AppSource::Operation("base.software.open"),
    html: include_str!("../card/software-v1.html"),
    connect_domains: &[],
    resource_domains: &[],
    skill: CARD_SKILL,
}) }
