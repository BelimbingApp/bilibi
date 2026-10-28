//! The dashboard entries (Bilimbi `DashboardStatsPanel`, embed
//! `dashboard.companies`, and `DashboardCompanyPanel`, embed
//! `dashboard.company`): the tenant's live company counts beside the viewer's
//! company code, and that company's facts. The viewer's company is the one
//! they are working in (`Scope::current_company`: the session's choice, else the
//! affiliation); an account with neither has no current company, and the
//! panels say so instead of guessing one (Bilimbi's first-company fallback
//! would contradict the top bar's "Choose a company"). Both read
//! `summary` once per refresh period the dashboard asks for; neither needs a
//! capability to read, and each link is offered only to a reader its page
//! admits.
use super::views::{allowed, render};
use crate::{organisation::display_name, LIST, VIEW};
use askama::Template;
use bilibi_dashboard::PanelArgs;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    ActionLink, Badge, BadgeKind, Card, EmptyState, Fact, Facts, Html, SectionHeading, StatItem,
    StatStrip,
};
use std::time::Instant;
use tokio_postgres::GenericClient;

/// The one company a panel shows.
struct Current {
    id: i64,
    code: String,
    name: String,
    status: String,
}
/// Bilimbi `Company.dashboard_summary/2`: counts over every live company of
/// the tenant and one company, in one statement and so one snapshot.
struct Summary {
    total: i64,
    active: i64,
    company: Option<Current>,
}
async fn summary(client: &impl GenericClient, scope: &Scope) -> Result<Summary> {
    let current = scope.current_company().map_or(0, |id| id.get());
    let row = client
        .query_opt(
            "SELECT id, code, name, legal_name, status, count(*) OVER () AS total, count(*) FILTER (WHERE status = 'active') OVER () AS active FROM companies WHERE tenant_id = $1 AND deleted_at IS NULL ORDER BY id = $2 DESC, id LIMIT 1",
            &[&scope.tenant(), &current],
        )
        .await?;
    Ok(match row {
        None => Summary {
            total: 0,
            active: 0,
            company: None,
        },
        Some(row) => Summary {
            total: row.get(5),
            active: row.get(6),
            company: (row.get::<_, i64>(0) == current).then(|| Current {
                id: row.get(0),
                code: row.get(1),
                name: display_name(row.get::<_, Option<&str>>(3), row.get(2)),
                status: row.get(4),
            }),
        },
    })
}

/// The last read, whether its link may show, and when; `refresh` from the
/// dashboard says how long it stands.
struct Cache {
    read: Option<(Summary, bool, Instant)>,
}
impl Cache {
    async fn summary(
        &mut self,
        db: &Db,
        scope: &Scope,
        args: &PanelArgs,
        link: &str,
    ) -> Result<(&Summary, bool)> {
        let fresh = self
            .read
            .as_ref()
            .is_some_and(|(_, _, at)| args.refresh.is_zero() || at.elapsed() < args.refresh);
        if !fresh {
            let client = db.get().await?;
            let read = summary(&**client, scope).await?;
            let linked = allowed(&**client, scope, link).await?;
            self.read = Some((read, linked, Instant::now()));
        }
        let (read, linked, _) = self.read.as_ref().ok_or(Error::Internal)?;
        Ok((read, *linked))
    }
}

#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" data-target="{{ id }}">{{ body }}</div>"#,
    ext = "html"
)]
struct View<'a> {
    id: &'a str,
    body: Html,
}
#[derive(Template)]
#[template(
    source = r#"<code class="font-medium">{{ code }}</code>"#,
    ext = "html"
)]
struct Code<'a> {
    code: &'a str,
}

fn only_refresh(input: &LiveInput<'_>) -> Result<()> {
    match input {
        LiveInput::Refresh => Ok(()),
        _ => Err(Error::Invalid),
    }
}

pub(crate) struct Stats {
    db: Db,
    args: PanelArgs,
    cache: Cache,
}
impl Stats {
    pub fn new(db: Db, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            db,
            args: PanelArgs::from_request(request)?,
            cache: Cache { read: None },
        })
    }
}
impl LiveState<Scope, Error> for Stats {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            only_refresh(&request.input)?;
            let (read, linked) = self
                .cache
                .summary(&self.db, request.scope, &self.args, LIST)
                .await?;
            let strip = render(&StatStrip {
                id: "stat-companies",
                title: "Companies",
                href: if linked && !self.args.editing {
                    "/companies"
                } else {
                    ""
                },
                items: vec![
                    StatItem::number("Total", read.total),
                    StatItem::number("Active", read.active),
                    StatItem::text(
                        "Current",
                        read.company.as_ref().map_or("—", |c| c.code.as_str()),
                    ),
                ],
            })?;
            let id = self.args.id();
            Ok(LiveBody {
                html: render(&View {
                    id: &id,
                    body: strip,
                })?
                .to_string(),
                rows: 1,
                ..LiveBody::default()
            })
        })
    }
}

pub(crate) struct CurrentCompany {
    db: Db,
    args: PanelArgs,
    cache: Cache,
}
impl CurrentCompany {
    pub fn new(db: Db, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            db,
            args: PanelArgs::from_request(request)?,
            cache: Cache { read: None },
        })
    }
}
impl LiveState<Scope, Error> for CurrentCompany {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            only_refresh(&request.input)?;
            let (read, linked) = self
                .cache
                .summary(&self.db, request.scope, &self.args, VIEW)
                .await?;
            let body = match &read.company {
                None if read.total == 0 => render(
                    &EmptyState::missing("No company yet.", "")
                        .id("dashboard-current-company-none"),
                )?,
                None => render(
                    &EmptyState::missing(
                        "No current company.",
                        "Choose a company in the top bar to see it here.",
                    )
                    .id("dashboard-current-company-none"),
                )?,
                Some(company) => {
                    let href = format!("/companies/{}", company.id);
                    let actions = if linked && !self.args.editing {
                        render(&ActionLink {
                            id: "dashboard-company-open",
                            icon: "forward",
                            title: "Open company",
                            href: &href,
                            method: "",
                            body: Html::text("Open company"),
                        })?
                    } else {
                        Html::EMPTY
                    };
                    let heading = render(&SectionHeading {
                        id: "dashboard-current-company-heading",
                        title: "Current company",
                        actions,
                        ..SectionHeading::default()
                    })?;
                    let facts = render(&Facts {
                        id: "dashboard-company-facts",
                        items: vec![
                            Fact {
                                title: "Name",
                                id: "dashboard-company-name",
                                value: Html::text(&company.name),
                            },
                            Fact {
                                title: "Code",
                                id: "",
                                value: render(&Code {
                                    code: &company.code,
                                })?,
                            },
                            Fact {
                                title: "Status",
                                id: "",
                                value: render(&Badge {
                                    kind: if company.status == "active" {
                                        BadgeKind::Success
                                    } else {
                                        BadgeKind::Warning
                                    },
                                    dot: true,
                                    class: "",
                                    body: Html::text(&company.status),
                                })?,
                            },
                        ],
                    })?;
                    let mut body = heading;
                    body.push(&facts);
                    render(&Card {
                        id: "dashboard-current-company",
                        inner_class: "p-5 sm:p-6",
                        role: "region",
                        labelledby: "dashboard-current-company-heading",
                        body,
                        ..Card::default()
                    })?
                }
            };
            let id = self.args.id();
            Ok(LiveBody {
                html: render(&View { id: &id, body })?.to_string(),
                rows: 1,
                ..LiveBody::default()
            })
        })
    }
}
