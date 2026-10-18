//! The `/dashboard` page and its mounted state (Bilimbi `Dashboard.Web.IndexLive`).
//!
//! The page owns the arrangement and nothing else. Each visible entry is a
//! server-declared panel (`LiveState::panels`) that Web mounts from the entry's
//! discovered embed, so this module names no owner and reads no owner's data.
//! Every update re-derives the arrangement from storage and a freshly guarded
//! scope: an entry whose capability is gone leaves the page (and its panel state
//! is dropped by Web) while its siblings keep running, and returns in its stored
//! place when the capability does.
use crate::{Dashboard, PanelArgs, Placement, Widget, LAYOUT, SECTIONS};
use askama::Template;
use bilibi_authz::authorize_key;
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest, PanelSpec,
    },
    Button, ButtonVariant, FlashMap, Html, StatusKind, NO_FLASH,
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, sync::Arc};

struct Core {
    db: Db,
    dashboard: Arc<Dashboard>,
    settings: Arc<Settings>,
    boundary: Arc<MutationBoundary>,
}
pub struct Pages(Arc<Core>);
impl Pages {
    pub fn new(
        db: Db,
        dashboard: Arc<Dashboard>,
        settings: Arc<Settings>,
        boundary: Arc<MutationBoundary>,
    ) -> Self {
        Self(Arc::new(Core {
            db,
            dashboard,
            settings,
            boundary,
        }))
    }
}

/// What one account sees right now: the arrangement of the entries it may see
/// and the authorized entries it has not placed.
struct Arrangement {
    grid: Vec<&'static Widget>,
    sections: Vec<&'static Widget>,
    spare_grid: Vec<&'static Widget>,
    spare_sections: Vec<&'static Widget>,
    /// The capabilities that withheld every grid entry, for the empty wording.
    withheld: Vec<&'static str>,
    catalogue_empty: bool,
}
impl Core {
    async fn arrangement(&self, scope: &Scope) -> Result<Arrangement> {
        let client = self.db.get().await?;
        let mut decided: HashMap<&str, bool> = HashMap::new();
        for key in self
            .dashboard
            .entries()
            .iter()
            .filter_map(|widget| widget.capability)
        {
            if !decided.contains_key(key) {
                let allowed = match authorize_key(&**client, scope, key).await {
                    Ok(()) => true,
                    Err(Error::Forbidden) => false,
                    Err(error) => return Err(error),
                };
                decided.insert(key, allowed);
            }
        }
        let offered = |widget: &Widget| widget.capability.is_none_or(|key| decided[key]);
        let mut stored = self
            .settings
            .resolve_many(
                &**client,
                scope,
                Target::User { company: None },
                &[LAYOUT, SECTIONS],
            )
            .await?;
        let mut arrange = |key: &str, placement: Placement| -> Result<_> {
            let stored = stored.remove(key).ok_or(Error::Internal)?;
            let available: Vec<_> = self
                .dashboard
                .entries()
                .iter()
                .filter(|widget| widget.placement == placement)
                .filter(|widget| offered(widget))
                .copied()
                .collect();
            // No stored row shows the whole authorized catalogue; a stored one
            // keeps its order and drops ids the account can no longer see.
            let placed: Vec<_> = if stored.overridden {
                let ids = stored.value.as_array().ok_or(Error::Invalid)?;
                ids.iter()
                    .filter_map(|id| available.iter().find(|w| Some(w.id) == id.as_str()))
                    .copied()
                    .collect()
            } else {
                available.clone()
            };
            let spare = available
                .into_iter()
                .filter(|widget| !placed.iter().any(|p| p.id == widget.id))
                .collect();
            Ok((placed, spare))
        };
        let (grid, spare_grid) = arrange(LAYOUT, Placement::Grid)?;
        let (sections, spare_sections) = arrange(SECTIONS, Placement::Section)?;
        let mut withheld: Vec<_> = self
            .dashboard
            .widgets()
            .iter()
            .filter_map(|widget| widget.capability)
            .collect();
        withheld.sort_unstable();
        withheld.dedup();
        Ok(Arrangement {
            grid,
            sections,
            spare_grid,
            spare_sections,
            withheld,
            catalogue_empty: self.dashboard.widgets().is_empty(),
        })
    }

    /// A self-service write to the signed-in account's own preference, so it is
    /// not a privileged administration write.
    async fn persist(&self, scope: &Scope, key: &str, entries: &[&Widget]) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin_view_state(&mut client, scope.mutation_context("dashboard.arrangement"))
            .await?;
        let ids: Vec<_> = entries.iter().map(|widget| widget.id).collect();
        self.settings
            .put(
                &mut tx,
                scope,
                Target::User { company: None },
                key,
                json!(ids),
            )
            .await?;
        tx.commit().await
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    ToggleLayoutEdit,
    AddWidget {
        id: String,
    },
    RemoveWidget {
        id: String,
    },
    MoveUp {
        id: String,
    },
    MoveDown {
        id: String,
    },
    /// The drag hook pushes the DOM order after a drop. The browser is not the
    /// source of truth: an order that is not exactly a permutation of the
    /// current ids is ignored, never persisted.
    ReorderWidgets {
        ids: Vec<String>,
    },
    AddSection {
        id: String,
    },
    RemoveSection {
        id: String,
    },
    MoveSectionUp {
        id: String,
    },
    MoveSectionDown {
        id: String,
    },
    #[serde(rename = "clear-flash")]
    ClearFlash {
        key: String,
    },
}

