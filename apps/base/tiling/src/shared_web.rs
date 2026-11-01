//! The `/workspace/shared-layouts` screen: a company's published workspaces and their
//! role audiences.
//!
//! The route mounts under `ui.workspace.publish`. Publishing and deleting are
//! [`Layouts`] writes that take the sealed scope and check that same
//! capability; these handlers only present the outcome. The tree to publish
//! arrives in `?t=` from the workspace's "Share current tiles" link, so the
//! form only renders when there is one; "Delete" opens the confirmation and
//! writes nothing until it is confirmed.
use crate::layouts::{self, Entry, LayoutError, Refusal, RoleOption};
use crate::web::{audience, columns, Core, SharedActions, WORKSPACE};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    lists::EmptyState,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    Button, Choice, Field, FlashMap, FlexRow, FlexTable, Html, ListColumns, MultiSelect,
    StatusKind, TableEmpty, NO_FLASH,
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

const COLUMNS: &[(&str, &str)] = &[("label", "Name"), ("audience", "Audience")];

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    PublishLayout {
        #[serde(rename = "layout[label]")]
        label: String,
        #[serde(rename = "layout[tree]", default)]
        tree: String,
        #[serde(rename = "layout[roles][]", default)]
        roles: Vec<String>,
    },
    DeleteLayout {
        slug: String,
    },
    CancelDeleteLayout,
    ConfirmDeleteLayout,
    Grid {
        #[serde(flatten)]
        params: HashMap<String, String>,
    },
    #[serde(rename = "clear-flash")]
    ClearFlash {
        key: String,
    },
}

pub(crate) struct SharedPage {
    /// The encoded tree to publish, when the address carries a readable one.
    tree: String,
    label: String,
    roles: Vec<String>,
    entries: Vec<Entry>,
    role_options: Vec<RoleOption>,
    columns: ListColumns,
    pending_delete: Option<String>,
    flash: Option<(StatusKind, String)>,
}

impl SharedPage {
    pub(crate) async fn load(
        core: &Core,
        scope: &Scope,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        let tree = query
            .get("t")
            .filter(|tree| layouts::clean_tree(tree).is_ok())
            .cloned()
            .unwrap_or_default();
        let client = core.db.get().await?;
        let entries = core.layouts.shared(&**client, scope).await?;
        let role_options = layouts::role_options(&**client, scope).await?;
        Ok(Self {
            tree,
            label: String::new(),
            roles: Vec::new(),
            entries,
            role_options,
            columns: columns("shared-workspaces", COLUMNS)?,
            pending_delete: None,
            flash: None,
        })
    }

