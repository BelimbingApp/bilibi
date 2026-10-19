//! The software contracts, reached through Agent API: three reads and five
//! reviewed writes (plan, then commit). A commit of change, install or undo
//! asks for the server owner's authenticator code every time, on every
//! instance (`STEP_UP`); cancel and pair do not change the software and ask
//! none.
use crate::{bound, SoftwareError, MANAGE, STEP_UP, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
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
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/software"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        _input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let software = bound()?;
            let value = match self {
                Self::Status => software.status(scope).await?,
                Self::Catalogue => software.catalogue(scope).await?,
                Self::Releases => software.releases(scope).await?,
            };
            Ok(value.into())
        })
    }
}

enum Write {
    Change,
    Install,
    Undo,
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
                Self::Change => software.review_change(scope, input).await?,
                Self::Install => software.review_install(scope, input).await?,
                Self::Undo => software.review_undo(scope, input).await?,
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
                Self::Change => software.change_in(tx, scope, input).await?,
                Self::Install => software.install_in(tx, scope, input).await?,
                Self::Undo => software.undo_in(tx, scope, input).await?,
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
fn change_schema() -> Value {
    json!({"type":"object","required":["domains"],"properties":{
        "platform":{"type":"string","maxLength":32,"description":"The Bilibi release to build on, like 0.1.0. Left out: the newest release every chosen Domain accepts."},
        "domains":{"type":"array","description":"Every Domain the server should have after the change; a mounted Domain left out is removed.","items":{"type":"object","required":["id"],"properties":{
            "id":{"type":"string","maxLength":64,"description":"The Domain id from the catalogue, like finance."},
            "version":{"type":"string","maxLength":32,"description":"A release of that Domain, like 0.1.0. Left out: its newest release."},
            "repository":{"type":"string","maxLength":512,"description":"For a Domain outside the catalogue: its https:// Git address."},
            "ref":{"type":"string","maxLength":128,"description":"For a Domain outside the catalogue: the tag or commit to build."}}}},
        "plugins":{"type":"array","items":{"type":"object","required":["id"],"properties":{
            "id":{"type":"string","maxLength":64},
            "enabled":{"type":"boolean"}}}},
        "switch":{"type":"string","enum":["auto","ask"],"description":"auto: put the new program into service as soon as it is built. ask: build now, ask before the one-minute pause."}
    }})
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
fn status_output() -> Value {
    json!({"type":"object","description":"instance (role, label, builds, installs_from), installed (platform, domains, extensions from the running program), plugins, job (the one waiting or running, or the last one, with its steps and whether cancel is available), undo (available, job, restores_backup), jobs (recent)."})
}
fn catalogue_output() -> Value {
    json!({"type":"object","description":"available, url, platform versions, domains with their releases and the Platform versions each was tested on, plugins, tested sets. When not available: message."})
}
fn review_output() -> Value {
    json!({"type":"object","description":"The review: summary, the change list, warnings, the estimate, what happens, the steps, and the step-up operation key (base.software.change). The commit answers the queued job with its id, state and steps."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "base/software", Provenance { module: "base/software", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key:"base.software.status", title:"Software status", summary:"Which instance this is, what is installed, the plug-ins, the job waiting or running with its steps, and whether Undo is available.", keywords:&["software","status","build","install","domain","version","job","undo"], capability:VIEW, input:empty_schema, output:status_output, handler:Handler::Read(&Read::Status), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.catalogue", title:"Software catalogue", summary:"The Domains, releases, plug-ins and tested sets the server can be built from.", keywords:&["software","catalogue","domain","version","release","tested"], capability:VIEW, input:empty_schema, output:catalogue_output, handler:Handler::Read(&Read::Catalogue), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.releases", title:"Releases available to install", summary:"What this instance can install: the releases its source reports (the one in service is the tested one), the release files uploaded here, whether a pairing key is set, and the releases kept here.", keywords:&["software","release","install","source","download","upload","pairing"], capability:VIEW, input:empty_schema, output:releases_output, handler:Handler::Read(&Read::Releases), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.change", title:"Change the software (build on Dev)", summary:"Queue a build of a new program with the chosen Domains and versions. Dev only. The review lists what changes, whether the set was tested together and the estimate; the commit asks for the server owner's authenticator code every time.", keywords:&["software","build","domain","add","remove","update","version"], capability:MANAGE, input:change_schema, output:review_output, handler:Handler::SafeWrite(&Write::Change), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.install", title:"Install a tested release", summary:"Queue an install of a release the source instance tested (release), or of a release file uploaded here (bundle), on Staging or Production. The review names the source and the change list; the commit asks for the server owner's authenticator code every time.", keywords:&["software","install","release","staging","production","upload"], capability:MANAGE, input:install_schema, output:review_output, handler:Handler::SafeWrite(&Write::Install), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.undo", title:"Undo the last software change", summary:"Queue a switch back to the previous program. The review says whether the backup is restored and what would be lost. The commit asks for the server owner's authenticator code every time.", keywords:&["software","undo","rollback","previous","restore"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Undo), channels:Channels::Every, step_up:&[STEP_UP] },
        Operation { key:"base.software.cancel", title:"Cancel the software job", summary:"Stop the job waiting or running. A waiting job is removed; a running one stops before the pause. Too late once the pause has started.", keywords:&["software","cancel","stop","job"], capability:MANAGE, input:job_schema, output:review_output, handler:Handler::SafeWrite(&Write::Cancel), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.software.pair", title:"Set the pairing key", summary:"Set or clear the key with which this instance downloads releases from its source, and lets the instance that installs from it download. Set the same key on both. The key is never shown back.", keywords:&["software","pairing","key","release","download","source"], capability:MANAGE, input:pair_schema, output:review_output, handler:Handler::SafeWrite(&Write::Pair), channels:Channels::Every, step_up:&[] },
    ],
    guides: &[Guide { key:"base.software", title:"Changing the server's software", summary:"How to see what is installed, choose Domains from the catalogue, build on Dev, install a tested release elsewhere, undo and cancel.", keywords:&["software","domain","build","install","catalogue","undo"], capability:VIEW,
        body:"Start with base.software.status: it says which instance this is (Dev builds; Staging and Production install what another instance tested), what is installed, and whether a job is running. Read base.software.catalogue for the Domains and releases. On Dev, plan base.software.change with the full list of Domains the server should have afterwards (a mounted Domain left out is removed); the review shows the change list, whether the versions were tested together, a rough time and the steps. Show it to the person, then commit. On Staging or Production, read base.software.releases first: it lists what the source (the instance named in \"Install releases tested on\") runs now, and the release files uploaded here. Plan base.software.install with that release number, or with bundle for an uploaded file; the review names the source, the change list and the checks. A source on another machine is reached over HTTPS with a pairing key set on both instances (base.software.pair); the target downloads, and nothing is ever pushed to it. Every change, install or undo asks for the server owner's own authenticator code on every commit, on every instance: answer step_up_required by asking the person for the current code and committing the same plan again with step_up_code. The job then waits for the builder or installer; follow it with base.software.status. Cancel is possible until the pause starts. Only a server owner (the platform operator's administrators) may change the software, because one build changes every company on the server." }],
}) }
