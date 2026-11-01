//! The dashboard entries (Bilimbi `DashboardStatsPanel`, embed
//! `dashboard.users`, and `DashboardPeoplePanel`, embed `dashboard.people`):
//! the current workplace's account counts as a stat strip, and its first five accounts
//! on the shared flexible table. Both read `summary` once per refresh period
//! the dashboard asks for, under `admin.user.list`; without it the strip shows
//! em dashes and the table nothing, and neither reads.
use super::{event, render, summary, verified_badge, Listed, Strong, Summary};
use crate::{contributions::LIST, web::Pages};
use askama::Template;
use bilibi_dashboard::PanelArgs;
use bilibi_database::{Error, Result};
use bilibi_security::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    params, ActionLink, BuiltinColumn, Card, ColumnsOutcome, FlexRow, FlexTable, Html, ListColumns,
    SectionHeading, StatItem, StatStrip, TableEmpty,
};
use std::time::Instant;

/// The last read and when; `refresh` from the dashboard says how long it
/// stands. `None` inside means the actor may not list accounts.
struct Cache {
    read: Option<(i64, Option<Summary>, Instant)>,
}
impl Cache {
    async fn summary(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        args: &PanelArgs,
    ) -> Result<&Option<Summary>> {
        let company = scope.current_company().ok_or(Error::Forbidden)?.get();
        let fresh = self.read.as_ref().is_some_and(|(cached, _, at)| {
            *cached == company && (args.refresh.is_zero() || at.elapsed() < args.refresh)
        });
        if !fresh {
            let value = if pages.allowed(scope, LIST).await? {
                let client = pages.db.get().await?;
                Some(summary(&**client, scope).await?)
            } else {
                None
            };
            self.read = Some((company, value, Instant::now()));
        }
        Ok(&self.read.as_ref().ok_or(Error::Internal)?.1)
    }
}

pub(crate) struct Stats {
    pages: Pages,
    args: PanelArgs,
    cache: Cache,
}
impl Stats {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            pages,
            args: PanelArgs::from_request(request)?,
            cache: Cache { read: None },
        })
    }
}
impl LiveState<Scope, Error> for Stats {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if !matches!(request.input, LiveInput::Refresh) {
                return Err(Error::Invalid);
            }
            let read = self
                .cache
                .summary(&self.pages, request.scope, &self.args)
                .await?;
            let (total, verified, pending) = match read {
                Some(summary) => (
                    summary.total.to_string(),
                    summary.verified.to_string(),
                    (summary.total - summary.verified).to_string(),
                ),
                None => ("—".into(), "—".into(), "—".into()),
            };
            let id = self.args.id();
            let html = render(&StatStrip {
                id: &id,
                title: "Users",
                href: if read.is_some() && !self.args.editing {
                    "/users"
                } else {
                    ""
                },
                items: vec![
                    StatItem::number("Total", total),
                    StatItem::number("Verified", verified),
                    StatItem::number("Pending", pending),
                ],
            })?;
            Ok(LiveBody {
                html: html.to_string(),
                rows: 1,
                ..LiveBody::default()
            })
        })
    }
}

#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" data-target="{{ id }}">{{ card }}</div>"#,
    ext = "html"
)]
struct View<'a> {
    id: &'a str,
    card: Html,
}
#[derive(Template)]
#[template(
    source = r#"<div class="px-5 pt-5 sm:px-6 sm:pt-6">{{ heading }}</div>{{ table }}"#,
    ext = "html"
)]
struct CardBody {
    heading: Html,
    table: Html,
}

pub(crate) struct People {
    pages: Pages,
    args: PanelArgs,
    cache: Cache,
    columns: ListColumns,
}
impl People {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            pages,
            args: PanelArgs::from_request(request)?,
            cache: Cache { read: None },
            columns: ListColumns::mount(
                "dashboard-users",
                vec![
                    BuiltinColumn::new("name", "Name"),
                    BuiltinColumn::new("email", "Email"),
                    BuiltinColumn::new("email_verified", "Email verified"),
                ],
            )
            .map_err(|_| Error::Internal)?,
        })
    }
}
impl LiveState<Scope, Error> for People {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => match event(value)? {
                    "people_grid" => {
                        if let ColumnsOutcome::Update | ColumnsOutcome::Noop = self.columns.handle(
                            &params::string_values_checked(value, params::FIELD_LIMIT)
                                .map_err(Error::refused)?,
                        ) {
                            rows_changed = false;
                        }
                    }
                    _ => return Err(Error::Invalid),
                },
            }
            let read = self
                .cache
                .summary(&self.pages, request.scope, &self.args)
                .await?;
            let users: &[Listed] = read.as_ref().map_or(&[], |s| s.users.as_slice());
            if rows_changed {
                self.columns.load(users, Listed::key);
            }
            let mut rows = Vec::with_capacity(users.len());
            for user in users {
                rows.push(
                    FlexRow::new(user.key())
                        .slot("name", render(&Strong { text: &user.name })?)
                        .slot("email", Html::text(&user.email))
                        .slot("email_verified", verified_badge(user)?),
                );
            }
            let id = self.args.id();
            let mut table = FlexTable::new("dashboard-users", self.columns.flex_columns(), rows);
            table.zoom = self.columns.zoom;
            table.suggestions = self.columns.flex_suggestions();
            table.add_query = &self.columns.add_query;
            table.event = "people_grid";
            table.target = &id;
            table.caption = "People in this workspace";
            table.framed = false;
            if users.is_empty() {
                table.empty = Some(TableEmpty::Content(Html::text(if read.is_some() {
                    "No accounts in this workspace yet."
                } else {
                    "—"
                })));
            }
            let actions = if read.is_some() && !self.args.editing {
                render(&ActionLink {
                    id: "dashboard-users-open",
                    icon: "forward",
                    title: "All users",
                    href: "/users",
                    method: "",
                    body: Html::text("All users"),
                })?
            } else {
                Html::EMPTY
            };
            let heading = render(&SectionHeading {
                id: "dashboard-recent-users-heading",
                title: "People in this workspace",
                actions,
                ..SectionHeading::default()
            })?;
            let card = render(&Card {
                id: "dashboard-recent-users",
                inner_class: "p-0",
                role: "region",
                labelledby: "dashboard-recent-users-heading",
                body: render(&CardBody {
                    heading,
                    table: render(&table)?,
                })?,
                ..Card::default()
            })?;
            Ok(LiveBody {
                html: render(&View { id: &id, card })?.to_string(),
                rows: users.len(),
                ..LiveBody::default()
            })
        })
    }
}
