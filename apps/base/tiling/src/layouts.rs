//! The layouts an account saves, the one its workspace opens with, and the
//! layouts a company publishes (Bilimbi `Tiling.SavedLayouts` and
//! `Tiling.SharedLayouts`).
//!
//! All three live in Settings rows, the way the dashboard keeps its widget
//! order, so every write is audited like any other: `ui.workspace.layouts` is
//! the account's list and `ui.workspace.default` names one of them by slug,
//! both at user scope; `ui.workspace.shared_layouts` is the company's list. A
//! layout is a plain JSON object, `{"slug","label","layout","tree"}`, with
//! `roles` on a shared one; `tree` is [`Layout::encode`] output kept as
//! written, so a layout that points at a page the account can no longer open
//! is still listed and the workspace decides what to show in that tile. The
//! slug is derived from the label once and never changes, so a link to
//! `/workspace/<slug>` survives a rename.
//!
//! Publishing and deleting a shared layout check `ui.workspace.publish`, the
//! capability the shared-workspaces screen already requires; the screen only
//! presents that result. An empty role list shares with the whole company;
//! role codes limit who sees a layout, and a viewer may open or copy one while
//! every tile still applies that viewer's own route access.
use crate::layout::Layout;
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{
    Definition, ScopeKind, Setting, Settings, Target, Validator, ValueType, DEFINITIONS,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc};
use tokio_postgres::GenericClient;

pub const LAYOUTS: &str = "ui.workspace.layouts";
pub const DEFAULT: &str = "ui.workspace.default";
pub const SHARED: &str = "ui.workspace.shared_layouts";
/// The capability that publishes, deletes and manages a company's shared
/// layouts; the route of the shared-workspaces screen requires it too.
pub const PUBLISH: &str = "ui.workspace.publish";
pub const MAX_LABEL: usize = 60;
const MAX_ENTRIES: usize = 100;
const MAX_TREE: usize = 2048;
/// Slugs the workspace's own routes take, so a saved layout never shadows them.
const RESERVED_SLUGS: &[&str] = &["shared-layouts", "shared"];

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/tiling",
            source: file!(),
            line: line!(),
        }
    };
}

/// The tiling mode a layout was saved in; the tree and the mode are saved
/// together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Dwindle,
    Master,
}
impl Mode {
    pub fn parse(mode: &str) -> Option<Self> {
        match mode {
            "dwindle" => Some(Self::Dwindle),
            "master" => Some(Self::Master),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dwindle => "dwindle",
            Self::Master => "master",
        }
    }
    pub fn other(self) -> Self {
        match self {
            Self::Dwindle => Self::Master,
            Self::Master => Self::Dwindle,
        }
    }
}

/// One saved or shared layout as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub slug: String,
    pub label: String,
    pub layout: Mode,
    pub tree: String,
    /// Role codes a shared layout is limited to; empty is the whole company.
    pub roles: Vec<String>,
}
impl Entry {
    fn read(value: &Value) -> Option<Self> {
        let slug = value.get("slug")?.as_str()?;
        let label = value.get("label")?.as_str()?;
        let tree = value.get("tree")?.as_str()?;
        if !valid_slug(slug) || clean_label(label).is_err() || clean_tree(tree).is_err() {
            return None;
        }
        let layout = match value.get("layout") {
            None => Mode::Dwindle,
            Some(mode) => Mode::parse(mode.as_str()?)?,
        };
        let roles = match value.get("roles") {
            None => Vec::new(),
            Some(roles) => roles
                .as_array()?
                .iter()
                .map(|role| role.as_str().filter(|code| valid_role(code)))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };
        Some(Self {
            slug: slug.to_owned(),
            label: label.to_owned(),
            layout,
            tree: tree.to_owned(),
            roles,
        })
    }
    fn write(&self, shared: bool) -> Value {
        let mut value = json!({
            "slug": self.slug,
            "label": self.label,
            "layout": self.layout.as_str(),
            "tree": self.tree,
        });
        if shared {
            value["roles"] = json!(self.roles);
        }
        value
    }
}

