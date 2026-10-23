use super::fields;
use super::Pending;
use crate::{Company, FieldError, NewCompany, Params};
use askama::Template;
use bilibi_custom_fields::ReferenceOption;
use bilibi_custom_fields::{Definition, FieldType};
use bilibi_database::{Error, Result};
use bilibi_grid::{PageColumns, PageViews};
use bilibi_tenancy::Scope;
use bilibi_ui::*;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
    PageColumns::mount(views, "companies", columns)
        .map(PageColumns::sortable_runtime_fields)
        .map_err(|_| Error::Internal)
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
pub(super) struct CustomListState<'a> {
    pub definitions: &'a [Definition],
    pub filters: &'a [crate::CustomFilter],
}
pub(super) struct ListRenderState<'a> {
    pub params: &'a Params,
    pub columns: &'a PageColumns,
    pub workspace: bool,
    pub masked: &'a HashSet<String>,
    pub warning: &'a str,
    pub custom: CustomListState<'a>,
}
pub(super) async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    data: &crate::Page,
    state: ListRenderState<'_>,
) -> Result<String> {
    let ListRenderState {
        params,
        columns,
        workspace,
        masked,
        warning,
        custom,
    } = state;
    let CustomListState {
        definitions,
        filters: custom_filters,
    } = custom;
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
    let filterable: Vec<&Definition> = definitions
        .iter()
        .filter(|definition| {
            !definition.archived
                && !matches!(definition.field_type, FieldType::Reference)
                && !masked.contains(&format!("cf_{}", definition.key))
        })
        .collect();
    if !filterable.is_empty() && custom_filters.len() < 10 {
        actions.push(&render(&Button {
            id: "companies-add-custom-filter",
            event: "add_cf_filter",
            body: Html::text("Add custom filter"),
            ..Button::default()
        })?);
    }
    for index in 0..custom_filters.len() {
        let id = format!("companies-remove-custom-filter-{index}");
        let label = format!("Remove filter {}", index + 1);
        let value = index.to_string();
        actions.push(&render(&Button {
            id: &id,
            event: "remove_cf_filter",
            name: "index",
            value: &value,
            body: Html::text(&label),
            ..Button::default()
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
    let mut controls = vec![
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
    ];
    for (index, filter) in custom_filters.iter().enumerate() {
        let field_key = format!("cf_field_{index}");
        let operator_key = format!("cf_operator_{index}");
        let value_key = format!("cf_value_{index}");
        let fields = filterable
            .iter()
            .map(|definition| Choice::new(&definition.label, format!("cf_{}", definition.key)))
            .collect();
        controls.push(ToolbarControl::select(
            &format!("companies-cf-field-{index}"),
            &field_key,
            "Custom field",
            &filter.field,
            fields,
        ));
        let definition = filterable
            .iter()
            .find(|definition| format!("cf_{}", definition.key) == filter.field);
        let operators: Vec<(&str, &str)> = definition.map_or(
            vec![("Contains", "contains"), ("Equals", "equals")],
            |definition| match definition.field_type {
                FieldType::Text | FieldType::LongText => {
                    vec![("Contains", "contains"), ("Equals", "equals")]
                }
                FieldType::Integer | FieldType::Decimal | FieldType::Date | FieldType::DateTime => {
                    vec![
                        ("Equals", "eq"),
                        ("Greater than", "gt"),
                        ("At least", "gte"),
                        ("Less than", "lt"),
                        ("At most", "lte"),
                    ]
                }
                FieldType::Boolean | FieldType::Choice => vec![("Equals", "equals")],
                FieldType::MultiChoice => vec![("Contains", "contains")],
                FieldType::Reference => vec![],
            },
        );
        controls.push(ToolbarControl::select(
            &format!("companies-cf-operator-{index}"),
            &operator_key,
            "Operator",
            &filter.operator,
            choices(&operators),
        ));
        if let Some(definition) = definition.filter(|definition| {
            matches!(
                definition.field_type,
                FieldType::Choice | FieldType::MultiChoice | FieldType::Boolean
            )
        }) {
            let options = if definition.field_type == FieldType::Boolean {
                choices(&[("Yes", "true"), ("No", "false")])
            } else {
                definition
                    .options
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        Some(Choice::new(
                            option.get("label")?.as_str()?,
                            option.get("code")?.as_str()?,
                        ))
                    })
                    .collect()
            };
            controls.push(ToolbarControl::select(
                &format!("companies-cf-value-{index}"),
                &value_key,
                "Value",
                &filter.value,
                options,
            ));
        } else if definition.is_some_and(|definition| definition.field_type == FieldType::Date) {
            controls.push(ToolbarControl::date(
                &format!("companies-cf-value-{index}"),
                &value_key,
                "Value",
                &filter.value,
            ));
        } else {
            controls.push(ToolbarControl::search(
                &format!("companies-cf-value-{index}"),
                &value_key,
                "Value",
                &filter.value,
                "Filter value",
            ));
        }
    }
    let mut toolbar = render(&FilterToolbar::new("companies-filters", controls))?;
    if !warning.is_empty() {
        toolbar.push(&render(&Alert {
            kind: StatusKind::Warning,
            id: "companies-custom-filter-warning",
            body: Html::text(warning),
            ..Alert::default()
        })?);
    }
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
    let filtered =
        !params.q.is_empty() || !params.status.is_empty() || !params.custom_filters.is_empty();
    let title = if filtered {
        format!(
            "No {}companies{}",
            if params.status.is_empty() {
                String::new()
            } else {
                format!("{} ", params.status)
            },
            if params.q.is_empty() {
                if params.custom_filters.is_empty() {
                    String::new()
                } else {
                    " with custom filters".into()
                }
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
                .wrapper_class("mb-0"),
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

fn custom_fact(
    company: &Company,
    definition: &Definition,
    value: Option<&Value>,
    editable: bool,
    status: CommitStatus,
    reference_targets: &BTreeMap<String, Vec<ReferenceOption>>,
) -> Result<Html> {
    let id = format!("company-custom-{}", definition.key);
    let name = format!("cf_{}", definition.key);
    let wire = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(value @ Value::Array(_)) => serde_json::to_string(value).unwrap_or_default(),
        Some(_) => String::new(),
    };
    let active_choices = custom_options(
        definition,
        &value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(value.and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    let reference_choices = definition
        .reference_table
        .as_ref()
        .and_then(|table| reference_targets.get(table))
        .map(|targets| {
            targets
                .iter()
                .map(|target| Choice::new(target.label.clone(), target.key.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let reference_readable = definition.field_type != FieldType::Reference
        || value
            .and_then(Value::as_str)
            .is_none_or(|key| reference_choices.iter().any(|choice| choice.value == key));
    let wire = if definition.field_type == FieldType::Reference && !reference_readable {
        String::new()
    } else {
        wire
    };
    let display = match definition.field_type {
        FieldType::Boolean => match value.and_then(Value::as_bool) {
            Some(true) => "Yes".to_owned(),
            Some(false) => "No".to_owned(),
            None => "—".to_owned(),
        },
        FieldType::Choice => {
            let code = value.and_then(Value::as_str).unwrap_or_default();
            active_choices
                .iter()
                .find(|choice| choice.value == code)
                .map(|choice| choice.label.clone())
                .unwrap_or_else(|| code.to_owned())
        }
        FieldType::MultiChoice => value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|code| {
                active_choices
                    .iter()
                    .find(|choice| choice.value == code)
                    .map_or_else(|| code.to_owned(), |choice| choice.label.clone())
            })
            .collect::<Vec<_>>()
            .join(", "),
        FieldType::Reference => {
            if value.is_none_or(Value::is_null) {
                "—".into()
            } else if reference_readable {
                reference_choices
                    .iter()
                    .find(|choice| Some(choice.value.as_str()) == value.and_then(Value::as_str))
                    .map(|choice| choice.label.clone())
                    .unwrap_or_else(|| "Unavailable".into())
            } else {
                "Unavailable".into()
            }
        }
        _ => wire.clone(),
    };
    let display_html = if display.is_empty() {
        render(&MutedValue { text: "—" })?
    } else {
        Html::text(&display)
    };
    let mut inline = InlineField::new(&id, &name, format!("Edit {}", definition.label), &wire)
        .version(company.version)
        .status(status)
        .hint(definition.help_text.clone().unwrap_or_default());
    inline.id_value = company.id.to_string();
    inline.editable = editable;
    inline.allow_empty = !definition.required;
    match definition.field_type {
        FieldType::LongText => render(&InlineLongText {
            field: inline,
            rows: 4,
        }),
        FieldType::MultiChoice => {
            let selected = value
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let editor = render(
                &Field::named(&name)
                    .id(format!("{id}-input"))
                    .label(&definition.label)
                    .kind(InputKind::Select)
                    .multiple()
                    .values(selected)
                    .options(active_choices)
                    .aria_label(&definition.label)
                    .wrapper_class("mb-0"),
            )?;
            render(&InlineMultiChoice {
                field: inline,
                display: display_html,
                editor,
            })
        }
        FieldType::Choice | FieldType::Boolean | FieldType::Date | FieldType::Reference => {
            if definition.field_type == FieldType::Reference {
                let mut options = reference_choices;
                if !definition.required {
                    options.insert(0, Choice::new("None", ""));
                }
                let editor = render(
                    &Combobox::new(format!("{id}-input"), &name)
                        .label(&definition.label)
                        .value(&wire)
                        .options(options)
                        .placeholder(if definition.required {
                            "Choose a company..."
                        } else {
                            "None"
                        })
                        .aria_label(&definition.label)
                        .hint(definition.help_text.clone().unwrap_or_default())
                        .wrapper_class("mb-0"),
                )?;
                return render(&InlineChoice {
                    field: inline,
                    display: display_html,
                    editor,
                });
            }
            let options = if definition.field_type == FieldType::Boolean {
                choices(&[("Yes", "true"), ("No", "false")])
            } else {
                active_choices
            };
            let editor = match definition.field_type {
                FieldType::Date => render(
                    &Field::named(&name)
                        .id(format!("{id}-input"))
                        .label(&definition.label)
                        .kind(InputKind::Date)
                        .value(&wire)
                        .aria_label(&definition.label)
                        .wrapper_class("mb-0"),
                )?,
                _ => {
                    let mut field = Field::named(&name)
                        .id(format!("{id}-input"))
                        .label(&definition.label)
                        .kind(InputKind::Select)
                        .aria_label(&definition.label)
                        .options(options.clone())
                        .wrapper_class("mb-0");
                    field = field.value(&wire);
                    if !definition.required && definition.field_type != FieldType::Boolean {
                        field = field.prompt("Choose an option...");
                    }
                    render(&field)?
                }
            };
            render(&InlineChoice {
                field: inline,
                display: display_html,
                editor,
            })
        }
        FieldType::Text | FieldType::Integer | FieldType::Decimal | FieldType::DateTime => {
            if definition.field_type == FieldType::DateTime && inline.hint.is_empty() {
                inline.hint = "Enter an ISO 8601 UTC timestamp.".into();
            }
            render(&InlineEdit { field: inline })
        }
    }
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
    pub custom_fields: &'a [Definition],
    pub custom_reference_targets: &'a BTreeMap<String, Vec<ReferenceOption>>,
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
        custom_fields,
        custom_reference_targets,
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
    for (name, value) in [
        ("name", company.name.as_str()),
        ("code", company.code.as_str()),
        ("legal_name", company.legal_name.as_deref().unwrap_or("")),
    ] {
        facts.push(Fact {
            title: fields::get(name).label,
            id: "",
            value: text_fact(&company, name, value, editable, false, statuses.get(name))?,
        });
    }
    facts.push(Fact {
        title: fields::STATUS.label,
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
        title: fields::LEGAL_ENTITY_TYPE_ID.label,
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
    for (name, value) in [
        (
            "registration_number",
            company.registration_number.as_deref().unwrap_or(""),
        ),
        ("tax_id", company.tax_id.as_str()),
    ] {
        facts.push(Fact {
            title: fields::get(name).label,
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
        title: fields::JURISDICTION.label,
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
    for (name, value) in [
        ("email", company.email.as_str()),
        ("website", company.website.as_deref().unwrap_or("")),
    ] {
        facts.push(Fact {
            title: fields::get(name).label,
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
        title: fields::PARENT_ID.label,
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
    // Company custom facts mirror Bilimbi Core Company's record facts.
    for definition in custom_fields.iter() {
        let field_id = format!("cf_{}", definition.key);
        let id = format!("company-custom-{}", definition.key);
        let value = company.custom.get(&definition.key);
        let body = if masked.contains(&field_id) {
            restricted(&id)?
        } else {
            custom_fact(
                &company,
                definition,
                value,
                editable,
                statuses.get(&field_id),
                custom_reference_targets,
            )?
        };
        facts.push(Fact {
            title: &definition.label,
            id: "",
            value: body,
        });
    }
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
                title: fields::get(name).label,
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
            title: fields::get(name).label,
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
            crate::lifecycle::Operation::Archive => {
                "Its records are kept. Archiving is final: an archived company cannot be reactivated."
            }
            crate::lifecycle::Operation::Suspend => {
                "Its records are kept. Reactivating the company lifts the suspension."
            }
            crate::lifecycle::Operation::Activate => {
                "It becomes a working company of this workspace. It can be suspended later."
            }
            crate::lifecycle::Operation::Reactivate => {
                "The suspension is lifted and the company is active again."
            }
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
    custom: Vec<Html>,
    custom_heading: Html,
    submit: Html,
}
pub(super) async fn create(
    client: &impl GenericClient,
    scope: &Scope,
    values: &HashMap<String, String>,
    errors: &[FieldError],
    custom_fields: &[Definition],
    reference_targets: &BTreeMap<String, Vec<ReferenceOption>>,
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
    // Creation controls mirror Bilimbi Core Company's NewLive form.
    let custom = custom_fields
        .iter()
        .map(|definition| {
            let id = format!("company-custom-{}", definition.key);
            if masked
                .get("companies")
                .is_some_and(|fields| fields.contains(&format!("cf_{}", definition.key)))
            {
                render(&RestrictedField::new(&id, &definition.label))
            } else {
                custom_create_field(definition, values, errors, reference_targets)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let custom_heading = render(&SectionHeading {
        id: "company-create-custom-fields-heading",
        title: "Custom fields",
        ..SectionHeading::default()
    })?;
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
            custom,
            custom_heading,
            submit,
        })?,
    )
}
pub(super) fn new_company(
    values: &HashMap<String, String>,
    custom_fields: &[Definition],
) -> (NewCompany, Option<Value>, Option<Value>, Vec<FieldError>) {
    let text = |name| values.get(name).filter(|s| !s.trim().is_empty()).cloned();
    let mut errors = Vec::new();
    let custom = custom_values(values, custom_fields, &mut errors);
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
        custom,
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

fn custom_form_name(key: &str, multiple: bool) -> String {
    format!("custom[{key}]{}", if multiple { "[]" } else { "" })
}

fn form_custom_value(values: &HashMap<String, String>, definition: &Definition) -> Value {
    let name = custom_form_name(
        &definition.key,
        definition.field_type == FieldType::MultiChoice,
    );
    let Some(raw) = values.get(&name) else {
        return Value::Null;
    };
    if definition.field_type == FieldType::Reference {
        return Value::String(raw.clone());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
}

fn custom_error_messages(errors: &[FieldError], key: &str) -> Vec<String> {
    let field = format!("custom.{key}");
    errors
        .iter()
        .filter(|error| error.field == field || error.field == "custom")
        .map(|error| error.message.clone())
        .collect()
}

fn custom_options(definition: &Definition, include_retired: &[String]) -> Vec<Choice> {
    definition
        .options
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| {
            let code = option.get("code")?.as_str()?;
            let label = option.get("label")?.as_str()?;
            let active = option
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            (active || include_retired.iter().any(|value| value == code))
                .then(|| Choice::new(label, code))
        })
        .collect()
}

fn custom_create_field(
    definition: &Definition,
    values: &HashMap<String, String>,
    errors: &[FieldError],
    reference_targets: &BTreeMap<String, Vec<ReferenceOption>>,
) -> Result<Html> {
    let name = custom_form_name(
        &definition.key,
        definition.field_type == FieldType::MultiChoice,
    );
    let id = format!("company-custom-{}", definition.key);
    let value = form_custom_value(values, definition);
    if definition.field_type == FieldType::Reference {
        let mut options: Vec<Choice> = definition
            .reference_table
            .as_ref()
            .and_then(|table| reference_targets.get(table))
            .into_iter()
            .flatten()
            .map(|target| Choice::new(target.label.clone(), target.key.clone()))
            .collect();
        if !definition.required {
            options.insert(0, Choice::new("None", ""));
        }
        let selected = value
            .as_str()
            .filter(|key| options.iter().any(|option| option.value == *key))
            .unwrap_or_default();
        let mut hint = definition.help_text.clone().unwrap_or_default();
        if options.is_empty() {
            if !hint.is_empty() {
                hint.push(' ');
            }
            hint.push_str("No readable targets are available.");
        }
        return render(
            &Combobox::new(&id, &name)
                .label(&definition.label)
                .value(selected)
                .options(options)
                .placeholder(if definition.required {
                    "Choose a company..."
                } else {
                    "None"
                })
                .errors(custom_error_messages(errors, &definition.key))
                .hint(hint)
                .attrs(Attrs {
                    required: definition.required,
                    ..Attrs::default()
                }),
        );
    }
    let mut field = Field::named(&name)
        .id(&id)
        .label(&definition.label)
        .hint(definition.help_text.clone().unwrap_or_default())
        .errors(custom_error_messages(errors, &definition.key));
    field = match definition.field_type {
        FieldType::Text => field
            .kind(InputKind::Text)
            .value(value.as_str().unwrap_or_default())
            .attr(
                "maxlength",
                &definition.text_limit.unwrap_or(255).to_string(),
            ),
        FieldType::LongText => field
            .kind(InputKind::Textarea)
            .value(value.as_str().unwrap_or_default())
            .attr(
                "maxlength",
                &definition.text_limit.unwrap_or(100_000).to_string(),
            ),
        FieldType::Integer => field
            .kind(InputKind::Number)
            .value(
                value
                    .as_i64()
                    .map(|v| v.to_string())
                    .or_else(|| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            )
            .attr("step", "1"),
        FieldType::Decimal => field
            .kind(InputKind::Number)
            .value(value.as_str().map(str::to_owned).unwrap_or_default())
            .attr("step", "any"),
        FieldType::Date => field
            .kind(InputKind::Date)
            .value(value.as_str().unwrap_or_default()),
        FieldType::DateTime => field
            .kind(InputKind::Text)
            .value(value.as_str().unwrap_or_default())
            .placeholder("2026-10-23T12:00:00Z")
            .hint(
                definition
                    .help_text
                    .clone()
                    .unwrap_or_else(|| "Enter an ISO 8601 UTC timestamp.".into()),
            ),
        FieldType::Boolean => field.kind(InputKind::Checkbox).checked(
            value
                .as_bool()
                .unwrap_or_else(|| value.as_str() == Some("true")),
        ),
        FieldType::Choice => field
            .kind(InputKind::Select)
            .prompt("Choose an option...")
            .value(value.as_str().unwrap_or_default())
            .options(custom_options(definition, &[])),
        FieldType::MultiChoice => field
            .kind(InputKind::Select)
            .multiple()
            .values(
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            )
            .options(custom_options(definition, &[])),
        FieldType::Reference => return Ok(Html::EMPTY),
    };
    if definition.required && definition.field_type != FieldType::Boolean {
        field = field.required();
    }
    render(&field)
}

fn custom_values(
    values: &HashMap<String, String>,
    definitions: &[Definition],
    errors: &mut Vec<FieldError>,
) -> Value {
    let mut custom = serde_json::Map::new();
    for (name, raw) in values {
        let Some(inner) = name.strip_prefix("custom[") else {
            continue;
        };
        let (key, multiple) = if let Some(key) = inner.strip_suffix("][]") {
            (key, true)
        } else if let Some(key) = inner.strip_suffix(']') {
            (key, false)
        } else {
            continue;
        };
        let value =
            serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.clone()));
        let Some(definition) = definitions.iter().find(|definition| definition.key == key) else {
            custom.insert(key.to_owned(), value);
            continue;
        };
        let parsed = if value.as_str() == Some("")
            && !matches!(definition.field_type, FieldType::Text | FieldType::LongText)
        {
            Value::Null
        } else {
            match definition.field_type {
                FieldType::Text
                | FieldType::LongText
                | FieldType::Decimal
                | FieldType::Date
                | FieldType::DateTime
                | FieldType::Choice => {
                    if value.is_null() || (multiple && value.as_array().is_some_and(Vec::is_empty))
                    {
                        Value::Null
                    } else {
                        Value::String(
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| raw.clone()),
                        )
                    }
                }
                FieldType::Integer => match value.as_str().unwrap_or(raw).parse::<i64>() {
                    Ok(number) => Value::Number(number.into()),
                    Err(_) => {
                        errors.push(FieldError::new(
                            format!("custom.{key}"),
                            "must be a whole number",
                        ));
                        Value::Null
                    }
                },
                FieldType::Boolean => match value.as_str().unwrap_or(raw) {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => {
                        errors.push(FieldError::new(
                            format!("custom.{key}"),
                            "must be yes or no",
                        ));
                        Value::Null
                    }
                },
                FieldType::MultiChoice => {
                    if !multiple || !value.is_array() {
                        errors.push(FieldError::new(
                            format!("custom.{key}"),
                            "has invalid choices",
                        ));
                        Value::Array(Vec::new())
                    } else {
                        value
                    }
                }
                FieldType::Reference => Value::String(
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| raw.clone()),
                ),
            }
        };
        custom.insert(key.to_owned(), parsed);
    }
    Value::Object(custom)
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
