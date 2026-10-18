use super::Pending;
use crate::{Company, FieldError, NewCompany, Params};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_grid::{PageColumns, PageViews};
use bilibi_tenancy::Scope;
use bilibi_ui::*;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio_postgres::GenericClient;

pub(crate) fn render(value: &impl Template) -> Result<Html> {
    Html::render(value).map_err(|_| Error::Internal)
}
pub(crate) async fn allowed(client: &impl GenericClient, scope: &Scope, key: &str) -> Result<bool> {
    match bilibi_authz::authorize_key(client, scope, key).await {
        Ok(()) => Ok(true),
        Err(Error::Forbidden) => Ok(false),
        Err(e) => Err(e),
    }
}
pub(super) fn columns(views: Arc<PageViews>, masked: &HashSet<String>) -> Result<PageColumns> {
    let columns = [
        ("name", "Name", true),
        ("code", "Code", false),
        ("parent_name", "Parent", false),
        ("status", "Status", true),
        ("jurisdiction", "Jurisdiction", true),
    ]
    .into_iter()
    .filter(|(id, _, _)| !masked.contains(*id))
    .map(|(id, label, sort)| {
        let column = BuiltinColumn::new(id, label);
        if sort {
            column.sortable(id)
        } else {
            column
        }
    })
    .collect();
    PageColumns::mount(views, "companies", columns).map_err(|_| Error::Internal)
}
pub(super) fn badge(status: &str) -> Result<Html> {
    render(&Badge::status(
        match status {
            "active" => BadgeKind::Success,
            "pending" => BadgeKind::Warning,
            "suspended" => BadgeKind::Danger,
            _ => BadgeKind::Neutral,
        },
        status,
    ))
}
fn page(id: &str, variant: PageVariant, body: Html) -> Result<String> {
    Page {
        id,
        variant,
        body,
        ..Page::default()
    }
    .render()
    .map_err(|_| Error::Internal)
}
fn card(id: &str, body: Html, flat: bool) -> Result<Html> {
    render(&Card {
        id,
        inner_class: if flat { "p-0" } else { "p-5 sm:p-6" },
        body,
        ..Card::default()
    })
}
fn link(id: &str, title: &str, href: &str) -> Result<Html> {
    render(&ActionLink {
        id,
        icon: "manage",
        title,
        href,
        body: Html::text(title),
        ..ActionLink::default()
    })
}
fn back(id: &str) -> Result<Html> {
    render(&BackLink {
        id,
        href: "/companies",
        title: "Back to companies",
        ..BackLink::default()
    })
}

