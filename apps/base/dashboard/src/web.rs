//! The `/dashboard` page and its mounted state.
//!
//! The page owns the arrangement and nothing else. Each visible entry is a
//! server-declared panel (`LiveState::panels`) that Web mounts from the entry's
//! discovered embed, so this module names no owner and reads no owner's data.
//! Every update re-derives the arrangement from storage and a freshly guarded
//! scope: an entry whose capability is gone leaves the page (and its panel state
//! is dropped by Web) while its siblings keep running, and returns in its stored
//! place when the capability does. Providers recheck placed entries on every
//! update, but offer unplaced entries only in Customize.
use crate::{Dashboard, Item, PanelArgs, Placement, LAYOUT, SECTIONS};
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
    grid: Vec<Item>,
    sections: Vec<Item>,
    spare_grid: Vec<Item>,
    spare_sections: Vec<Item>,
    /// The capabilities that withheld every grid entry, for the empty wording.
    withheld: Vec<&'static str>,
    catalogue_empty: bool,
    /// An authorized provider can offer entries when Customize is opened.
    provider_offers: bool,
    /// Hidden stored pins still reserve space, so restoring access cannot
    /// overflow the host's panel limit after adding another entry.
    hidden: usize,
}
impl Core {
    async fn arrangement(&self, scope: &Scope, offers: bool) -> Result<Arrangement> {
        let client = self.db.get().await?;
        let mut decided: HashMap<&str, bool> = HashMap::new();
        let capabilities = self
            .dashboard
            .entries()
            .iter()
            .filter_map(|widget| widget.capability)
            .chain(
                self.dashboard
                    .providers()
                    .iter()
                    .filter_map(|p| p.capability),
            );
        for key in capabilities {
            if !decided.contains_key(key) {
                let allowed = match authorize_key(&**client, scope, key).await {
                    Ok(()) => true,
                    Err(Error::Forbidden) => false,
                    Err(error) => return Err(error),
                };
                decided.insert(key, allowed);
            }
        }
        let offered = |capability: Option<&str>| capability.is_none_or(|key| decided[key]);
        let contributed: Vec<Item> = self
            .dashboard
            .entries()
            .iter()
            .filter(|widget| offered(widget.capability))
            .map(|widget| Item::widget(widget))
            .collect();
        let mut stored = self
            .settings
            .resolve_many(
                &**client,
                scope,
                Target::User { company: None },
                &[LAYOUT, SECTIONS],
            )
            .await?;
        // A provider takes its own pooled clients: holding this one across
        // them lets concurrent page updates starve the pool.
        drop(client);
        // Placed ids, so a provider answers those before choosing which of
        // its other entries to offer: a pin never falls past the offer cap.
        let placed_ids: Vec<String> = stored
            .values()
            .filter(|s| s.overridden)
            .filter_map(|s| s.value.as_array())
            .flatten()
            .filter_map(|id| id.as_str().map(str::to_owned))
            .collect();
        let mut provided: Vec<Item> = Vec::new();
        for provider in self.dashboard.providers() {
            if !offered(provider.capability) {
                continue;
            }
            let prefix = format!("{}-", provider.prefix);
            let pinned: Vec<String> = placed_ids
                .iter()
                .filter_map(|id| id.strip_prefix(&prefix).map(str::to_owned))
                .collect();
            if pinned.is_empty() && !offers {
                continue;
            }
            let entries = match (provider.entries)(scope, &pinned, offers).await {
                Ok(entries) => entries,
                Err(Error::Forbidden) => continue,
                Err(error) => return Err(error),
            };
            let (kept, others): (Vec<_>, Vec<_>) =
                entries.into_iter().partition(|e| pinned.contains(&e.key));
            provided.extend(
                kept.into_iter()
                    .chain(others.into_iter().take(crate::MAX_PROVIDED))
                    .filter_map(|entry| Item::provided(provider, entry)),
            );
        }
        let mut hidden = 0;
        let mut arrange = |key: &str, placement: Placement| -> Result<_> {
            let stored = stored.remove(key).ok_or(Error::Internal)?;
            let own: Vec<_> = contributed
                .iter()
                .filter(|i| i.placement == placement)
                .cloned()
                .collect();
            let available: Vec<_> = own
                .iter()
                .chain(provided.iter().filter(|i| i.placement == placement))
                .cloned()
                .collect();
            // No stored row shows the whole authorized catalogue of contributed
            // entries (never a provided one); a stored row keeps its order and
            // withholds ids the account can no longer see without forgetting them.
            let placed: Vec<Item> = if stored.overridden {
                let ids = stored.value.as_array().ok_or(Error::Invalid)?;
                ids.iter()
                    .filter_map(|id| {
                        available
                            .iter()
                            .find(|w| Some(w.id.as_str()) == id.as_str())
                    })
                    .cloned()
                    .collect()
            } else {
                own
            };
            if stored.overridden {
                hidden += stored.value.as_array().ok_or(Error::Invalid)?.len() - placed.len();
            }
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
        let provider_offers = !offers
            && self.dashboard.providers().iter().any(|provider| {
                provider.placement == Placement::Grid && offered(provider.capability)
            });
        let catalogue_empty = self.dashboard.widgets().is_empty()
            && !provider_offers
            && !provided.iter().any(|i| i.placement == Placement::Grid);
        Ok(Arrangement {
            grid,
            sections,
            spare_grid,
            spare_sections,
            withheld,
            catalogue_empty,
            provider_offers,
            hidden,
        })
    }

    /// A self-service write to the signed-in account's own preference, so it is
    /// not a privileged administration write.
    async fn persist(
        &self,
        scope: &Scope,
        key: &str,
        before: &[Item],
        entries: &[Item],
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin_view_state(&mut client, scope.mutation_context("dashboard.arrangement"))
            .await?;
        let mut stored = self
            .settings
            .resolve_many(tx.sql(), scope, Target::User { company: None }, &[key])
            .await?;
        let stored = stored.remove(key).ok_or(Error::Internal)?;
        let previous: Vec<String> = if stored.overridden {
            serde_json::from_value(stored.value).map_err(|_| Error::Invalid)?
        } else {
            before.iter().map(|item| item.id.clone()).collect()
        };
        // Replace only the previously visible slots in their new order. Hidden
        // pins keep their slots, and only an explicit visible removal disappears.
        let mut next = entries.iter().map(|item| item.id.clone());
        let mut ids = Vec::new();
        for id in previous {
            if before.iter().any(|item| item.id == id) {
                // Removing a visible entry removes that slot, not a later
                // visible slot on the other side of a hidden pin.
                if entries.iter().any(|item| item.id == id) {
                    ids.extend(next.next());
                }
            } else {
                ids.push(id);
            }
        }
        ids.extend(next);
        // Another tab may have added the same entry since `before` was read.
        // It then appears both as a preserved pin and in the pending additions.
        let mut seen = std::collections::HashSet::new();
        ids.retain(|id| seen.insert(id.clone()));
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
            Event::AddWidget { .. } | Event::AddSection { .. } if full(arrangement) => {
                self.flash = Some((
                    StatusKind::Error,
                    format!("A dashboard holds at most {MAX_PANELS} panels. Remove one first."),
                ));
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
                let mut known: Vec<_> = current.iter().map(|w| w.id.as_str()).collect();
                known.sort_unstable();
                if sorted.len() == ids.len() && sorted == known {
                    let reordered: Vec<_> = ids
                        .iter()
                        .filter_map(|id| current.iter().find(|w| &w.id == id))
                        .cloned()
                        .collect();
                    if reordered
                        .iter()
                        .map(|w| &w.id)
                        .ne(current.iter().map(|w| &w.id))
                    {
                        self.core
                            .persist(scope, LAYOUT, current, &reordered)
                            .await?;
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
        placed: &mut Vec<Item>,
        spare: &[Item],
        id: &str,
    ) -> Result<()> {
        // A stale or forged id changes nothing.
        let Some(widget) = spare.iter().find(|w| w.id == id) else {
            return Ok(());
        };
        let mut next = placed.clone();
        next.push(widget.clone());
        self.core.persist(scope, key, placed, &next).await?;
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
        placed: &mut Vec<Item>,
        id: &str,
        message: Option<&str>,
    ) -> Result<()> {
        let Some(at) = placed.iter().position(|w| w.id == id) else {
            return Ok(());
        };
        let mut next = placed.clone();
        let widget = next.remove(at);
        self.core.persist(scope, key, placed, &next).await?;
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
        placed: &mut Vec<Item>,
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
        self.core.persist(scope, key, placed, &next).await?;
        *placed = next;
        Ok(())
    }
}

/// Panels the live host mounts for one page at most.
const MAX_PANELS: usize = 32;
fn full(arrangement: &Arrangement) -> bool {
    arrangement.grid.len() + arrangement.sections.len() + arrangement.hidden >= MAX_PANELS
}

struct Placed<'a> {
    id: &'a str,
    label: &'a str,
    panel: String,
}
fn placed(entries: &[Item]) -> Vec<Placed<'_>> {
    entries
        .iter()
        .map(|w| Placed {
            id: &w.id,
            label: &w.label,
            panel: crate::panel_id(&w.id),
        })
        .collect()
}
struct Spare<'a> {
    id: &'a str,
    label: &'a str,
}
fn spare(entries: &[Item]) -> Vec<Spare<'_>> {
    entries
        .iter()
        .map(|w| Spare {
            id: &w.id,
            label: &w.label,
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
    let spare_grid_empty = arrangement.spare_grid.is_empty() && !arrangement.provider_offers;
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
            let arrangement = self.0.arrangement(scope, false).await?;
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
            let mut arrangement = self.core.arrangement(scope, self.editing).await?;
            let before = self.flash.clone();
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) if values.is_empty() => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    let event: Event =
                        serde_json::from_value(value.clone()).map_err(|_| Error::Invalid)?;
                    self.event(scope, &mut arrangement, event).await?;
                    // Re-read so what was removed is offered again at once,
                    // not after the next refresh.
                    arrangement = self.core.arrangement(scope, self.editing).await?;
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