    async fn reload(&mut self, core: &Core, scope: &Scope) -> Result<()> {
        let client = core.db.get().await?;
        self.entries = core.layouts.shared(&**client, scope).await?;
        self.role_options = layouts::role_options(&**client, scope).await?;
        Ok(())
    }

    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }

    async fn event(&mut self, core: &Core, scope: &Scope, event: Event) -> Result<()> {
        match event {
            Event::PublishLayout { label, tree, roles } => {
                let roles: Vec<String> = roles.into_iter().filter(|r| !r.is_empty()).collect();
                self.label = label.clone();
                self.roles = roles.clone();
                let known = |code: &String| self.role_options.iter().any(|r| r.code == *code);
                if !roles.iter().all(known) {
                    self.flash(StatusKind::Error, "Choose role codes from this company.");
                    return Ok(());
                }
                // The tree the form carries is the one the address handed over;
                // a forged one is still only a layout the actor may publish.
                let tree = if tree.is_empty() {
                    self.tree.clone()
                } else {
                    tree
                };
                match core.layouts.publish(scope, &label, &tree, &roles).await {
                    Ok(_) => {
                        self.reload(core, scope).await?;
                        self.flash(StatusKind::Success, "Shared the workspace.");
                        self.label.clear();
                        self.roles.clear();
                    }
                    Err(LayoutError::Storage(Error::Unauthorized)) => {
                        return Err(Error::Unauthorized)
                    }
                    Err(LayoutError::Refused(Refusal::Forbidden)) => {
                        self.flash(StatusKind::Error, "You cannot share workspaces.");
                    }
                    Err(LayoutError::Refused(Refusal::Label)) => {
                        self.flash(StatusKind::Error, "A layout name is one to 60 characters.");
                    }
                    Err(LayoutError::Refused(Refusal::Tree)) => {
                        self.flash(
                            StatusKind::Error,
                            "Open at least one page before sharing a workspace.",
                        );
                    }
                    Err(_) => self.flash(StatusKind::Error, "The workspace could not be shared."),
                }
            }
            Event::DeleteLayout { slug } => {
                if self.entries.iter().any(|entry| entry.slug == slug) {
                    self.pending_delete = Some(slug);
                }
            }
            Event::CancelDeleteLayout => self.pending_delete = None,
            Event::ConfirmDeleteLayout => {
                let Some(slug) = self.pending_delete.take() else {
                    self.flash(StatusKind::Error, "You cannot manage shared workspaces.");
                    return Ok(());
                };
                match core.layouts.unpublish(scope, &slug).await {
                    Ok(()) => self.reload(core, scope).await?,
                    Err(LayoutError::Storage(Error::Unauthorized)) => {
                        return Err(Error::Unauthorized)
                    }
                    Err(LayoutError::Refused(Refusal::Forbidden)) => {
                        self.flash(StatusKind::Error, "You cannot manage shared workspaces.");
                    }
                    Err(_) => {
                        self.flash(
                            StatusKind::Error,
                            "The shared workspace could not be deleted.",
                        );
                    }
                }
            }
            Event::Grid { params } => {
                let _ = self.columns.handle(&params);
            }
            Event::ClearFlash { key } => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| kind.name() == key)
                {
                    self.flash = None;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn render(&self) -> Result<String> {
        let internal = |_| Error::Internal;
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        let label = Field::in_form("layout", "label")
            .label("Name")
            .value(self.label.clone())
            .required();
        let roles = MultiSelect::in_form("layout", "roles")
            .id("shared-workspace-roles")
            .label("Limit to roles")
            .placeholder("Everyone in this company")
            .selection_label(":count role selected|:count roles selected")
            .options(
                self.role_options
                    .iter()
                    .map(|role| Choice::from((role.name.clone(), role.code.clone())))
                    .collect(),
            )
            .values(self.roles.clone());
        let mut rows = Vec::new();
        for entry in &self.entries {
            let slug = &entry.slug;
            let mut row = FlexRow::new(slug.clone());
            row.dom_id = Some(format!("shared-workspace-{slug}"));
            row.slots.insert("label".into(), Html::text(&entry.label));
            row.slots
                .insert("audience".into(), Html::text(&audience(entry)));
            row.actions = Html::render(&SharedActions {
                open_id: &format!("shared-workspace-open-{slug}"),
                open_href: &format!("{WORKSPACE}/shared/{slug}"),
                action: Html::render(&Button {
                    id: &format!("shared-workspace-delete-{slug}"),
                    event: "delete_layout",
                    name: "slug",
                    value: slug,
                    body: Html::text("Delete"),
                    ..Button::default()
                })
                .map_err(internal)?,
            })
            .map_err(internal)?;
            rows.push(row);
        }
        let mut table = FlexTable::new("shared-workspaces", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Shared workspaces";
        table.actions = true;
        if self.entries.is_empty() {
            table.empty = Some(TableEmpty::State(EmptyState::missing(
                "No shared workspaces",
                "Publish a workspace to make it available to this company.",
            )));
        }
        SharedView {
            flash,
            tree: &self.tree,
            label,
            roles,
            table: Html::render(&table).map_err(internal)?,
            pending_delete: self.pending_delete.is_some(),
        }
        .render()
        .map_err(internal)
    }
}

#[derive(Template)]
#[template(path = "shared_layouts.html")]
struct SharedView<'a> {
    flash: FlashMap<'a>,
    tree: &'a str,
    label: Field,
    roles: MultiSelect,
    table: Html,
    pending_delete: bool,
}

pub(crate) struct Mounted {
    core: Arc<Core>,
    page: SharedPage,
}
impl Mounted {
    pub(crate) fn new(core: Arc<Core>, page: SharedPage) -> Self {
        Self { core, page }
    }
}

impl LiveState<Scope, Error> for Mounted {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let before = self.page.flash.clone();
            match request.input {
                LiveInput::Refresh => self.page.reload(&self.core, request.scope).await?,
                LiveInput::Params(values) => {
                    let flash = self.page.flash.take();
                    let columns = std::mem::replace(
                        &mut self.page.columns,
                        columns("shared-workspaces", COLUMNS)?,
                    );
                    self.page = SharedPage::load(&self.core, request.scope, values).await?;
                    self.page.flash = flash;
                    self.page.columns = columns;
                }
                LiveInput::Event(value) => {
                    let event: Event =
                        serde_json::from_value(value.clone()).map_err(|_| Error::Invalid)?;
                    self.page.event(&self.core, request.scope, event).await?;
                }
            }
            let notice = match &self.page.flash {
                Some((_, message)) if self.page.flash != before => message.clone(),
                _ => String::new(),
            };
            let mut query = HashMap::new();
            if !self.page.tree.is_empty() {
                query.insert("t".to_owned(), self.page.tree.clone());
            }
            Ok(LiveBody {
                html: self.page.render()?,
                query,
                notice,
                rows: self.page.entries.len(),
                ..LiveBody::default()
            })
        })
    }
}
