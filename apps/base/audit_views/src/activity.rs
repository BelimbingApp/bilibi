//! The dashboard's Recent Activity panel (embed `dashboard.activity`): the five most recent audit mutations in
//! this tenant.
//!
//! The dashboard offers the card to holders of `admin.audit.log.list`; the
//! embed itself is only `SignedIn`, so a card already on the page stays mounted
//! after the grant is removed and the panel shows an em dash without reading.
//! Like every panel it answers from its `PanelCache` until the entry's refresh
//! interval passes, and asks the capability again on each real read.
use crate::{present, LIST};
use askama::Template;
use bilibi_authz::effective_capabilities;
use bilibi_dashboard::{PanelArgs, PanelCache};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::DisplayPolicy;
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageFuture,
        PageRequest,
    },
    EmptyState, Html, IconButton, IconContext,
};
use std::sync::Arc;

const FEED: i64 = 5;

pub(crate) struct Panels {
    pub db: Db,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

struct Panel {
    db: Db,
    display: Arc<DisplayPolicy>,
    locales: Arc<Locales>,
    args: PanelArgs,
    cache: PanelCache,
}

struct Entry {
    event: String,
    kind: String,
    when: Html,
}
enum Feed {
    /// Not read: the reader lacks `LIST`.
    Pending,
    Empty,
    Entries(Vec<Entry>),
}
#[derive(Template)]
#[template(path = "activity.html")]
struct Activity {
    id: String,
    open: Html,
    feed: Feed,
    empty: EmptyState,
}

impl Page<Scope, Error> for Panels {
    fn render<'a>(&'a self, _request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async { Err(Error::NotFound) })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "recent" {
                return Err(Error::NotFound);
            }
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Panel {
                db: self.db.clone(),
                display: self.display.clone(),
                locales: self.locales.clone(),
                args: PanelArgs::from_request(&request)?,
                cache: PanelCache::default(),
            });
            Ok(Some(state))
        })
    }
}

impl Panel {
    async fn read(&self, scope: &Scope) -> Result<String> {
        let client = self.db.get().await?;
        let allowed = effective_capabilities(&**client, scope)
            .await?
            .contains(LIST);
        let feed = if allowed {
            let locale = self.locales.resolve(&**client, scope, None).await?.locale;
            let display = self.display.display(&**client, scope, None, locale).await?;
            let recent = bilibi_audit::recent_mutations(&**client, scope, FEED).await?;
            if recent.is_empty() {
                Feed::Empty
            } else {
                let mut entries = Vec::new();
                for mutation in recent {
                    entries.push(Entry {
                        when: present::time(
                            &display,
                            &format!("audit-entry-{}", mutation.id),
                            mutation.occurred_at,
                            "text-xs tabular-nums text-ink-faint",
                        )?,
                        event: mutation.event,
                        kind: mutation.auditable_type,
                    });
                }
                Feed::Entries(entries)
            }
        } else {
            Feed::Pending
        };
        // The open control is a navigation, so it is withheld while the account
        // customizes the layout (the page lays its own controls over this corner).
        let open = if !self.args.editing && allowed {
            let mut button = IconButton::new("forward", "Open audit log");
            button.context = IconContext::Inline;
            button.id = "stat-recent-audit-open";
            // Opens the Data Mutations list, which this feed shows.
            button.href = "/audit/mutations";
            Html::render(&button).map_err(|_| Error::Internal)?
        } else {
            Html::EMPTY
        };
        Activity {
            id: self.args.id(),
            open,
            feed,
            empty: EmptyState::missing("No recent activity.", "").id("stat-recent-audit-empty"),
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if !matches!(request.input, LiveInput::Refresh) {
                return Err(Error::Invalid);
            }
            if let Some(html) = self.cache.fresh(&self.args) {
                return Ok(LiveBody {
                    html: html.to_owned(),
                    ..LiveBody::default()
                });
            }
            let html = self.read(request.scope).await?;
            Ok(LiveBody {
                html: self.cache.store(html),
                ..LiveBody::default()
            })
        })
    }
}