#[derive(Template)]
#[template(path = "list.html")]
struct ListBody {
    header: Html,
    toolbar: Html,
    card: Html,
}
#[derive(Template)]
#[template(path = "company_name.html")]
struct CompanyName<'a> {
    link: Html,
    legal: Option<&'a str>,
}
pub(super) async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    data: &crate::Page,
    params: &Params,
    columns: &PageColumns,
    workspace: bool,
    masked: &HashSet<String>,
) -> Result<String> {
    let mut actions = Html::EMPTY;
    if allowed(client, scope, crate::CREATE).await? {
        actions.push(&render(&CreateButton {
            noun: "Company",
            href: "/companies/create",
            id: "companies-add",
            class: "",
            event: "",
            button_type: "button",
        })?);
    }
    actions.push(&link(
        "companies-department-types",
        "Department Types",
        "/companies/department-types",
    )?);
    actions.push(&link(
        "companies-legal-entity-types",
        "Legal Entity Types",
        "/companies/legal-entity-types",
    )?);
    actions.push(&link(
        "companies-relationship-types",
        "Relationship Roles",
        "/companies/relationship-types",
    )?);
    let mut pin = IconButton::new("bilimbi-pin", "Pin Companies to sidebar");
    pin.id = "companies-pin";
    pin.context = IconContext::Inline;
    pin.nav_pin = "nav-admin-company";
    let header = render(&Header {
        title_actions: render(&pin)?,
        title: Html::text("Companies"),
        subtitle: "",
        actions: render(&Actions {
            class: "flex flex-wrap items-center gap-2 sm:gap-3",
            body: actions,
        })?,
    })?;
    let placeholder = if masked.contains("email") && masked.contains("jurisdiction") {
        "Search by name, code, or legal name..."
    } else if masked.contains("email") {
        "Search by name, code, legal name, or jurisdiction..."
    } else if masked.contains("jurisdiction") {
        "Search by name, code, legal name, or email..."
    } else {
        "Search by name, code, legal name, email, or jurisdiction..."
    };
    let toolbar = render(&FilterToolbar::new(
        "companies-filters",
        vec![
            ToolbarControl::search(
                "companies-search",
                "search",
                "Search companies",
                &params.q,
                placeholder,
            ),
            ToolbarControl::select(
                "companies-status-filter",
                "status_filter",
                "Status filter",
                &params.status,
                choices(&[
                    ("All statuses", ""),
                    ("Active", "active"),
                    ("Suspended", "suspended"),
                    ("Pending", "pending"),
                    ("Archived", "archived"),
                ]),
            ),
        ],
    ))?;
    let ids: Vec<i64> = data.rows.iter().map(|row| row.id).collect();
    let facts=client.query("SELECT c.id,p.name,(t.company_id IS NOT NULL) FROM companies c LEFT JOIN companies p ON p.id=c.parent_id AND p.tenant_id=c.tenant_id AND p.deleted_at IS NULL LEFT JOIN tenant_primary_companies t ON t.tenant_id=c.tenant_id AND t.company_id=c.id WHERE c.tenant_id=$1 AND c.id=ANY($2)",&[&scope.tenant(),&ids]).await?;
    let mut rows = columns.rows.clone();
    for row in &mut rows {
        let company = data
            .rows
            .iter()
            .find(|c| c.id.to_string() == row.key)
            .ok_or(Error::Internal)?;
        let href = format!("/companies/{}", company.id);
        let id = company.id.to_string();
        let name = render(&RecordLink {
            workspace,
            kind: "core/company",
            record_id: &id,
            href: &href,
            class: "font-medium text-ink-strong hover:underline",
            body: Html::text(&company.name),
        })?;
        let name = render(&CompanyName {
            link: name,
            legal: if columns.mode() == RowMode::Normal {
                company
                    .legal_name
                    .as_deref()
                    .filter(|name| *name != company.name)
            } else {
                None
            },
        })?;
        row.slots.insert("name".into(), name);
        row.slots.insert(
            "code".into(),
            render(&Code {
                text: &company.code,
            })?,
        );
        row.slots.insert("status".into(), badge(&company.status)?);
        row.slots.insert(
            "jurisdiction".into(),
            render(&ReadValue {
                value: company.jurisdiction.as_deref(),
                empty: "—",
            })?,
        );
        if let Some(fact) = facts.iter().find(|r| r.get::<_, i64>(0) == company.id) {
            row.slots.insert(
                "parent_name".into(),
                render(&ReadValue {
                    value: fact.get::<_, Option<String>>(1).as_deref(),
                    empty: "None",
                })?,
            );
            if fact.get::<_, bool>(2) {
                row.actions = render(&Badge::status(BadgeKind::Neutral, "Primary"))?;
            }
        }
    }
    let mut table = columns.flex_table("companies", rows).sorted(
        Some(&params.sort),
        Some(if params.dir == "desc" {
            SortDir::Desc
        } else {
            SortDir::Asc
        }),
    );
    table.caption = "Companies";
    table.framed = false;
    table.actions = true;
    let filtered = !params.q.is_empty() || !params.status.is_empty();
    let title = if filtered {
        format!(
            "No {}companies{}",
            if params.status.is_empty() {
                String::new()
            } else {
                format!("{} ", params.status)
            },
            if params.q.is_empty() {
                String::new()
            } else {
                format!(" match “{}”", params.q)
            }
        )
    } else {
        "No companies yet".into()
    };
    let recovery = if filtered {
        render(&Button {
            id: "companies-clear",
            event: "clear_filters",
            body: Html::text("Clear search and filter"),
            ..Button::default()
        })?
    } else {
        Html::EMPTY
    };
    table.empty = Some(TableEmpty::State(
        EmptyState::missing(
            &title,
            if filtered {
                "Clear the search and filter to see every company."
            } else {
                "Create your first company."
            },
        )
        .action(recovery),
    ));
    let mut contents = render(&table)?;
    contents.push(&render(&Pagination::new(
        "companies-pagination",
        PageCounts::new(params.page as u64, params.size as u64, data.total as u64),
        &[25, 50, 100, 300],
    ))?);
    page(
        "companies-index",
        PageVariant::List,
        render(&ListBody {
            header,
            toolbar,
            card: card("companies-card", contents, true)?,
        })?,
    )
}
#[derive(Template)]
#[template(
    source = "<code class=\"text-xs font-medium tabular-nums\">{{ text }}</code>",
    ext = "html"
)]
struct Code<'a> {
    text: &'a str,
}

