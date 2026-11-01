//! Drop targets: where a record may be dropped as a shortcut for an action
//! the target page already offers (issue 265). A page declares what it
//! accepts and which operation contract the drop's review commits, with one
//! typed contribution; `Records` answers the rest for every caller: which
//! targets a person may use for a kind of record (the "Add to…" menu and the
//! highlighted tiles), and the records a drop or a link carried, re-read and
//! re-checked under the reader's own scope. A drop carries references only
//! (`kind:id`), never field values, and grants no authority: the target opens
//! its own review, and the contract it commits decides again.
use crate::{
    declaration::{key_shaped, KINDS},
    records::{Records, Reference, Resolved},
    RecordKind,
};
use bilibi_module_registry::{
    contributions::{Method, PageWrites, Route, PAGE_WRITES, ROUTES},
    Catalog, Consumer, Entry, RegistryError, Result as RegistryResult,
};
use bilibi_tenancy::Scope;
use std::collections::BTreeSet;

pub const DROP_TARGETS: &str = "base/records.drop_targets";
/// The live event a target page receives for a drop: `{type: "drop_records",
/// records: "kind:id,kind:id"}`.
pub const DROP_EVENT: &str = "drop_records";
/// The query parameter the "Add to…" link carries to the target page, in the
/// same spelling as the event's `records`.
pub const DROP_PARAM: &str = "drop";
/// The most records one drop may name, whatever a target declares.
pub const MAX_DROP: usize = 100;

/// One place a record may be dropped. Declared by the page's owner with
/// [`drop_target!`](crate::drop_target).
pub struct DropTarget {
    /// A stable key, lower case with dots: `customer_access.invite`.
    pub id: &'static str,
    /// The page service that receives the drop event; it must serve `path`.
    pub service: &'static str,
    /// The operation contract the drop's review commits, one the service
    /// already declares in its `PageWrites`. A drop adds no write of its own.
    pub contract: &'static str,
    /// The capability the contract needs, so only people who can finish the
    /// review are offered the target. Display only: the contract decides.
    pub capability: &'static str,
    /// The target page, a contributed GET page route without parameters.
    pub path: &'static str,
    /// What the shortcut does, as the "Add to…" menu and the highlighted
    /// tile say it: "Invite as customer".
    pub label: &'static str,
    /// The kinds of record the target accepts.
    pub accepts: &'static [&'static str],
    /// The most records one drop may carry (1 to [`MAX_DROP`]). A target
    /// whose review takes one record at a time says 1.
    pub max: usize,
}

impl DropTarget {
    /// The page's drop zone (`DropZone::wrap`), from this declaration, so
    /// the page repeats neither the kinds, the count nor the label.
    pub fn zone(&self) -> bilibi_ui::DropZone {
        bilibi_ui::DropZone::new(self.accepts, self.max, self.label)
    }
}

/// Declares one drop target from module `$module`:
/// `bilibi_records::drop_target!("core/customer_access", DropTarget { … });`
#[macro_export]
macro_rules! drop_target {
    ($module:literal, $target:expr) => {
        $crate::inventory::submit! {{
            const TARGET: $crate::DropTarget = $target;
            $crate::Contribution::new(
                $crate::DROP_TARGETS,
                TARGET.id,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                &TARGET,
            )
        }}
    };
}

