//! Company owns its page state, Grid arrangement and all business events:
//! the list/create/detail pages here, the two reference catalogs in
//! `reference_types`, a company's departments in `departments` and the
//! relationship-role catalog, relationship page and panel in `relationships`
//! the platform-operator setup page in `setup` and the two dashboard entries
//! in `dashboard`.
mod dashboard;
mod departments;
mod reference_types;
pub(super) mod relationships;
mod setup;
mod views;
use crate::{lifecycle::Operation, CreateOutcome, Edit, FieldError, Params};
use bilibi_database::{Db, Error, Result};
use bilibi_grid::{PageColumns, PageViews};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, CommitStates, CommitStatus, Filter, FlashMap, ListColumns, ListSpec, ListState,
    SortDir, StatusKind, NO_FLASH,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};
pub(crate) use views::{allowed, render};

/// The page's one flash message, by kind (Bilimbi `put_flash`/`clear_flash`),
/// rendered by the page's own `flash_group`.
#[derive(Default)]
pub(crate) struct Flash(Option<(StatusKind, String)>);
impl Flash {
    pub fn put(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.0 = Some((kind, message.into()));
    }
    pub fn clear(&mut self) {
        self.0 = None;
    }
    /// The message for the page a redirect lands on (`LiveBody.flash`).
    pub fn take(&mut self) -> Option<(StatusKind, String)> {
        self.0.take()
    }
    /// The browser dismissed the message of this kind.
    pub fn dismiss(&mut self, key: Option<&str>) {
        if self
            .0
            .as_ref()
            .is_some_and(|(kind, _)| Some(kind.name()) == key)
        {
            self.0 = None;
        }
    }
    pub fn map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }
}

