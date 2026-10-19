//! The software contracts, reached through Agent API: four reads and five
//! reviewed writes (plan, then commit). A commit of install, undo,
//! go or pair asks for the server owner's authenticator code every time, on
//! every instance (`STEP_UP`): the pairing key opens the program for
//! download, so setting it changes the software's reach; cancel changes
//! nothing and asks none. The `open` read answers the `open_software` MCP
//! tool, which shows the card (`card/software-v1.html`) registered here as
//! an MCP App; `releases` says what this instance can install.
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
                Self::Releases => software.releases(scope).await?,
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
    Pair,
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
            let review = match self {
                Self::Install => software.review_install(scope, input).await?,
                Self::Undo => software.review_undo(scope, input).await?,
                Self::Go => software.review_go(scope, input).await?,
                Self::Cancel => software.review_cancel(scope, input).await?,
                Self::Pair => software.review_pair(scope, input).await?,
            };
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
                Self::Pair => software.pair_in(tx, scope, input).await?,
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
        "release":{"type":"integer","minimum":1,"description":"The release number the source instance reports as tested (base.software.releases lists them)."},
        "bundle":{"type":"string","maxLength":32,"description":"Instead of release: the name of a release file uploaded here, like upload-3 (base.software.releases lists them)."},
        "switch":{"type":"string","enum":["auto","ask"]}
    }})
}
fn pair_schema() -> Value {
    json!({"type":"object","properties":{
        "key":{"type":"string","maxLength":128,"description":"The pairing key: 16 to 128 characters, no spaces. Set the same key on the instance this one shares releases with. Never shown back."},
        "clear":{"type":"boolean","description":"true removes the key: this instance then shares no releases and downloads none."}
    }})
}
fn releases_output() -> Value {
    json!({"type":"object","description":"instance; source (available, current, healthy, releases with release, in_service, sha256, platform, domains; or message); uploads (release files waiting here: bundle, release, from, platform, domains, sha256); pairing (set); here (the releases kept on this instance)."})
}
fn job_schema() -> Value {
    json!({"type":"object","properties":{
        "job":{"type":"integer","minimum":1,"description":"The job number from base.software.status. Left out: the one that applies."}
    }})
}
fn open_schema() -> Value {
    json!({"type":"object","properties":{
        "propose":{"type":"object","description":"Domains or Extensions to highlight in the catalogue, by id. Discovery supplies local build instructions; it creates no job.","properties":{
            "domains":{"type":"array","items":{"type":"string","maxLength":64}},
            "extensions":{"type":"array","items":{"type":"string","maxLength":64}}}}
    }})
}
fn open_output() -> Value {
    json!({"type":"object","description":"Everything base.software.status answers, plus catalogue on every instance, proposal (found and unknown ids), next_step (build locally, then install here), and card.resource."})
}
fn status_output() -> Value {
    json!({"type":"object","description":"instance (role, label, installs_from), installed (platform, domains, extensions from the running program), plugins, uploads (release files waiting here: bundle, release, from), job (the one waiting or running, or the last one, with its steps and whether cancel is available), undo (available, job, restores_backup, backup_at), jobs (recent)."})
}
fn catalogue_output() -> Value {
    json!({"type":"object","description":"available, url, running_platform, platform versions, domains and extensions with descriptions, repositories, versions, fits_running, tested_with_running, recommended_version, compatibility, local_steps and next_step; plugins and tested sets. When not available: message."})
}
fn review_output() -> Value {
    json!({"type":"object","description":"The review: summary, the change list, warnings, the estimate, what happens, the steps, and the step-up operation key (base.software.change). The commit answers the queued job with its id, state and steps."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "base/software", Provenance { module: "base/software", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key:"base.software.status", title:"Software status", summary:"Which instance this is, what is installed, the plug-ins, the job waiting or running with its steps, and whether Undo is available.", keywords:&["software","status","build","install","domain","version","job","undo"], capability:VIEW, input:empty_schema, output:status_output, handler:Handler::Read(&Read::Status), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.open", title:"Software card", summary:"What the Software card opens with: the instance, what is installed, the job and Undo state, and the catalogue with local build instructions for requested Domains and Extensions. The same answer as base.software.status and base.software.catalogue together.", keywords:&["software","card","open","propose","domain","build"], capability:VIEW, input:open_schema, output:open_output, handler:Handler::Read(&Read::Open), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.catalogue", title:"Software catalogue", summary:"Discover official Domains and Extensions, which versions fit this server and which commits were tested together. Includes the exact local build steps, then install the tested release here.", keywords:&["software","catalogue","domain","version","release","tested"], capability:VIEW, input:empty_schema, output:catalogue_output, handler:Handler::Read(&Read::Catalogue), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.releases", title:"Releases available to install", summary:"What this instance can install: the releases its source reports (the one in service is the tested one), the release files uploaded here, whether a pairing key is set, and the releases kept here.", keywords:&["software","release","install","source","download","upload","pairing"], capability:VIEW, input:empty_schema, output:releases_output, handler:Handler::Read(&Read::Releases), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.install", title:"Install a tested release", summary:"Queue an install of a release the source instance tested (release), or of a release file uploaded here (bundle). The review names the source and the change list; the commit asks for the server owner's authenticator code every time.", keywords:&["software","install","release","staging","production","upload"], capability:MANAGE, input:install_schema, output:review_output, handler:Handler::SafeWrite(&Write::Install), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.undo", title:"Undo the last software change", summary:"Queue a switch back to the previous program. The review says whether the backup is restored and what would be lost. The commit asks for the server owner's authenticator code every time.", keywords:&["software","undo","rollback","previous","restore"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Undo), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.go", title:"Go ahead with the pause", summary:"Let a copied install go ahead when it was started with switch ask: the instance then pauses for about a minute to back up, update, switch and check. The commit asks for the server owner's authenticator code every time.", keywords:&["software","go","ahead","switch","pause","install","ask"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Go), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.cancel", title:"Cancel the software job", summary:"Stop the job waiting or running. A waiting job is removed; a running one stops before the pause. Too late once the pause has started.", keywords:&["software","cancel","stop","job"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Cancel), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.pair", title:"Set the pairing key", summary:"Set or clear the key with which this instance downloads releases from its source, and lets the instance that installs from it download. Set the same key on both. The key is never shown back. The commit asks for the server owner's authenticator code every time.", keywords:&["software","pairing","key","release","download","source"], capability:MANAGE, input:pair_schema, output:review_output, handler:Handler::SafeWrite(&Write::Pair), channels:Channels::Every, step_up:&[STEP_UP] },
    ],
    guides: &[Guide { key:"base.software", title:"Discover and install software", summary:"Find official Domains and Extensions, build locally, install tested releases, undo and cancel.", keywords:&["software","domain","extension","build","install","catalogue","undo"], capability:VIEW,
        body:"Start with base.software.status to see what this server runs. Read base.software.catalogue or open the Software card with propose.domains or propose.extensions for what the person asked for. Each entry says what it does, which release accepts the running Bilibi, whether the exact commits were recorded as tested together, and local_steps. Add it in your local Bilibi copy and build it there; then install the tested release here. In that local checkout, clone the official repository into apps/domains/<id> or apps/extensions/<id>, check out the recommended commit, run python3 scripts/discover-modules.py, ./scripts/build-assets.sh --check, cargo metadata --format-version 1 > /dev/null to resolve the mounted Cargo.lock, python3 scripts/discover-modules.py --pin, cargo build --release --locked -p bilibi-web, and ./target/release/bilibi-web status. Run the composition locally and check it in the browser. Use docs/server-install.md to prepare its release file. Servers never build software. For installation read base.software.releases, then plan base.software.install with release (the source's running tested release), or bundle (a file uploaded here). Install and undo back up, switch, check and switch back automatically on failure through the installer. Install, undo, go and pair require the owner's current authenticator code every time. Follow jobs with status. An install with switch ask waits after copying: plan base.software.go to proceed. Cancel is available until the pause starts. Only a server owner can change software, because an install affects every company on this server." }],
}) }