struct Dash {
    core: Arc<Core>,
    editing: bool,
    flash: Option<(StatusKind, String)>,
    specs: Vec<PanelSpec>,
}
impl Dash {
    fn new(core: &Arc<Core>) -> Self {
        Self {
            core: core.clone(),
            editing: false,
            flash: None,
            specs: Vec::new(),
        }
    }
    async fn event(
        &mut self,
        scope: &Scope,
        arrangement: &mut Arrangement,
        event: Event,
    ) -> Result<()> {
        match event {
            Event::ToggleLayoutEdit => self.editing = !self.editing,
            Event::ClearFlash { key } => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| kind.name() == key)
                {
                    self.flash = None;
                }
            }
            Event::AddWidget { id } => {
                self.add(
                    scope,
                    LAYOUT,
                    &mut arrangement.grid,
                    &arrangement.spare_grid,
                    &id,
                )
                .await?
            }
            Event::AddSection { id } => {
                self.add(
                    scope,
                    SECTIONS,
                    &mut arrangement.sections,
                    &arrangement.spare_sections,
                    &id,
                )
                .await?
            }
            Event::RemoveWidget { id } => {
                self.remove(
                    scope,
                    LAYOUT,
                    &mut arrangement.grid,
                    &id,
                    Some("Widget removed."),
                )
                .await?
            }
            Event::RemoveSection { id } => {
                self.remove(scope, SECTIONS, &mut arrangement.sections, &id, None)
                    .await?
            }
            Event::MoveUp { id } => {
                self.step(scope, LAYOUT, &mut arrangement.grid, &id, -1)
                    .await?
            }
            Event::MoveDown { id } => {
                self.step(scope, LAYOUT, &mut arrangement.grid, &id, 1)
                    .await?
            }
            Event::MoveSectionUp { id } => {
                self.step(scope, SECTIONS, &mut arrangement.sections, &id, -1)
                    .await?
            }
            Event::MoveSectionDown { id } => {
                self.step(scope, SECTIONS, &mut arrangement.sections, &id, 1)
                    .await?
            }
            Event::ReorderWidgets { ids } => {
                let current = &arrangement.grid;
                let mut sorted: Vec<_> = ids.iter().map(String::as_str).collect();
                sorted.sort_unstable();
                sorted.dedup();
                let mut known: Vec<_> = current.iter().map(|w| w.id).collect();
                known.sort_unstable();
                if sorted.len() == ids.len() && sorted == known {
                    let reordered: Vec<_> = ids
                        .iter()
                        .filter_map(|id| current.iter().find(|w| w.id == id))
                        .copied()
                        .collect();
                    if reordered
                        .iter()
                        .map(|w| w.id)
                        .ne(current.iter().map(|w| w.id))
                    {
                        self.core.persist(scope, LAYOUT, &reordered).await?;
                        arrangement.grid = reordered;
                    }
                }
            }
        }
        Ok(())
    }
    async fn add(
        &mut self,
        scope: &Scope,
        key: &str,
        placed: &mut Vec<&'static Widget>,
        spare: &[&'static Widget],
        id: &str,
    ) -> Result<()> {
        // A stale or forged id changes nothing.
        let Some(&widget) = spare.iter().find(|w| w.id == id) else {
            return Ok(());
        };
        let mut next = placed.clone();
        next.push(widget);
        self.core.persist(scope, key, &next).await?;
        *placed = next;
        self.flash = Some((
            StatusKind::Success,
            format!("{} added to dashboard.", widget.label),
        ));
        Ok(())
    }
    async fn remove(
        &mut self,
        scope: &Scope,
        key: &str,
        placed: &mut Vec<&'static Widget>,
        id: &str,
        message: Option<&str>,
    ) -> Result<()> {
        let Some(at) = placed.iter().position(|w| w.id == id) else {
            return Ok(());
        };
        let mut next = placed.clone();
        let widget = next.remove(at);
        self.core.persist(scope, key, &next).await?;
        *placed = next;
        self.flash = Some((
            StatusKind::Success,
            message.map_or_else(
                || format!("{} removed from dashboard.", widget.label),
                str::to_owned,
            ),
        ));
        Ok(())
    }
    async fn step(
        &mut self,
        scope: &Scope,
        key: &str,
        placed: &mut Vec<&'static Widget>,
        id: &str,
        direction: isize,
    ) -> Result<()> {
        let Some(at) = placed.iter().position(|w| w.id == id) else {
            return Ok(());
        };
        let Some(to) = at
            .checked_add_signed(direction)
            .filter(|to| *to < placed.len())
        else {
            return Ok(());
        };
        let mut next = placed.clone();
        next.swap(at, to);
        self.core.persist(scope, key, &next).await?;
        *placed = next;
        Ok(())
    }
}