struct Options {
    parents: Vec<Choice>,
    types: Vec<Choice>,
    countries: Vec<Choice>,
}
async fn options(
    client: &impl GenericClient,
    scope: &Scope,
    except: Option<i64>,
    create: bool,
) -> Result<Options> {
    let tenant_wide = allowed(client, scope, crate::TENANT_WIDE).await?;
    let own = scope.actor().organisation().map(|id| id.get());
    let parents=client.query("SELECT id,COALESCE(NULLIF(legal_name,''),name) AS label FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL AND id IS DISTINCT FROM $2 AND ($3 OR id=$4 OR id IN (SELECT parent_id FROM companies WHERE tenant_id=$1 AND id=$2)) ORDER BY label,id",&[&scope.tenant(),&except,&tenant_wide,&own]).await?.into_iter().map(|r|Choice::new(r.get::<_,String>(1),r.get::<_,i64>(0).to_string())).collect();
    let types = crate::reference_types::list_legal_entity_types(
        client,
        crate::reference_types::TypeSort::Name,
        crate::reference_types::Direction::Asc,
    )
    .await?
    .into_iter()
    .filter(|t| !create || t.is_active)
    .map(|t| Choice::new(t.name, t.id.to_string()))
    .collect();
    let countries = client
        .query(
            "SELECT iso,country FROM geonames_countries ORDER BY country",
            &[],
        )
        .await?
        .into_iter()
        .map(|r| Choice::new(r.get::<_, String>(1), r.get::<_, String>(0)))
        .collect();
    Ok(Options {
        parents,
        types,
        countries,
    })
}
fn choice(
    id: &str,
    name: &str,
    value: String,
    options: Vec<Choice>,
    status: CommitStatus,
    company: &Company,
    editable: bool,
) -> Result<Html> {
    let display = options
        .iter()
        .find(|c| c.value == value)
        .map(|c| c.label.as_str())
        .unwrap_or(if name == "parent_id" { "None" } else { "—" });
    let editor = if name == "jurisdiction" {
        render(
            &Combobox::new(format!("{id}-input"), name)
                .value(&value)
                .options(options.clone())
                .placeholder("None")
                .aria_label("Jurisdiction")
                .wrapper_class("mb-0 inline-block min-w-56"),
        )?
    } else {
        render(
            &Field::named(name)
                .id(format!("{id}-input"))
                .kind(InputKind::Select)
                .value(&value)
                .prompt("None")
                .options(options.clone())
                .aria_label(name)
                .wrapper_class("mb-0"),
        )?
    };
    let display = if value.is_empty() && name != "parent_id" {
        render(&MutedValue { text: display })?
    } else {
        Html::text(display)
    };
    let mut field = InlineField::new(id, name, format!("Edit {}", name.replace('_', " ")), value)
        .version(company.version)
        .status(status)
        .allow_empty();
    field.id_value = company.id.to_string();
    field.editable = editable;
    render(&InlineChoice {
        field,
        display,
        editor,
    })
}
#[derive(Template)]
#[template(path = "detail.html")]
struct DetailBody<'a> {
    id: i64,
    panels: &'a [String],
    header: Html,
    notice: Html,
    card: Html,
    modal: Html,
}
#[derive(Template)]
#[template(path = "company_status.html")]
struct StatusBody {
    badge: Html,
    operations: Vec<(&'static str, &'static str)>,
    final_status: bool,
}
fn operation_name(op: crate::lifecycle::Operation) -> (&'static str, &'static str) {
    use crate::lifecycle::Operation::*;
    match op {
        Activate => ("activate", "Activate"),
        Reactivate => ("reactivate", "Reactivate"),
        Suspend => ("suspend", "Suspend"),
        Archive => ("archive", "Archive"),
    }
}
/// What the detail page's live state hands its view: commit outcomes, the
/// open dialog, the notice and the Departments table arrangement.
pub(super) struct DetailState<'a> {
    pub statuses: &'a CommitStates,
    pub pending: Option<&'a Pending>,
    pub notice: &'a str,
    pub pending_activity: Option<&'a (String, i64)>,
    pub departments_columns: &'a mut ListColumns,
    pub children_columns: &'a ListColumns,
    pub workspace: bool,
    /// Anchors of the contributed record panels the reader may mount.
    pub panels: &'a [String],
}
pub(super) async fn detail(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    id: i64,
    state: DetailState<'_>,
) -> Result<String> {
    let DetailState {
        statuses,
        pending,
        notice,
        pending_activity,
        departments_columns,
        children_columns,
        workspace,
        panels,
    } = state;
    let (company, masked) = crate::detail(client, scope, id).await?;
    let editable = allowed(client, scope, crate::UPDATE).await? && !company.archived();
    let can_target = allowed(client, scope, crate::TENANT_WIDE).await?
        || scope
            .actor()
            .organisation()
            .is_some_and(|own| own.get() == id);
    let editable = editable && can_target;
    let primary = crate::primary::is_primary(client, scope, id).await?;
    let opts = options(client, scope, Some(id), false).await?;
    let mut actions = badge(&company.status)?;
    actions.push(&render(&History {})?);
    actions.push(&back("company-back")?);
    let header = render(&Header {
        title_actions: render(&RecordPin {
            pin_label: format!("Administration / Companies / {}", company.name),
            url: format!("/companies/{id}"),
        })?,
        title: Html::text(&company.name),
        subtitle: company
            .legal_name
            .as_deref()
            .filter(|n| *n != company.name)
            .unwrap_or(&company.code),
        actions: render(&Actions {
            class: "flex items-center gap-3",
            body: actions,
        })?,
    })?;
    let mut notices = Html::EMPTY;
    for (show, kind, message) in [
        (
            primary,
            StatusKind::Info,
            "This is your organisation's primary company.",
        ),
        (
            company.archived(),
            StatusKind::Warning,
            "This company is archived and read-only. Archiving is final.",
        ),
        (
            !notice.is_empty(),
            if notice == "Company status changed." {
                StatusKind::Success
            } else {
                StatusKind::Error
            },
            notice,
        ),
    ] {
        if show {
            notices.push(&render(&Alert {
                kind,
                id: "",
                class: "mt-4",
                body: Html::text(message),
            })?);
        }
    }
    let mut facts = Vec::new();
    for (name, label, value) in [
        ("name", "Name", company.name.as_str()),
        ("code", "Code", company.code.as_str()),
        (
            "legal_name",
            "Legal Name",
            company.legal_name.as_deref().unwrap_or(""),
        ),
    ] {
        facts.push(Fact {
            title: label,
            id: "",
            value: text_fact(&company, name, value, editable, false, statuses.get(name))?,
        });
    }
    facts.push(Fact {
        title: "Status",
        id: "detail-status",
        value: render(&StatusBody {
            badge: badge(&company.status)?,
            operations: if editable {
                crate::lifecycle::operations_from(&company.status)
                    .into_iter()
                    .map(operation_name)
                    .collect()
            } else {
                Vec::new()
            },
            final_status: company.archived(),
        })?,
    });
    facts.push(Fact {
        title: "Legal Entity Type",
        id: "detail-legal-entity-type",
        value: choice(
            "company-legal-entity-type",
            "legal_entity_type_id",
            company
                .legal_entity_type_id
                .map(|v| v.to_string())
                .unwrap_or_default(),
            opts.types,
            statuses.get("legal_entity_type_id"),
            &company,
            editable,
        )?,
    });
    for (name, label, value) in [
        (
            "registration_number",
            "Registration Number",
            company.registration_number.as_deref().unwrap_or(""),
        ),
        ("tax_id", "Tax ID", company.tax_id.as_str()),
    ] {
        facts.push(Fact {
            title: label,
            id: "",
            value: text_fact(
                &company,
                name,
                value,
                editable,
                masked.contains(name),
                statuses.get(name),
            )?,
        });
    }
    facts.push(Fact {
        title: "Jurisdiction",
        id: "detail-jurisdiction",
        value: if masked.contains("jurisdiction") {
            restricted("company-jurisdiction-restricted")?
        } else {
            choice(
                "company-jurisdiction",
                "jurisdiction",
                company.jurisdiction.clone().unwrap_or_default(),
                opts.countries,
                statuses.get("jurisdiction"),
                &company,
                editable,
            )?
        },
    });
    for (name, label, value) in [
        ("email", "Email", company.email.as_str()),
        (
            "website",
            "Website",
            company.website.as_deref().unwrap_or(""),
        ),
    ] {
        facts.push(Fact {
            title: label,
            id: "",
            value: text_fact(
                &company,
                name,
                value,
                editable,
                masked.contains(name),
                statuses.get(name),
            )?,
        });
    }
    facts.push(Fact {
        title: "Parent Company",
        id: "detail-parent",
        value: choice(
            "company-parent",
            "parent_id",
            company.parent_id.map(|v| v.to_string()).unwrap_or_default(),
            opts.parents,
            statuses.get("parent_id"),
            &company,
            editable,
        )?,
    });
    let context = client
        .query_one(
            "SELECT scope_activities,metadata FROM companies WHERE tenant_id=$1 AND id=$2",
            &[&scope.tenant(), &id],
        )
        .await?;
    for (index, name, label) in [
        (0, "scope_activities", "Business Activities"),
        (1, "metadata", "Metadata"),
    ] {
        let value: Option<Value> = context.get(index);
        if name == "scope_activities" && value.as_ref().is_none_or(|value| value.is_array()) {
            let activities = value
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            let mut field = InlineField::new(
                "company-new-activity",
                "scope_activities",
                "Add business activity",
                "",
            )
            .version(company.version)
            .status(statuses.get("scope_activities"))
            .placeholder("Add activity");
            field.id_value = id.to_string();
            field.class = "min-w-56".into();
            field.save_event = "add_activity".into();
            field.editable = editable;
            facts.push(Fact {
                title: label,
                id: "scope-activities-section",
                value: render(&ActivityBody {
                    activities,
                    editable,
                    add: render(&InlineEdit { field })?,
                })?,
            });
            continue;
        }
        let mut field = InlineField::new(
            format!("company-{name}"),
            name,
            format!("Edit {label}"),
            value
                .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
                .unwrap_or_default(),
        )
        .version(company.version)
        .allow_empty()
        .status(statuses.get(name));
        field.id_value = id.to_string();
        field.editable = editable;
        field.class = "font-mono text-xs".into();
        field.input_class = "font-mono text-xs".into();
        facts.push(Fact {
            title: label,
            id: "",
            value: render(&InlineLongText { field, rows: 5 })?,
        });
    }
    let mut details = render(&SectionHeading {
        id: "company-details-heading",
        title: "Company Details",
        ..SectionHeading::default()
    })?;
    details.push(&render(&Facts {
        id: "company-details",
        items: facts,
    })?);
    let mut content = render(&Card {
        id: "company-details-card",
        inner_class: "p-5 sm:p-6",
        role: "region",
        labelledby: "company-details-heading",
        body: details,
        ..Card::default()
    })?;
    // Child-owning slices contribute the remaining panels; subsidiaries are
    // Company-owned and can be shown immediately without an upward dependency.
    // Keep the platform's bounded read; the extra row makes truncation explicit.
    let children = client.query(
        "SELECT c.id,c.name,c.status,t.name AS legal_entity_type_name,CASE WHEN $3 THEN NULL ELSE c.jurisdiction END AS jurisdiction FROM companies c LEFT JOIN company_legal_entity_types t ON t.id=c.legal_entity_type_id WHERE c.tenant_id=$1 AND c.parent_id=$2 AND c.deleted_at IS NULL ORDER BY c.id LIMIT 301",
        &[&scope.tenant(), &id, &masked.contains("jurisdiction")],
    ).await?;
    if !children.is_empty() {
        let truncated = children.len() > 300;
        let mut rows = Vec::new();
        for child in children.into_iter().take(300) {
            let key = child.get::<_, i64>(0).to_string();
            let href = format!("/companies/{key}");
            let name = child.get::<_, String>(1);
            let status = child.get::<_, String>(2);
            rows.push(
                FlexRow::new(&key)
                    .slot(
                        "name",
                        render(&RecordLink {
                            workspace,
                            kind: "core/company",
                            record_id: &key,
                            href: &href,
                            class: "font-medium hover:underline",
                            body: Html::text(&name),
                        })?,
                    )
                    .slot("status", badge(&status)?)
                    .slot(
                        "legal_entity_type",
                        Html::text(
                            child
                                .get::<_, Option<String>>("legal_entity_type_name")
                                .as_deref()
                                .unwrap_or("—"),
                        ),
                    )
                    .slot(
                        "jurisdiction",
                        if masked.contains("jurisdiction") {
                            render(&Restricted::new(format!("subsidiary-{key}-jurisdiction")))?
                        } else {
                            Html::text(
                                child
                                    .get::<_, Option<String>>("jurisdiction")
                                    .as_deref()
                                    .unwrap_or("—"),
                            )
                        },
                    ),
            );
        }
        let mut children_body = render(&SectionHeading {
            title: "Subsidiaries",
            count: Some(rows.len()),
            description: if truncated {
                "Showing the first 300 subsidiaries. Use Companies to search the full directory."
            } else {
                ""
            },
            ..SectionHeading::default()
        })?;
        let mut table = FlexTable::new(&children_columns.id, children_columns.flex_columns(), rows);
        table.event = "children_grid";
        table.zoom = children_columns.zoom;
        table.suggestions = children_columns.flex_suggestions();
        table.add_query = &children_columns.add_query;
        table.framed = false;
        table.caption = "Subsidiaries";
        children_body.push(&render(&table)?);
        content.push(&card("company-subsidiaries-card", children_body, true)?);
    }
    content.push(&departments_section(client, scope, id, departments_columns).await?);
    // The Relationships section arrives as the `company.relationships` panel
    // (`web::relationships`); the slot keeps the mounted card through patches.
    content.push(&render(&RelationshipsSlot {})?);
    let modal = if let Some((activity, _)) = pending_activity {
        render(&ConfirmDialog {
            id: "company-remove-activity-dialog",
            consequence: &format!(
                "Business activity “{activity}” will be removed from this company."
            ),
            detail: "The company and its other activities are kept.",
            confirm: "Remove",
            working: "Removing…",
            confirm_event: "remove_activity",
            cancel_event: "cancel_remove_activity",
        })?
    } else if let Some(pending) = pending {
        let (_, verb) = operation_name(pending.operation);
        let consequence = format!(
            "{} will be {}.",
            company.name,
            match pending.operation {
                crate::lifecycle::Operation::Activate => "activated",
                crate::lifecycle::Operation::Reactivate => "reactivated",
                crate::lifecycle::Operation::Suspend => "suspended",
                crate::lifecycle::Operation::Archive => "archived",
            }
        );
        let detail = match pending.operation {
            crate::lifecycle::Operation::Archive => "Its records are kept. Archiving is final: an archived company cannot be reactivated.",
            crate::lifecycle::Operation::Suspend => "Its records are kept. Reactivating the company lifts the suspension.",
            crate::lifecycle::Operation::Activate => "It becomes a working company of this workspace. It can be suspended later.",
            crate::lifecycle::Operation::Reactivate => "The suspension is lifted and the company is active again.",
        };
        let mut reason = Field::named("reason")
            .id("company-lifecycle-reason")
            .label("Reason")
            .hint("Optional. Recorded with this change in the audit trail.")
            .kind(InputKind::Textarea)
            .value(&pending.reason)
            .attr("maxlength", "500")
            .errors(if pending.error.is_empty() {
                vec![]
            } else {
                vec![pending.error.clone()]
            });
        reason.attrs.autofocus = true;
        let body = render(&LifecycleForm {
            reason: render(&reason)?,
            cancel: render(&Button {
                id: "company-lifecycle-cancel",
                event: "cancel_lifecycle",
                button_type: "button",
                body: Html::text("Cancel"),
                ..Button::default()
            })?,
            confirm: render(&Button {
                id: "company-lifecycle-confirm",
                button_type: "submit",
                working: match pending.operation {
                    crate::lifecycle::Operation::Activate => "Activating…",
                    crate::lifecycle::Operation::Reactivate => "Reactivating…",
                    crate::lifecycle::Operation::Suspend => "Suspending…",
                    crate::lifecycle::Operation::Archive => "Archiving…",
                },
                variant: if pending.operation == crate::lifecycle::Operation::Archive {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                },
                body: Html::text(verb),
                ..Button::default()
            })?,
        })?;
        render(&Modal {
            id: "company-lifecycle-modal",
            title: &consequence,
            description: detail,
            cancel_event: "cancel_lifecycle",
            body,
            ..Modal::default()
        })?
    } else {
        Html::EMPTY
    };
    page(
        "company-detail-page",
        PageVariant::Detail,
        render(&DetailBody {
            id,
            panels,
            header,
            notice: notices,
            card: content,
            modal,
        })?,
    )
}
#[derive(Template)]
#[template(
    source = "<div id=\"company-record-history-slot\" data-live-ignore><div id=\"company-record-history\"></div></div>",
    ext = "html"
)]
struct History {}
#[derive(Template)]
#[template(
    source = "<div id=\"company-relationships-slot\" data-live-ignore><div id=\"company-relationships\"></div></div>",
    ext = "html"
)]
struct RelationshipsSlot {}
#[derive(Template)]
#[template(path = "company_lifecycle_form.html")]
struct LifecycleForm {
    reason: Html,
    cancel: Html,
    confirm: Html,
}
fn restricted(id: &str) -> Result<Html> {
    render(&Restricted::new(id))
}
fn text_fact(
    company: &Company,
    name: &str,
    value: &str,
    editable: bool,
    masked: bool,
    status: CommitStatus,
) -> Result<Html> {
    let id = format!("company-{}", name.replace('_', "-"));
    if masked {
        return restricted(&id);
    }
    let mut field = InlineField::new(id, name, format!("Edit {}", name.replace('_', " ")), value)
        .version(company.version)
        .status(status);
    field.id_value = company.id.to_string();
    field.editable = editable;
    field.class = match name {
        "name" => "font-semibold",
        "code" => "font-mono",
        _ => "",
    }
    .into();
    if !["name", "code"].contains(&name) {
        field.allow_empty = true;
    }
    render(&InlineEdit { field })
}