pub struct Pages {
    pub db: Db,
    pub company: Arc<crate::CompanyService>,
    pub views: Arc<PageViews>,
    pub datetime: Arc<bilibi_datetime::DisplayPolicy>,
    pub locales: Arc<bilibi_locale::Locales>,
    pub record_panels: Vec<RecordPanel>,
}
/// A panel another module contributes for a company record (Address's
/// `company.addresses`), discovered so Company imports none of them.
#[derive(Clone)]
pub struct RecordPanel {
    anchor: String,
    embed: &'static str,
    access: bilibi_module_registry::contributions::Access,
}
/// Embeds whose context is `Record { kind: "core/company" }`. The host
/// reauthorizes each on mount, event and refresh; this only decides whether
/// to place its anchor.
pub fn record_panels() -> bilibi_module_registry::Result<Vec<RecordPanel>> {
    use bilibi_module_registry::contributions::{Embed, EmbedContext, EMBEDS};
    let mut panels = Vec::new();
    for entry in inventory::iter::<bilibi_module_registry::Contribution>
        .into_iter()
        .filter(|entry| entry.consumer == EMBEDS)
    {
        let embed = entry.get::<Embed>()?;
        if matches!(
            embed.context,
            EmbedContext::Record {
                kind: "core/company"
            }
        ) {
            panels.push(RecordPanel {
                anchor: format!("company-panel-{}", entry.id.replace(['.', '/'], "-")),
                embed: entry.id,
                access: embed.access,
            });
        }
    }
    panels.sort_by(|a, b| a.embed.cmp(b.embed));
    Ok(panels)
}
fn record_id(request: &PageRequest<'_, Scope>) -> Result<i64> {
    request
        .path
        .get("id")
        .and_then(|v| v.parse().ok())
        .filter(|id: &i64| *id > 0)
        .ok_or(Error::Invalid)
}
impl Pages {
    /// The state of a page other than list/create/detail, by operation.
    fn other(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Option<Box<dyn LiveState<Scope, Error>>>> {
        Ok(Some(match request.operation {
            "legal_entity_types" => Box::new(reference_types::ReferenceTypes::new(
                self.db.clone(),
                self.company.clone(),
                &reference_types::LEGAL_ENTITY_TYPES,
            )?),
            "department_types" => Box::new(reference_types::ReferenceTypes::new(
                self.db.clone(),
                self.company.clone(),
                &reference_types::DEPARTMENT_TYPES,
            )?),
            "platform_operator_setup" => {
                Box::new(setup::Setup::new(self.db.clone(), self.company.clone()))
            }
            "dashboard_companies" => Box::new(dashboard::Stats::new(self.db.clone(), request)?),
            "dashboard_company" => {
                Box::new(dashboard::CurrentCompany::new(self.db.clone(), request)?)
            }
            "departments" => Box::new(departments::Departments::new(
                self.db.clone(),
                self.company.clone(),
                record_id(request)?,
            )?),
            _ => return Ok(None),
        }))
    }
    async fn state(&self, request: &PageRequest<'_, Scope>) -> Result<CompanyPage> {
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        let id = match request.operation {
            "list" | "create" => None,
            "detail" => Some(record_id(request)?),
            _ => return Err(Error::NotFound),
        };
        let client = self.db.get().await?;
        let masked = bilibi_authz::restrictions(&**client, scope).await?;
        let mut columns = views::columns(self.views.clone(), &masked)?;
        columns.from_params(scope, request.query).await?;
        Ok(CompanyPage {
            db: self.db.clone(),
            company: self.company.clone(),
            datetime: self.datetime.clone(),
            locales: self.locales.clone(),
            operation: request.operation,
            id,
            params: list_state(request.query)?,
            columns,
            framed: request.framed,
            statuses: CommitStates::default(),
            views: self.views.clone(),
            masked: masked.clone(),
            departments_columns: ListColumns::mount(
                "company-departments-table",
                views::department_builtins(),
            )
            .map_err(|_| Error::Internal)?,
            children_columns: ListColumns::mount(
                "company-subsidiaries",
                views::children_builtins(),
            )
            .map_err(|_| Error::Internal)?,
            form: HashMap::from([("status".into(), "active".into())]),
            errors: Vec::new(),
            duplicate_candidates: Vec::new(),
            duplicate_columns: ListColumns::mount(
                "company-duplicates",
                views::duplicate_builtins(),
            )
            .map_err(|_| Error::Internal)?,
            duplicate_reason: String::new(),
            confirming_duplicate: false,
            pending: None,
            pending_activity: None,
            history: false,
            record_panels: self.record_panels.clone(),
            active_panels: Vec::new(),
            notice: String::new(),
            landing: Flash::default(),
        })
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "create" => "Create Company",
            "detail" => "Company",
            "legal_entity_types" => "Legal Entity Types",
            "department_types" => "Department Types",
            "departments" => "Departments",
            "platform_operator_setup" => "Set Up Platform Operator",
            _ => "Companies",
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if let Some(mut other) = self.other(&request)? {
                let scope = request.scope.ok_or(Error::Unauthorized)?;
                let body = other
                    .update(LiveRequest {
                        scope,
                        credential: "",
                        input: LiveInput::Refresh,
                    })
                    .await?;
                return Ok(PageBody {
                    html: body.html,
                    live: true,
                    title: None,
                });
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(&request).await?;
            let body = state.draw(scope).await?;
            // The detail page is titled by the company it shows, as Bilimbi's
            // `page_title` is the company's display name.
            let title = if request.operation == "detail" {
                let client = self.db.get().await?;
                let (company, _) = crate::detail(&**client, scope, record_id(&request)?).await?;
                Some(company.display_name())
            } else {
                None
            };
            Ok(PageBody {
                html: body.html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if let Some(other) = self.other(&request)? {
                return Ok(Some(other));
            }
            Ok(Some(
                Box::new(self.state(&request).await?) as Box<dyn LiveState<Scope, Error>>
            ))
        })
    }
}

static LIST_SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("status".into(), SortDir::Asc),
            ("jurisdiction".into(), SortDir::Asc),
            ("created_at".into(), SortDir::Asc),
        ]),
        default_sort: Some("name".into()),
        page_size_param: "per_page".into(),
        page_size_aliases: vec!["size".into(), "perPage".into()],
        invalid_page_size: bilibi_ui::list_state::InvalidPageSize::Default,
        param_names: bilibi_ui::list_state::ParamNames {
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
            ..Default::default()
        },
        filters: BTreeMap::from([(
            "status".into(),
            Filter::one_of(&["", "active", "suspended", "pending", "archived"], ""),
        )]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("company list specification")
});
fn list_state(values: &HashMap<String, String>) -> Result<ListState> {
    let mut values = values.clone();
    if !values.contains_key("search") {
        if let Some(q) = values.get("q").cloned() {
            values.insert("search".into(), q);
        }
    }
    if !values.contains_key("status") {
        if let Some(status) = values.get("status_filter").cloned() {
            values.insert("status".into(), status);
        }
    }
    ListState::parse(&values, LIST_SPEC.clone()).map_err(|_| Error::Internal)
}
fn query_params(state: &ListState) -> Params {
    Params {
        page: state.page as i64,
        size: state.page_size as i64,
        sort: state.sort_by.clone().unwrap_or_else(|| "name".into()),
        dir: state.sort_dir.unwrap_or_default().key().into(),
        q: state.search.clone(),
        status: state.filters.get("status").cloned().unwrap_or_default(),
    }
}
struct Pending {
    operation: Operation,
    version: i64,
    reason: String,
    error: String,
}
struct CompanyPage {
    db: Db,
    company: Arc<crate::CompanyService>,
    datetime: Arc<bilibi_datetime::DisplayPolicy>,
    locales: Arc<bilibi_locale::Locales>,
    operation: &'static str,
    id: Option<i64>,
    params: ListState,
    columns: PageColumns,
    framed: bool,
    statuses: CommitStates,
    views: Arc<PageViews>,
    masked: std::collections::HashSet<String>,
    /// The detail page's Departments table arrangement (Bilimbi
    /// `departments_columns`).
    departments_columns: ListColumns,
    /// Bilimbi Company.ShowLive's independent `children_grid`.
    children_columns: ListColumns,
    form: HashMap<String, String>,
    errors: Vec<FieldError>,
    duplicate_candidates: Vec<crate::DuplicateCandidate>,
    duplicate_columns: ListColumns,
    duplicate_reason: String,
    confirming_duplicate: bool,
    pending: Option<Pending>,
    pending_activity: Option<(String, i64)>,
    history: bool,
    record_panels: Vec<RecordPanel>,
    active_panels: Vec<PanelSpec>,
    notice: String,
    /// What the page a create redirects to shows.
    landing: Flash,
}
impl CompanyPage {
    /// The contributed record panels this reader may mount.
    async fn allowed_panels(
        &self,
        client: &impl tokio_postgres::GenericClient,
        scope: &Scope,
        id: i64,
    ) -> Result<Vec<PanelSpec>> {
        use bilibi_module_registry::contributions::Access;
        let mut active = Vec::new();
        for panel in &self.record_panels {
            let allowed = match panel.access {
                Access::Capability(key) => {
                    match bilibi_authz::authorize_key(client, scope, key).await {
                        Ok(()) => true,
                        Err(Error::Forbidden) => false,
                        Err(error) => return Err(error),
                    }
                }
                Access::SignedIn => true,
                _ => false,
            };
            if allowed {
                active.push(PanelSpec {
                    id: panel.anchor.clone(),
                    embed: panel.embed.into(),
                    path: [
                        ("panel".into(), panel.anchor.clone()),
                        ("id".into(), id.to_string()),
                        ("company_id".into(), id.to_string()),
                        ("kinds".into(), "company".into()),
                    ]
                    .into(),
                    query: HashMap::new(),
                });
            }
        }
        Ok(active)
    }
    fn query(&self) -> HashMap<String, String> {
        let mut values = self.params.to_params();
        values.extend(self.columns.params());
        values
    }
    async fn draw(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.db.get().await?;
        let masked = bilibi_authz::restrictions(&**client, scope).await?;
        let html = match self.operation {
            "list" => {
                if self
                    .params
                    .sort_by
                    .as_ref()
                    .is_some_and(|key| masked.contains(key))
                {
                    self.params.sort_by = Some("name".into());
                    self.params.sort_dir = Some(SortDir::Asc);
                }
                let mut page = crate::list(&**client, scope, &query_params(&self.params)).await?;
                let last = ((page.total + self.params.page_size as i64 - 1)
                    / self.params.page_size as i64) as u64;
                if self.params.clamp_to_last_page(last, true) {
                    page = crate::list(&**client, scope, &query_params(&self.params)).await?;
                }
                // Rebuild built-ins under the fresh restrictions too: a retained
                // built-in must never outlive its reader's field access.
                if masked != self.masked {
                    let query = self.columns.params();
                    self.columns = views::columns(self.views.clone(), &masked)?;
                    self.columns.from_params(scope, &query).await?;
                    self.masked = masked.clone();
                }
                let locale = self.locales.resolve(&**client, scope, None).await?;
                let display = self
                    .datetime
                    .display(&**client, scope, None, locale.locale)
                    .await?;
                self.columns
                    .load(
                        scope,
                        &page
                            .rows
                            .iter()
                            .map(|row| serde_json::json!(row.id))
                            .collect::<Vec<_>>(),
                        &display,
                    )
                    .await?;
                views::list(
                    &**client,
                    scope,
                    &page,
                    &query_params(&self.params),
                    &self.columns,
                    self.framed,
                    &masked,
                )
                .await?
            }
            "detail" => {
                self.history =
                    match bilibi_authz::authorize_key(&**client, scope, "admin.audit.log.list")
                        .await
                    {
                        Ok(()) => true,
                        Err(Error::Forbidden) => false,
                        Err(error) => return Err(error),
                    };
                let id = self.id.ok_or(Error::Invalid)?;
                self.active_panels = self.allowed_panels(&**client, scope, id).await?;
                let anchors: Vec<String> =
                    self.active_panels.iter().map(|p| p.id.clone()).collect();
                views::detail(
                    &**client,
                    scope,
                    id,
                    views::DetailState {
                        statuses: &self.statuses,
                        pending: self.pending.as_ref(),
                        notice: &self.notice,
                        pending_activity: self.pending_activity.as_ref(),
                        departments_columns: &mut self.departments_columns,
                        children_columns: &self.children_columns,
                        workspace: self.framed,
                        panels: &anchors,
                    },
                )
                .await?
            }
            "create" => {
                views::create(
                    &**client,
                    scope,
                    views::CreateState {
                        values: &self.form,
                        errors: &self.errors,
                        candidates: &self.duplicate_candidates,
                        columns: &self.duplicate_columns,
                        confirming: self.confirming_duplicate,
                        reason: &self.duplicate_reason,
                    },
                )
                .await?
            }
            _ => return Err(Error::NotFound),
        };
        Ok(LiveBody {
            html,
            query: if self.operation == "list" {
                self.query()
            } else {
                HashMap::new()
            },
            notice: self.notice.clone(),
            rows: 1,
            ..LiveBody::default()
        })
    }
    fn sort(&mut self, field: &str) -> Result<()> {
        if self.operation != "list" || !self.params.next_sort(field) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<Option<String>> {
        let get = |key| value.get(key).and_then(Value::as_str).unwrap_or("");
        match get("type") {
            "grid" if self.operation == "list" => {
                let values = value
                    .as_object()
                    .ok_or(Error::Invalid)?
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.into())))
                    .collect();
                if let bilibi_grid::page_columns::Outcome::Sort(field) =
                    self.columns.handle(scope, &values).await?
                {
                    self.sort(&field)?;
                }
            }
            "departments_grid" if self.operation == "detail" => {
                self.departments_columns
                    .handle(&bilibi_ui::params::string_values(value));
            }
            "children_grid" if self.operation == "detail" => {
                self.children_columns
                    .handle(&bilibi_ui::params::string_values(value));
            }
            "sort" => self.sort(if get("sort").is_empty() {
                get("field")
            } else {
                get("sort")
            })?,
            "filters" if self.operation == "list" => {
                let mut posted = HashMap::new();
                for (key, field) in [
                    ("filters[search]", "search"),
                    ("filters[status_filter]", "status"),
                    ("filters[perPage]", "perPage"),
                ] {
                    if let Some(value) = value.get(key).and_then(Value::as_str) {
                        posted.insert(field.into(), value.into());
                    }
                }
                self.params.apply_filters(&posted);
            }
            "page" if self.operation == "list" => self.params.put_page(Some(get("page"))),
            "clear_filters" if self.operation == "list" => {
                self.params.apply_filters(&HashMap::from([
                    ("search".into(), "".into()),
                    ("status".into(), "".into()),
                ]))
            }
            "save_field" if self.operation == "detail" => {
                let id = get("id").parse().map_err(|_| Error::Invalid)?;
                if self.id != Some(id) {
                    return Err(Error::Invalid);
                }
                let edit = Edit {
                    id,
                    field: get("field").into(),
                    value: get("value").into(),
                    version: get("version").parse().map_err(|_| Error::Invalid)?,
                };
                let result = self.company.save_fact(scope, credential, &edit).await;
                let status = match result {
                    Ok(()) => CommitStatus::Saved,
                    Err(crate::WriteError::Access(Error::Unauthorized)) => {
                        return Err(Error::Unauthorized)
                    }
                    Err(crate::WriteError::Access(Error::Forbidden)) => {
                        self.notice="You no longer have permission to change this company, or the company is archived.".into();
                        CommitStatus::Error(self.notice.clone())
                    }
                    Err(crate::WriteError::Access(Error::Conflict)) => CommitStatus::Error(
                        "The record changed. Your draft is kept; reload before saving.".into(),
                    ),
                    Err(crate::WriteError::Access(Error::Invalid)) => CommitStatus::Error(
                        "The value was not saved. Check its format, length and references.".into(),
                    ),
                    Err(crate::WriteError::Invalid(errors)) => CommitStatus::Error(
                        errors
                            .into_iter()
                            .map(|e| e.message)
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    Err(_) => CommitStatus::failure(),
                };
                self.statuses.put(edit.field, status);
            }
            "add_activity" | "request_remove_activity" | "remove_activity"
                if self.operation == "detail" =>
            {
                let id = self.id.ok_or(Error::Invalid)?;
                let client = self.db.get().await?;
                let row=client.query_one("SELECT scope_activities,version FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL", &[&scope.tenant(),&id]).await?;
                let value: Option<Value> = row.get(0);
                let version: i64 = row.get(1);
                let mut activities = match value {
                    None => Vec::new(),
                    Some(Value::Array(a)) => a,
                    _ => return Err(Error::Invalid),
                };
                if get("type") == "request_remove_activity" {
                    crate::organisation::authorize_target(&**client, scope, id, crate::UPDATE)
                        .await?;
                    let index: usize = get("index").parse().map_err(|_| Error::Invalid)?;
                    let activity = activities
                        .get(index)
                        .and_then(Value::as_str)
                        .ok_or(Error::Invalid)?;
                    self.pending_activity = Some((activity.into(), version));
                } else {
                    let expected = if get("type") == "add_activity" {
                        let activity = get("value").trim();
                        if activity.is_empty() || activity.chars().count() > 255 {
                            return Err(Error::Invalid);
                        }
                        let activity = Value::String(activity.into());
                        if !activities.contains(&activity) {
                            activities.push(activity);
                        }
                        get("version").parse().map_err(|_| Error::Invalid)?
                    } else {
                        let (activity, version) =
                            self.pending_activity.take().ok_or(Error::Invalid)?;
                        activities.retain(|v| v.as_str() != Some(&activity));
                        version
                    };
                    let edit = Edit {
                        id,
                        field: "scope_activities".into(),
                        value: if activities.is_empty() {
                            String::new()
                        } else {
                            serde_json::to_string(&activities).map_err(|_| Error::Internal)?
                        },
                        version: expected,
                    };
                    let status = match self.company.save_fact(scope, credential, &edit).await {
                        Ok(()) => CommitStatus::Saved,
                        Err(crate::WriteError::Access(Error::Unauthorized)) => {
                            return Err(Error::Unauthorized)
                        }
                        Err(crate::WriteError::Access(Error::Conflict)) => CommitStatus::Error(
                            "The record changed. Reload before changing its activities.".into(),
                        ),
                        Err(crate::WriteError::Access(Error::Forbidden)) => CommitStatus::Error(
                            "The activity was not saved: this company is read-only.".into(),
                        ),
                        Err(_) => CommitStatus::failure(),
                    };
                    self.statuses.put("scope_activities", status);
                }
            }
            "cancel_remove_activity" => self.pending_activity = None,
            "request_lifecycle" if self.operation == "detail" => {
                let op: Operation = serde_json::from_value(Value::String(get("operation").into()))
                    .map_err(|_| Error::Invalid)?;
                let client = self.db.get().await?;
                let id = self.id.ok_or(Error::Invalid)?;
                crate::organisation::authorize_target(&**client, scope, id, crate::UPDATE).await?;
                let (company, _) = crate::detail(&**client, scope, id).await?;
                if !op.permits(&company.status) {
                    return Err(Error::Invalid);
                }
                self.pending = Some(Pending {
                    operation: op,
                    version: company.version,
                    reason: String::new(),
                    error: String::new(),
                });
            }
            "cancel_lifecycle" => self.pending = None,
            "apply_lifecycle" if self.operation == "detail" => {
                let pending = self.pending.as_mut().ok_or(Error::Invalid)?;
                pending.reason = get("reason").into();
                match self
                    .company
                    .transition_company(
                        scope,
                        self.id.ok_or(Error::Invalid)?,
                        pending.operation,
                        Some(&pending.reason),
                        Some(pending.version),
                    )
                    .await
                {
                    Ok(_) => {
                        self.pending = None;
                        self.notice = "Company status changed.".into();
                    }
                    Err(crate::lifecycle::LifecycleError::Write(crate::WriteError::Access(
                        Error::Unauthorized,
                    ))) => return Err(Error::Unauthorized),
                    Err(error) => pending.error = error.to_string(),
                }
            }
            "duplicate_grid" if self.operation == "create" => {
                self.duplicate_columns
                    .handle(&bilibi_ui::params::string_values(value));
            }
            "request_create_anyway" if self.operation == "create" => {
                let client = self.db.get().await?;
                bilibi_authz::authorize_key(&**client, scope, crate::CREATE).await?;
                if !self.duplicate_candidates.is_empty() {
                    self.confirming_duplicate = true;
                }
            }
            "cancel_duplicate_create" => self.confirming_duplicate = false,
            "duplicate_reason" if self.operation == "create" => {
                self.duplicate_reason = get("reason").to_owned();
            }
            "create_company" | "confirm_create_anyway" if self.operation == "create" => {
                let client = self.db.get().await?;
                bilibi_authz::authorize_key(&**client, scope, crate::CREATE).await?;
                let anyway = get("type") == "confirm_create_anyway";
                if !anyway {
                    self.form = value
                        .as_object()
                        .ok_or(Error::Invalid)?
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.into())))
                        .filter(|(k, _)| k != "type")
                        .collect();
                    self.duplicate_reason.clear();
                    self.confirming_duplicate = false;
                }
                let (mut new, activities, metadata, errors) = views::new_company(&self.form);
                self.errors = errors;
                if self.errors.is_empty() {
                    if let Err(errors) =
                        crate::organisation::preview(&**client, scope, &new).await?
                    {
                        self.errors = errors;
                    }
                }
                if self.errors.is_empty() {
                    let candidates = self
                        .company
                        .duplicates(scope, &crate::DuplicateQuery::from(&new))
                        .await?;
                    if anyway {
                        new.create_anyway = Some(crate::CreateAnyway {
                            candidate_ids: self.duplicate_candidates.iter().map(|c| c.id).collect(),
                            reason: self.duplicate_reason.clone(),
                        });
                    }
                    self.duplicate_candidates = candidates;
                    if anyway || self.duplicate_candidates.is_empty() {
                        match self
                            .company
                            .create_with_context(scope, &new, activities, metadata)
                            .await?
                        {
                            CreateOutcome::Created(company) => {
                                self.landing
                                    .put(StatusKind::Success, "Company created successfully.");
                                return Ok(Some(format!("/companies/{}", company.id)));
                            }
                            CreateOutcome::Refused(errors) => self.errors = errors,
                        }
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}
impl LiveState<Scope, Error> for CompanyPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.params = list_state(values)?;
                    self.columns.from_params(request.scope, values).await?;
                }
                LiveInput::Event(value) => {
                    if let Some(url) = self.event(request.scope, request.credential, value).await? {
                        return Ok(LiveBody {
                            redirect: Some(url),
                            flash: self.landing.take(),
                            ..LiveBody::default()
                        });
                    }
                }
            }
            self.draw(request.scope).await
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        let Some(id) = self.id else {
            return Vec::new();
        };
        let mut panels = self.active_panels.clone();
        if self.history {
            panels.push(PanelSpec {
                id: "company-record-history".into(),
                embed: "audit.record-history".into(),
                path: [
                    ("panel".into(), "company-record-history".into()),
                    ("kinds".into(), "company".into()),
                    ("id".into(), id.to_string()),
                ]
                .into(),
                query: HashMap::new(),
            });
        }
        // The Relationships section is Company's own panel (K6), the same
        // embed a Person page mounts with `kind: person`.
        panels.push(PanelSpec {
            id: "company-relationships".into(),
            embed: "company.relationships".into(),
            path: [
                ("panel".into(), "company-relationships".into()),
                ("kind".into(), "organisation".into()),
                ("id".into(), id.to_string()),
            ]
            .into(),
            query: HashMap::new(),
        });
        panels
    }
}