struct Placed<'a> {
    id: &'a str,
    label: &'a str,
    panel: String,
}
fn placed<'a>(entries: &'a [&'static Widget]) -> Vec<Placed<'a>> {
    entries
        .iter()
        .map(|w| Placed {
            id: w.id,
            label: w.label,
            panel: crate::panel_id(w.id),
        })
        .collect()
}
struct Spare<'a> {
    id: &'a str,
    label: &'a str,
}
fn spare<'a>(entries: &'a [&'static Widget]) -> Vec<Spare<'a>> {
    entries
        .iter()
        .map(|w| Spare {
            id: w.id,
            label: w.label,
        })
        .collect()
}
/// The one wording for a region the account may not see.
fn withheld_wording(capabilities: &[&str]) -> String {
    match capabilities {
        [only] => format!("see the dashboard widgets, each of which needs {only}"),
        [rest @ .., last] => format!(
            "see the dashboard widgets; each widget needs its own permission, and these widgets use {} and {last}",
            rest.join(", ")
        ),
        [] => "see the dashboard widgets".into(),
    }
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct View<'a> {
    subtitle: String,
    customize: Html,
    editing: bool,
    flash: FlashMap<'a>,
    widgets: Vec<Placed<'a>>,
    sections: Vec<Placed<'a>>,
    spare_widgets: Vec<Spare<'a>>,
    spare_sections: Vec<Spare<'a>>,
    nothing_placed: bool,
    all_withheld: bool,
    withheld: String,
    catalogue_empty: bool,
    empty_action: Html,
}
fn render(
    scope: &Scope,
    editing: bool,
    flash: Option<&(StatusKind, String)>,
    arrangement: &Arrangement,
) -> Result<String> {
    let mut messages = NO_FLASH;
    if let Some((kind, message)) = flash {
        match kind {
            StatusKind::Success => messages.success = message,
            StatusKind::Error => messages.error = message,
            StatusKind::Warning => messages.warning = message,
            StatusKind::Info => messages.info = message,
        }
    }
    let button = |id: &str, label: &str| {
        Html::render(&Button {
            variant: ButtonVariant::Secondary,
            id,
            event: "toggle_layout_edit",
            body: Html::text(label),
            ..Button::default()
        })
    };
    let render_error = |_| Error::Internal;
    let widgets_empty = arrangement.grid.is_empty();
    let spare_grid_empty = arrangement.spare_grid.is_empty();
    View {
        subtitle: format!("{} · {}", scope.tenant_name, scope.user_name),
        customize: if editing {
            button("close-customize", "Close")
        } else {
            button("customize-layout", "Customize")
        }
        .map_err(render_error)?,
        editing,
        flash: messages,
        widgets: placed(&arrangement.grid),
        sections: placed(&arrangement.sections),
        spare_widgets: spare(&arrangement.spare_grid),
        spare_sections: spare(&arrangement.spare_sections),
        nothing_placed: widgets_empty && !spare_grid_empty,
        all_withheld: widgets_empty && spare_grid_empty && !arrangement.catalogue_empty,
        withheld: withheld_wording(&arrangement.withheld),
        catalogue_empty: arrangement.catalogue_empty,
        empty_action: button("dashboard-widgets-customize", "Customize dashboard")
            .map_err(render_error)?,
    }
    .render()
    .map_err(render_error)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Dashboard")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" || !request.query.is_empty() {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let arrangement = self.0.arrangement(scope).await?;
            Ok(PageBody {
                html: render(scope, false, None, &arrangement)?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Dash::new(&self.0));
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for Dash {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope;
            let mut arrangement = self.core.arrangement(scope).await?;
            let before = self.flash.clone();
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) if values.is_empty() => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    let event: Event =
                        serde_json::from_value(value.clone()).map_err(|_| Error::Invalid)?;
                    self.event(scope, &mut arrangement, event).await?;
                }
            }
            self.specs = arrangement
                .grid
                .iter()
                .chain(&arrangement.sections)
                .map(|widget| PanelArgs::spec(widget, self.editing))
                .collect();
            // The status region announces a message once, when it is new.
            let notice = match &self.flash {
                Some((StatusKind::Success, message)) if self.flash != before => message.clone(),
                _ => String::new(),
            };
            Ok(LiveBody {
                html: render(scope, self.editing, self.flash.as_ref(), &arrangement)?,
                notice,
                rows: arrangement.grid.len() + arrangement.sections.len(),
                ..LiveBody::default()
            })
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        self.specs.clone()
    }
}