/// How an agent works with the card, for the plugin package's skill.
const CARD_SKILL: &str = "# Software discovery and installation\n\nCall open_software to discover Domains and Extensions, inspect this instance or install a tested release. Pass propose.domains or propose.extensions with the requested catalogue ids; they highlight entries, never request a server build. The card gives a description, compatible version, tested-together evidence and exact local_steps. Add it in your local Bilibi copy and build it there; then install the tested release here. Follow those steps in a local checkout, run the result and check it in the browser. Servers never build. For a tested release, the person reviews and confirms install or undo in the card with their authenticator code. Do not plan or commit those writes yourself while the card is open. Follow the job and explain its outcome. If the host cannot show cards, read base.software.catalogue and status for discovery, then plan install or undo and show the full review before the person confirms. Every install, undo and go-ahead asks for a fresh code on every instance. Only a server owner may install software because it affects every company.\n";

inventory::submit! { Contribution::new(APPS, "base/software", Provenance { module: "base/software", source: file!(), line: line!() }, &App {
    uri: CARD_URI,
    tool: "open_software",
    title: "Bilibi Software",
    description: "Open Software: discover official Domains and Extensions, compatible versions, tested-together evidence and local build steps; see what this server runs; install a tested release or undo with the owner's code. Pass propose.domains or propose.extensions to highlight requested entries. Servers never build software.",
    operation: "base.software.open",
    html: include_str!("../card/software-v1.html"),
    connect_domains: &[],
    resource_domains: &[],
    skill: CARD_SKILL,
}) }