#[derive(Template)]
#[template(path = "create.html")]
struct CreateBody {
    header: Html,
    fields: Vec<Html>,
    contacts: Vec<Html>,
    context: Vec<Html>,
    submit: Html,
}
pub(super) async fn create(
    client: &impl GenericClient,
    scope: &Scope,
    values: &HashMap<String, String>,
    errors: &[FieldError],
) -> Result<String> {
    bilibi_authz::authorize_key(client, scope, crate::CREATE).await?;
    let masked = bilibi_authz::restrictions(client, scope).await?;
    let opts = options(client, scope, None, true).await?;
    let mut fields = Vec::new();
    for (name, label, kind, placeholder) in [
        ("parent_id", "Parent Company", InputKind::Select, ""),
        ("status", "Status", InputKind::Select, ""),
        ("name", "Name", InputKind::Text, "Company display name"),
        ("code", "Code", InputKind::Text, "Auto-generated if blank"),
        (
            "legal_name",
            "Legal Name",
            InputKind::Text,
            "Registered legal entity name",
        ),
        (
            "legal_entity_type_id",
            "Legal Entity Type",
            InputKind::Select,
            "",
        ),
        (
            "registration_number",
            "Registration Number",
            InputKind::Text,
            "",
        ),
        ("tax_id", "Tax ID", InputKind::Text, ""),
        ("jurisdiction", "Jurisdiction", InputKind::Select, ""),
        ("email", "Email", InputKind::Email, ""),
        ("website", "Website", InputKind::Text, "example.com"),
    ] {
        let id = format!("company-{}", name.replace('_', "-"));
        if masked.contains(name) {
            fields.push(render(&RestrictedField::new(&id, label))?);
            continue;
        }
        let mut field = Field::named(name)
            .id(id)
            .kind(kind)
            .label(label)
            .value(values.get(name).cloned().unwrap_or_default())
            .placeholder(placeholder)
            .errors(
                errors
                    .iter()
                    .filter(|e| e.field == name)
                    .map(|e| e.message.clone())
                    .collect(),
            );
        if ["name", "status"].contains(&name) {
            field = field.required();
        }
        field = match name {
            "parent_id" => field.prompt("None").options(opts.parents.clone()),
            "legal_entity_type_id" => field.prompt("Select type...").options(opts.types.clone()),
            "jurisdiction" => field
                .prompt("Select country...")
                .options(opts.countries.clone()),
            "status" => field.options(choices(&[("Active", "active"), ("Pending", "pending")])),
            _ => field.attr("maxlength", if name == "name" { "160" } else { "255" }),
        };
        if name == "jurisdiction" {
            fields.push(render(
                &Combobox::new(&field.id, name)
                    .label(label)
                    .value(&field.value)
                    .options(opts.countries.clone())
                    .placeholder("Select country...")
                    .errors(field.errors.clone()),
            )?);
        } else {
            fields.push(render(&field)?);
        }
    }
    let contacts = fields.split_off(8);
    let context = [
        ("scope_activities", "Business Activities (JSON)"),
        ("metadata", "Metadata (JSON)"),
    ]
    .into_iter()
    .map(|(name, label)| {
        render(
            &Field::named(name)
                .id(format!("company-{name}"))
                .label(label)
                .kind(InputKind::Textarea)
                .placeholder(if name == "metadata" {
                    r#"{"employee_count":120}"#
                } else {
                    r#"{"industry":"Manufacturing"}"#
                })
                .value(values.get(name).cloned().unwrap_or_default())
                .errors(
                    errors
                        .iter()
                        .filter(|e| e.field == name)
                        .map(|e| e.message.clone())
                        .collect(),
                ),
        )
    })
    .collect::<Result<Vec<_>>>()?;
    let header = render(&Header {
        title: Html::text("Create Company"),
        subtitle: "",
        actions: back("company-create-back")?,
        ..Header::default()
    })?;
    let submit = render(&Button {
        id: "company-save",
        button_type: "submit",
        variant: ButtonVariant::Primary,
        working: "Creating…",
        body: Html::text("Create Company"),
        ..Button::default()
    })?;
    page(
        "company-create-page",
        PageVariant::Form,
        render(&CreateBody {
            header,
            fields,
            contacts,
            context,
            submit,
        })?,
    )
}
pub(super) fn new_company(
    values: &HashMap<String, String>,
) -> (NewCompany, Option<Value>, Option<Value>, Vec<FieldError>) {
    let text = |name| values.get(name).filter(|s| !s.trim().is_empty()).cloned();
    let mut errors = Vec::new();
    let mut id = |name: &'static str| match text(name) {
        None => None,
        Some(v) => match v.parse::<i64>() {
            Ok(v) if v > 0 => Some(v),
            _ => {
                errors.push(FieldError::new(name, "is invalid"));
                None
            }
        },
    };
    let new = NewCompany {
        name: text("name").unwrap_or_default(),
        code: text("code"),
        status: text("status"),
        parent_id: id("parent_id"),
        legal_name: text("legal_name"),
        registration_number: text("registration_number"),
        tax_id: text("tax_id"),
        legal_entity_type_id: id("legal_entity_type_id"),
        jurisdiction: text("jurisdiction"),
        email: text("email"),
        website: text("website"),
        ..NewCompany::default()
    };
    let mut object = |name: &'static str| {
        text(name).and_then(|value| match serde_json::from_str::<Value>(&value) {
            Ok(v) if v.is_object() => Some(v),
            _ => {
                errors.push(FieldError::new(name, "must be a valid JSON object"));
                None
            }
        })
    };
    let activities = object("scope_activities");
    let metadata = object("metadata");
    (new, activities, metadata, errors)
}