// Shared facts mirror Bilimbi’s record detail module; columns extend its list vocabulary.
bilibi_ui::record_fields! { pub(crate) mod fields {
    NAME: bilibi_ui::RecordField::new("name", "Name"),
    CODE: bilibi_ui::RecordField::new("code", "Code"),
    LEGAL_NAME: bilibi_ui::RecordField::new("legal_name", "Legal Name"),
    STATUS: bilibi_ui::RecordField::new("status", "Status"),
    LEGAL_ENTITY_TYPE_ID: bilibi_ui::RecordField::new("legal_entity_type_id", "Legal Entity Type").path("legal_entity_type.name"),
    REGISTRATION_NUMBER: bilibi_ui::RecordField::new("registration_number", "Registration Number"),
    TAX_ID: bilibi_ui::RecordField::new("tax_id", "Tax ID"),
    JURISDICTION: bilibi_ui::RecordField::new("jurisdiction", "Jurisdiction"),
    EMAIL: bilibi_ui::RecordField::new("email", "Email"),
    WEBSITE: bilibi_ui::RecordField::new("website", "Website"),
    PARENT_ID: bilibi_ui::RecordField::new("parent_id", "Parent Company").path("parent.name").slot("parent_name"),
    ACTIVITIES: bilibi_ui::RecordField::new("scope_activities", "Business Activities").excluded("Nested collection"),
    METADATA: bilibi_ui::RecordField::new("metadata", "Metadata").excluded("Long structured text"),
} }