/// A target offered to one person for one kind of record: what the
/// "Add to…" menu lists. `path` is the page; `drop_href` adds the records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddTo {
    pub id: &'static str,
    pub label: &'static str,
    pub path: &'static str,
    /// The most records one drop carries.
    pub max: usize,
}
impl AddTo {
    /// The target page with `records` (`kind:id`) for its review to open on.
    pub fn href(&self, records: &[Reference]) -> String {
        format!(
            "{}?{DROP_PARAM}={}",
            self.path,
            records
                .iter()
                .map(|record| format!("{}:{}", record.kind, record.id))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

/// Why a drop is refused. The target shows `message` as its own notice and
/// opens no review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropRefusal {
    /// The drop names no record, or one that is not `kind:id`.
    Malformed,
    /// More records than the target takes at once.
    TooMany(usize),
    /// A record of a kind the target does not accept.
    WrongKind,
    /// A record the reader cannot see, or that no longer exists.
    Unavailable,
}
impl DropRefusal {
    pub fn message(self) -> String {
        match self {
            Self::Malformed => "That drop did not name a record.".into(),
            Self::TooMany(1) => "This takes one record at a time.".into(),
            Self::TooMany(max) => format!("This takes at most {max} records at a time."),
            Self::WrongKind => "This does not take that kind of record.".into(),
            Self::Unavailable => "A dropped record is not one you may see, or was removed.".into(),
        }
    }
}

/// The records of a drop's text, `kind:id` joined by commas. Ids that could
/// not survive that spelling are refused, so a forged package cannot smuggle
/// a second record into one.
fn parse(text: &str) -> Result<Vec<Reference>, DropRefusal> {
    let mut references: Vec<Reference> = Vec::new();
    for part in text.split(',').filter(|part| !part.trim().is_empty()) {
        let (kind, id) = part.trim().split_once(':').ok_or(DropRefusal::Malformed)?;
        if !key_shaped(kind) || id.contains(':') || !bilibi_ui::workspace::record_id(id) {
            return Err(DropRefusal::Malformed);
        }
        let reference = Reference::new(kind, id);
        if !references.contains(&reference) {
            references.push(reference);
        }
        if references.len() > MAX_DROP {
            return Err(DropRefusal::TooMany(MAX_DROP));
        }
    }
    if references.is_empty() {
        return Err(DropRefusal::Malformed);
    }
    Ok(references)
}

impl Records {
    /// The declared drop targets.
    pub fn drop_targets(&self) -> &[&'static DropTarget] {
        &self.drop_targets
    }

    pub fn drop_target(&self, id: &str) -> Option<&'static DropTarget> {
        self.drop_targets
            .iter()
            .copied()
            .find(|target| target.id == id)
    }

    /// The targets this person may use for a record of `kind`, in label
    /// order: those whose contract capability they hold. Display only (an
    /// unlogged read); the target page and its contract decide again. Empty
    /// when nothing accepts the kind, so a list then offers neither a drag
    /// handle nor a menu.
    pub async fn add_to(&self, scope: &Scope, kind: &str) -> bilibi_database::Result<Vec<AddTo>> {
        let candidates: Vec<&'static DropTarget> = self
            .drop_targets
            .iter()
            .copied()
            .filter(|target| target.accepts.contains(&kind))
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let held = {
            let client = self.db.get().await?;
            bilibi_authz::effective_capabilities(&**client, scope).await?
        };
        let mut offered: Vec<AddTo> = candidates
            .into_iter()
            .filter(|target| held.contains(target.capability))
            .map(|target| AddTo {
                id: target.id,
                label: target.label,
                path: target.path,
                max: target.max,
            })
            .collect();
        offered.sort_by(|a, b| a.label.cmp(b.label).then(a.id.cmp(b.id)));
        Ok(offered)
    }

    /// The records a drop or an "Add to…" link carried (`text` is `kind:id`
    /// joined by commas), for the target page to open its review on. The
    /// package is only a request: the count, the kinds and every record are
    /// checked again here, each record read under `scope` the way a reference
    /// is named (`resolve`), so a record the reader may not see, a removed
    /// one and a forged id are refused alike. The contract the review commits
    /// still decides on its own.
    pub async fn receive(
        &self,
        scope: &Scope,
        target: &str,
        text: &str,
    ) -> bilibi_database::Result<Result<Vec<Resolved>, DropRefusal>> {
        let Some(target) = self.drop_target(target) else {
            return Err(bilibi_database::Error::Internal);
        };
        let references = match parse(text) {
            Ok(references) => references,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if references.len() > target.max {
            return Ok(Err(DropRefusal::TooMany(target.max)));
        }
        if references
            .iter()
            .any(|reference| !target.accepts.contains(&reference.kind.as_str()))
        {
            return Ok(Err(DropRefusal::WrongKind));
        }
        let resolved = self.resolve(scope, &references).await?;
        if resolved.len() != references.len()
            || resolved.iter().any(|record| record.label.is_none())
        {
            return Ok(Err(DropRefusal::Unavailable));
        }
        Ok(Ok(resolved))
    }
}

fn at(entry: &Entry) -> String {
    format!(
        "drop target {} at {}:{}",
        entry.id, entry.provenance.source, entry.provenance.line
    )
}

/// What needs nothing but the declarations. The kinds, the page and its
/// declared contract are checked by [`validate_installed`].
fn validate(entries: &[Entry]) -> RegistryResult<()> {
    let mut ids = BTreeSet::new();
    for entry in entries {
        let target = entry.get::<DropTarget>()?;
        let refuse = |reason: &str| {
            Err(RegistryError::Contribution(format!(
                "{} {reason}",
                at(entry)
            )))
        };
        if !key_shaped(target.id) || entry.id != target.id || !ids.insert(target.id) {
            return refuse("must be keyed by its own id, once, in lower case");
        }
        if !crate::declaration::depends(entry) {
            return refuse("is contributed by a module that does not depend on base/records");
        }
        if target.accepts.is_empty()
            || !target.accepts.iter().all(|kind| key_shaped(kind))
            || target.accepts.iter().collect::<BTreeSet<_>>().len() != target.accepts.len()
        {
            return refuse("must accept at least one kind of record, each once");
        }
        if target.max == 0 || target.max > MAX_DROP {
            return refuse("must take between 1 and 100 records at once");
        }
        if target.label.trim().is_empty() || target.label.chars().count() > 40 {
            return refuse("needs a label of 1 to 40 characters");
        }
        if target.capability.is_empty() || target.contract.is_empty() || target.service.is_empty() {
            return refuse("needs a service, a contract and a capability");
        }
        if !target.path.starts_with('/') || target.path.contains(['{', '?', '#']) {
            return refuse("needs a page path without parameters");
        }
    }
    Ok(())
}

/// What boot proves once the routes and the page writes are known: each
/// accepted kind is a mounted kind (an unmounted module's kind makes the
/// target refuse to boot, as a mounted module's own declaration would), the
/// path is a contributed GET page route of the target's service, and the
/// service declares the contract among its page writes, so a drop can start
/// only a review that commits an existing operation.
pub(crate) fn validate_installed(catalog: &Catalog) -> RegistryResult<()> {
    let routes: Vec<&Route> = catalog
        .entries::<Route>(ROUTES)?
        .iter()
        .map(|entry| entry.get::<Route>())
        .collect::<RegistryResult<_>>()?;
    let kinds: BTreeSet<&str> = catalog
        .entries::<RecordKind>(KINDS)?
        .iter()
        .map(|entry| entry.get::<RecordKind>().map(|kind| kind.key))
        .collect::<RegistryResult<_>>()?;
    let writes: Vec<&PageWrites> = catalog
        .entries::<PageWrites>(PAGE_WRITES)?
        .iter()
        .map(|entry| entry.get::<PageWrites>())
        .collect::<RegistryResult<_>>()?;
    for entry in catalog.entries::<DropTarget>(DROP_TARGETS)? {
        let target = entry.get::<DropTarget>()?;
        let refuse = |reason: String| {
            Err(RegistryError::Contribution(format!(
                "{} {reason}",
                at(entry)
            )))
        };
        if let Some(kind) = target.accepts.iter().find(|kind| !kinds.contains(**kind)) {
            return refuse(format!("accepts {kind}, which no mounted module declares"));
        }
        if !routes.iter().any(|route| {
            route.method == Method::Get
                && route.path == target.path
                && route.service == target.service
        }) {
            return refuse(format!(
                "names page {}, which is no contributed GET page route of {}",
                target.path, target.service
            ));
        }
        if !writes.iter().any(|declared| {
            declared.service == target.service
                && declared
                    .contracts
                    .iter()
                    .any(|(_, key)| *key == target.contract)
        }) {
            return refuse(format!(
                "runs {}, which {} does not declare among its page writes",
                target.contract, target.service
            ));
        }
    }
    Ok(())
}

inventory::submit! { Consumer::new::<DropTarget>(DROP_TARGETS, "base/records", validate) }