#[derive(Template)]
#[template(path = "company_activities.html")]
struct ActivityBody {
    activities: Vec<String>,
    editable: bool,
    add: Html,
}

#[derive(Template)]
#[template(source = "<div class=\"{{ class }}\">{{ body }}</div>", ext = "html")]
pub(super) struct Actions<'a> {
    pub class: &'a str,
    pub body: Html,
}
#[derive(Template)]
#[template(
    source = "<span{% if value.is_none() %} class=\"text-ink-faint\"{% endif %}>{{ value.unwrap_or(empty) }}</span>",
    ext = "html"
)]
struct ReadValue<'a> {
    value: Option<&'a str>,
    empty: &'a str,
}

#[derive(Template)]
#[template(path = "company_pin.html")]
struct RecordPin {
    pin_label: String,
    url: String,
}

#[derive(Template)]
#[template(
    source = "<span class=\"text-ink-muted\">{{ text }}</span>",
    ext = "html"
)]
struct MutedValue<'a> {
    text: &'a str,
}

// Bilimbi Company.ShowLive children_columns; independent of Departments.
pub(super) fn children_builtins() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("name", "Name"),
        BuiltinColumn::new("status", "Status"),
        BuiltinColumn::new("legal_entity_type", "Legal Entity Type"),
        BuiltinColumn::new("jurisdiction", "Jurisdiction"),
    ]
}
/// The detail page's Departments table (Bilimbi `ShowLive` section 5):
/// built-in columns only, with Manage leading to the departments page.
pub(super) fn department_builtins() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("type", "Department Type"),
        BuiltinColumn::new("category", "Category"),
        BuiltinColumn::new("head", "Head"),
        BuiltinColumn::new("status", "Status"),
    ]
}
#[derive(Template)]
#[template(path = "department_section.html")]
struct DepartmentSection {
    heading: Html,
    table: Html,
}
async fn departments_section(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    id: i64,
    columns: &mut ListColumns,
) -> Result<Html> {
    let departments = crate::departments::list(
        client,
        scope,
        id,
        crate::departments::DepartmentSort::Name,
        crate::reference_types::Direction::Asc,
    )
    .await?;
    let heads = crate::departments::head_names(client, scope, &departments).await?;
    let href = format!("/companies/{id}/departments");
    let heading = render(&SectionHeading {
        id: "company-departments-heading",
        title: "Departments",
        count: Some(departments.len()),
        actions: render(&ActionLink {
            id: "company-departments-manage",
            icon: "manage",
            title: "Manage departments",
            href: &href,
            body: Html::text("Manage"),
            ..ActionLink::default()
        })?,
        ..SectionHeading::default()
    })?;
    let mut rows = Vec::with_capacity(departments.len());
    for department in &departments {
        let key = department.id.to_string();
        let mut row = FlexRow::new(&key);
        row.dom_id = Some(format!("department-{key}"));
        let head = department
            .head_affiliation_id
            .and_then(|head| heads.get(&head))
            .map(String::as_str)
            .unwrap_or("—");
        let muted = |text: &str| {
            Html::raw(format!(
                "<span class=\"text-sm text-ink-subtle\">{}</span>",
                Html::text(text)
            ))
        };
        rows.push(
            row.slot(
                "type",
                Html::raw(format!(
                    "<span class=\"font-medium text-ink-strong\">{}</span>",
                    Html::text(&department.type_name)
                )),
            )
            .slot("category", muted(&department.type_category))
            .slot("head", muted(head))
            .slot(
                "status",
                render(&Badge::status(
                    if department.status == "active" {
                        BadgeKind::Success
                    } else {
                        BadgeKind::Warning
                    },
                    &super::reference_types::capitalize(&department.status),
                ))?,
            ),
        );
    }
    let mut table = FlexTable::new("company-departments-table", columns.flex_columns(), rows);
    table.event = "departments_grid";
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.caption = "Departments";
    table.framed = false;
    if departments.is_empty() {
        table.empty = Some(TableEmpty::Content(Html::text(
            "No departments configured.",
        )));
    }
    render(&Card {
        id: "company-departments-card",
        class: "mt-6",
        inner_class: "p-0",
        role: "region",
        labelledby: "company-departments-heading",
        body: render(&DepartmentSection {
            heading,
            table: render(&table)?,
        })?,
        ..Card::default()
    })
}