/// Why a layout write was refused; a refusal is the person's to read, never a
/// fatal error of the page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The label is empty or over 60 bytes once whitespace is collapsed.
    Label,
    /// The tree does not decode to at least one tile.
    Tree,
    /// No saved layout has this slug.
    NotFound,
    /// A role code is not lower-case letters, digits and underscores.
    Roles,
    /// The actor may not publish or delete shared layouts.
    Forbidden,
}

#[derive(Debug)]
pub enum LayoutError {
    Refused(Refusal),
    Storage(Error),
}
impl From<Error> for LayoutError {
    fn from(error: Error) -> Self {
        // A write the storage refused for its own reasons (a company target the
        // proof provider does not know) reads as forbidden, not as a crash.
        match error {
            Error::Forbidden => Self::Refused(Refusal::Forbidden),
            other => Self::Storage(other),
        }
    }
}
impl From<Refusal> for LayoutError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}
pub type Outcome<T> = std::result::Result<T, LayoutError>;

/// The account's saved layouts and its default, read together.
#[derive(Clone, Debug, Default)]
pub struct Saved {
    pub entries: Vec<Entry>,
    /// The slug `/workspace` opens, only when a saved layout still has it.
    pub default: Option<String>,
}
impl Saved {
    pub fn fetch(&self, slug: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.slug == slug)
    }
    pub fn label<'a>(&'a self, slug: &'a str) -> &'a str {
        self.fetch(slug).map_or(slug, |entry| entry.label.as_str())
    }
}

/// A role an audience can be limited to: its display name and the code
/// stored in a shared layout.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleOption {
    pub name: String,
    pub code: String,
}

pub struct Layouts {
    db: Db,
    settings: Arc<Settings>,
    boundary: Arc<MutationBoundary>,
}

impl Layouts {
    pub fn new(db: Db, settings: Arc<Settings>, boundary: Arc<MutationBoundary>) -> Self {
        Self {
            db,
            settings,
            boundary,
        }
    }

    // ---- The account's own layouts ------------------------------------

    pub async fn saved(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Saved> {
        let mut values = self
            .settings
            .resolve_many(client, scope, USER, &[LAYOUTS, DEFAULT])
            .await?;
        let entries = entries(&values.remove(LAYOUTS).ok_or(Error::Internal)?.value);
        let default = values
            .remove(DEFAULT)
            .ok_or(Error::Internal)?
            .value
            .as_str()
            .filter(|slug| entries.iter().any(|entry| entry.slug == *slug))
            .map(str::to_owned);
        Ok(Saved { entries, default })
    }

    /// Saves `tree` under `label`. A label already in use replaces that
    /// layout's tree and keeps its slug; a new label gets a slug of its own.
    /// Without a mode, a replacement keeps its mode and a new layout starts in
    /// dwindle mode.
    pub async fn save(
        &self,
        scope: &Scope,
        label: &str,
        tree: &str,
        mode: Option<Mode>,
    ) -> Outcome<Entry> {
        let label = clean_label(label)?;
        let tree = clean_tree(tree)?;
        let mut entries = self
            .saved(&**self.db.get().await.map_err(Error::from)?, scope)
            .await?
            .entries;
        let entry = match entries.iter_mut().find(|entry| entry.label == label) {
            Some(entry) => {
                entry.tree = tree;
                if let Some(mode) = mode {
                    entry.layout = mode;
                }
                entry.clone()
            }
            None => {
                let entry = Entry {
                    slug: unique_slug(&slug(&label), &entries),
                    label,
                    layout: mode.unwrap_or(Mode::Dwindle),
                    tree,
                    roles: Vec::new(),
                };
                entries.push(entry.clone());
                entry
            }
        };
        self.put_saved(scope, &entries).await?;
        Ok(entry)
    }

    /// Gives the layout with `slug` a new label; the slug stays.
    pub async fn rename(&self, scope: &Scope, slug: &str, label: &str) -> Outcome<Entry> {
        let label = clean_label(label)?;
        let mut entries = self
            .saved(&**self.db.get().await.map_err(Error::from)?, scope)
            .await?
            .entries;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.slug == slug)
            .ok_or(Refusal::NotFound)?;
        entry.label = label;
        let entry = entry.clone();
        self.put_saved(scope, &entries).await?;
        Ok(entry)
    }

    /// Sets the tiling mode and its converted tree for one saved layout.
    pub async fn set_layout(
        &self,
        scope: &Scope,
        slug: &str,
        mode: Mode,
        tree: &str,
    ) -> Outcome<Entry> {
        let tree = clean_tree(tree)?;
        let mut entries = self
            .saved(&**self.db.get().await.map_err(Error::from)?, scope)
            .await?
            .entries;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.slug == slug)
            .ok_or(Refusal::NotFound)?;
        entry.layout = mode;
        entry.tree = tree;
        let entry = entry.clone();
        self.put_saved(scope, &entries).await?;
        Ok(entry)
    }

    /// Forgets the layout with `slug`, and the default if it named it.
    pub async fn delete(&self, scope: &Scope, slug: &str) -> Outcome<()> {
        let saved = self
            .saved(&**self.db.get().await.map_err(Error::from)?, scope)
            .await?;
        let entries: Vec<_> = saved
            .entries
            .into_iter()
            .filter(|entry| entry.slug != slug)
            .collect();
        self.put_saved(scope, &entries).await?;
        if saved.default.as_deref() == Some(slug) {
            self.set_default(scope, None).await?;
        }
        Ok(())
    }

    /// Makes the layout with `slug` the one `/workspace` opens; `None` clears
    /// it. Clearing removes the override: an absent row already reads as the
    /// definition's default.
    pub async fn set_default(&self, scope: &Scope, slug: Option<&str>) -> Outcome<()> {
        match slug {
            None => self.write(scope, USER, DEFAULT, None).await?,
            Some(slug) => {
                let saved = self
                    .saved(&**self.db.get().await.map_err(Error::from)?, scope)
                    .await?;
                if saved.fetch(slug).is_none() {
                    return Err(Refusal::NotFound.into());
                }
                self.write(scope, USER, DEFAULT, Some(json!(slug))).await?;
            }
        }
        Ok(())
    }

    async fn put_saved(&self, scope: &Scope, entries: &[Entry]) -> Result<()> {
        let value: Vec<_> = entries.iter().map(|entry| entry.write(false)).collect();
        self.write(scope, USER, LAYOUTS, Some(Value::Array(value)))
            .await
    }

    // ---- The company's shared layouts ---------------------------------

    /// Every layout published for the actor's company.
    pub async fn shared(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<Entry>> {
        let value = self
            .settings
            .get(client, scope, shared_target(scope), SHARED)
            .await?
            .value;
        Ok(entries(&value))
    }

    /// The shared layouts the actor may see: those for the whole company and
    /// those limited to a role the actor holds.
    pub async fn shared_visible(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<Entry>> {
        let held = role_codes(client, scope).await?;
        Ok(self
            .shared(client, scope)
            .await?
            .into_iter()
            .filter(|entry| {
                entry.roles.is_empty() || entry.roles.iter().any(|code| held.contains(code))
            })
            .collect())
    }

    /// Publishes `tree` under `label` for the company, limited to `roles` (none
    /// is everyone). A label already published replaces that layout's tree and
    /// audience and keeps its slug.
    pub async fn publish(
        &self,
        scope: &Scope,
        label: &str,
        tree: &str,
        roles: &[String],
    ) -> Outcome<Entry> {
        let label = clean_label(label)?;
        let tree = clean_tree(tree)?;
        if !roles.iter().all(|code| valid_role(code)) {
            return Err(Refusal::Roles.into());
        }
        let roles: Vec<String> = roles
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .cloned()
            .collect();
        let client = self.db.get().await.map_err(Error::from)?;
        self.authorize(&**client, scope).await?;
        let mut entries = self.shared(&**client, scope).await?;
        drop(client);
        let entry = match entries.iter_mut().find(|entry| entry.label == label) {
            Some(entry) => {
                entry.tree = tree;
                entry.roles = roles;
                entry.clone()
            }
            None => {
                let entry = Entry {
                    slug: unique_slug(&slug(&label), &entries),
                    label,
                    layout: Mode::Dwindle,
                    tree,
                    roles,
                };
                entries.push(entry.clone());
                entry
            }
        };
        self.put_shared(scope, &entries).await?;
        Ok(entry)
    }

    /// Unpublishes the shared layout with `slug` for everyone in the company.
    pub async fn unpublish(&self, scope: &Scope, slug: &str) -> Outcome<()> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.authorize(&**client, scope).await?;
        let entries: Vec<_> = self
            .shared(&**client, scope)
            .await?
            .into_iter()
            .filter(|entry| entry.slug != slug)
            .collect();
        drop(client);
        self.put_shared(scope, &entries).await?;
        Ok(())
    }

    // The screen's route capability is presentation. This is the write: the
    // same `ui.workspace.publish` key, asked of the scope the edge sealed.
    async fn authorize(&self, client: &impl GenericClient, scope: &Scope) -> Outcome<()> {
        match bilibi_authz::authorize_key(client, scope, PUBLISH).await {
            Ok(()) => Ok(()),
            Err(Error::Forbidden) => Err(Refusal::Forbidden.into()),
            Err(error) => Err(LayoutError::Storage(error)),
        }
    }

    async fn put_shared(&self, scope: &Scope, entries: &[Entry]) -> Result<()> {
        let value: Vec<_> = entries.iter().map(|entry| entry.write(true)).collect();
        self.write(
            scope,
            shared_target(scope),
            SHARED,
            Some(Value::Array(value)),
        )
        .await
    }

    /// A self-service write to the account's own preference, or a company
    /// write the caller has authorized; audited like any other mutation.
    async fn write(
        &self,
        scope: &Scope,
        target: Target,
        key: &str,
        value: Option<Value>,
    ) -> Result<()> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("tiling.layouts"))
            .await?;
        match value {
            Some(value) => {
                self.settings
                    .put(&mut tx, scope, target, key, value)
                    .await?
            }
            None => self.settings.delete(&mut tx, scope, target, key).await?,
        }
        tx.commit().await
    }
}

const USER: Target = Target::User { company: None };

/// Where the company's shared layouts live. The Core identity binder proves
/// an account's organisation from its affiliation; an unlinked account has
/// none, and the tenant stands in for the company so publishing stays usable.
/// Remove the fallback when every account carries its organisation.
fn shared_target(scope: &Scope) -> Target {
    match scope.actor().organisation() {
        Some(organisation) => Target::Company(organisation),
        None => Target::Tenant,
    }
}

/// The role codes the actor holds, which decide the shared layouts they see.
/// A role without a code (a tenant-defined role) is known by its name when that is a
/// valid code.
pub async fn role_codes(client: &impl GenericClient, scope: &Scope) -> Result<Vec<String>> {
    Ok(client
        .query(
            "SELECT COALESCE(r.code,r.name) FROM effective_principal_roles p JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2",
            &[&scope.tenant(), &scope.user()],
        )
        .await?
        .iter()
        .map(|row| row.get::<_, String>(0))
        .filter(|code| valid_role(code))
        .collect())
}

/// The roles a published layout may be limited to, by name.
pub async fn role_options(client: &impl GenericClient, scope: &Scope) -> Result<Vec<RoleOption>> {
    Ok(client
        .query(
            "SELECT r.name,COALESCE(r.code,r.name) FROM roles r WHERE r.tenant_id=$1 ORDER BY lower(r.name),r.id",
            &[&scope.tenant()],
        )
        .await?
        .iter()
        .map(|row| RoleOption {
            name: row.get(0),
            code: row.get(1),
        })
        .filter(|role| valid_role(&role.code))
        .collect())
}

fn entries(value: &Value) -> Vec<Entry> {
    value
        .as_array()
        .map(|entries| entries.iter().filter_map(Entry::read).collect())
        .unwrap_or_default()
}

/// The URL slug for a label: lower-case words joined by hyphens.
pub fn slug(label: &str) -> String {
    let mut slug = String::new();
    for character in label.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "layout".to_owned()
    } else {
        slug.to_owned()
    }
}

/// The first of `base`, `base-2`, `base-3`, ... that no entry and no workspace
/// route uses.
pub fn unique_slug(base: &str, entries: &[Entry]) -> String {
    let taken = |candidate: &str| {
        RESERVED_SLUGS.contains(&candidate) || entries.iter().any(|entry| entry.slug == candidate)
    };
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken(candidate))
        .expect("an unused numbered slug")
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 90
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A label with whitespace collapsed, when it is one to 60 bytes.
pub fn clean_label(label: &str) -> std::result::Result<String, Refusal> {
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() || label.len() > MAX_LABEL {
        return Err(Refusal::Label);
    }
    Ok(label)
}

/// The encoded tree, when it decodes to at least one tile.
pub fn clean_tree(tree: &str) -> std::result::Result<String, Refusal> {
    match Layout::decode(tree) {
        Some(layout) if !layout.is_empty() && tree.len() <= MAX_TREE => Ok(tree.to_owned()),
        _ => Err(Refusal::Tree),
    }
}

pub fn valid_role(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 100
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

// ---- Contributions -----------------------------------------------------

/// A stored list is a bounded array of complete entries, each decoding to at
/// least one tile; an entry Settings would refuse never reaches storage.
fn valid_entries(value: &Value) -> bool {
    value.as_array().is_some_and(|entries| {
        entries.len() <= MAX_ENTRIES
            && entries.iter().all(|entry| Entry::read(entry).is_some())
            && entries
                .iter()
                .filter_map(|entry| entry.get("slug").and_then(Value::as_str))
                .collect::<BTreeSet<_>>()
                .len()
                == entries.len()
    })
}

/// Empty is the declared default: no layout opens with the workspace.
fn valid_default(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|slug| slug.is_empty() || valid_slug(slug))
}

inventory::submit! { Contribution::new(DEFINITIONS, LAYOUTS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: &[ScopeKind::User], default: || json!([]),
    nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: valid_entries, message: "must be a bounded list of saved workspace layouts with unique slugs" }),
    editor: None,
})) }
inventory::submit! { Contribution::new(DEFINITIONS, DEFAULT, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::User], default: || json!(""),
    nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: valid_default, message: "must be the slug of a saved workspace layout" }),
    editor: None,
})) }
// Company scope is the home; tenant scope stands in for an account without a
// proven organisation (see `shared_target`). Shared layouts never inherit
// from a wider scope than the one written.
inventory::submit! { Contribution::new(DEFINITIONS, SHARED, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: &[ScopeKind::Company, ScopeKind::Tenant], default: || json!([]),
    nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: valid_entries, message: "must be a bounded list of shared workspace layouts with unique slugs and role codes" }),
    editor: None,
})) }
inventory::submit! { Contribution::new(bilibi_authz::VOCABULARY, "base/tiling", provenance!(), &bilibi_authz::VocabularyContribution {
    domains: &["ui"],
    verbs: &["publish"],
    capabilities: &[bilibi_authz::CapabilityDefinition::tenant(PUBLISH)],
    roles: &[],
    system_principals: &[],
}) }
// The workspace itself has no menu entry; the sidebar's tile control opens it.
// Only the shared-layouts screen is listed, under System.
bilibi_menu::contribute!(
    "base/tiling",
    bilibi_menu::MenuItem::page(
        "admin.system.workspace.shared",
        "Shared workspaces",
        "/workspace/shared-layouts"
    )
    .under("admin.system")
    .icon("squares-2x2")
    .requires(bilibi_menu::Requirement::Capability(PUBLISH))
    .order(50)
);
