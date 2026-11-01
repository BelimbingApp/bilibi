//! Company-page Users panel (embed
//! `company.users`): the accounts whose Person has a desk in the company, searched,
//! sorted and paged in the panel's own state over the shared flexible table
//! with built-in columns. Read-only; the company route's `admin.company.view`
//! is the only gate.
use super::{company_accounts, event, render, verified_badge, Listed, Strong};
use crate::web::Pages;
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_security::Scope;
use bilibi_ui::{
    list_state::{InvalidPageSize, ParamNames, FILTERS_FORM},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    params, Badge, BuiltinColumn, Card, ColumnsOutcome, FilterToolbar, FlexRow, FlexTable, Html,
    ListColumns, ListSpec, ListState, Pagination, SortDir, TableEmpty, ToolbarControl,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("email".into(), SortDir::Asc),
            ("email_verified".into(), SortDir::Asc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        page_size_aliases: vec!["perPage".into()],
        invalid_page_size: InvalidPageSize::Default,
        param_prefix: "users_".into(),
        param_names: ParamNames {
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
            ..ParamNames::default()
        },
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("company users spec")
});

#[derive(Template)]
#[template(path = "company_users.html")]
struct View<'a> {
    id: &'a str,
    card: Html,
}
#[derive(Template)]
#[template(path = "company_users_card.html")]
struct CardBody {
    count: Html,
    filters: Html,
    table: Html,
    pagination: Html,
}

pub(crate) struct CompanyUsers {
    pages: Pages,
    anchor: String,
    company: i64,
    list: ListState,
    columns: ListColumns,
    rows_changed: bool,
}
impl CompanyUsers {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let anchor = request.path.get("panel").cloned().ok_or(Error::Invalid)?;
        let company = params::positive_id(request.path.get("id").map(String::as_str))
            .ok_or(Error::Invalid)?;
        let columns = ListColumns::mount(
            "company-users-table",
            vec![
                BuiltinColumn::new("name", "Name").sortable("name"),
                BuiltinColumn::new("email", "Email").sortable("email"),
                BuiltinColumn::new("email_verified", "Email verified").sortable("email_verified"),
            ],
        )
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            anchor,
            company,
            list: ListState::parse(&HashMap::new(), SPEC.clone()).map_err(|_| Error::Internal)?,
            columns,
            rows_changed: true,
        })
    }
    fn arrange(&self, mut users: Vec<Listed>) -> Vec<Listed> {
        let search = self.list.search.trim().to_lowercase();
        if !search.is_empty() {
            users.retain(|u| {
                u.name.to_lowercase().contains(&search) || u.email.to_lowercase().contains(&search)
            });
        }
        match self.list.sort_by.as_deref() {
            Some("email") => users.sort_by_key(|u| (u.email.to_lowercase(), u.id)),
            Some("email_verified") => users.sort_by_key(|u| (u.email_verified_at.is_none(), u.id)),
            _ => users.sort_by_key(|u| (u.name.to_lowercase(), u.id)),
        }
        if self.list.sort_dir == Some(SortDir::Desc) {
            users.reverse();
        }
        users
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        self.pages.require(scope, bilibi_company::VIEW).await?;
        self.list.search = self.list.search.trim().chars().take(255).collect();
        let users = {
            let client = self.pages.db.get().await?;
            company_accounts(&**client, scope, self.company).await?
        };
        let count = users.len();
        let arranged = self.arrange(users);
        let total_pages = (arranged.len() as u64).div_ceil(self.list.page_size);
        self.list.clamp_to_last_page(total_pages, true);
        let page = self.list.paginate(&arranged);
        let counts = page.counts;
        if self.rows_changed {
            self.columns.load(page.entries, Listed::key);
        }
        self.rows_changed = true;
        let mut rows = Vec::with_capacity(page.entries.len());
        for user in page.entries {
            rows.push(
                FlexRow::new(user.key())
                    .slot("name", render(&Strong { text: &user.name })?)
                    .slot("email", Html::text(&user.email))
                    .slot("email_verified", verified_badge(user)?),
            );
        }
        let mut table = FlexTable::new("company-users-table", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.sort_by = self.list.sort_by.as_deref();
        table.sort_dir = self.list.sort_dir;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.event = "users_grid";
        table.target = &self.anchor;
        table.sort_event = "users_sort";
        table.sort_target = &self.anchor;
        table.caption = "Users";
        table.framed = false;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No users found for this company.",
            )));
        }
        let mut pagination = Pagination::new("company-users-pagination", counts, &PAGE_SIZES);
        pagination.filters_event = "users_filters";
        pagination.page_event = "users_page";
        pagination.target = &self.anchor;
        let mut filters = FilterToolbar::new(
            "company-users-filters",
            vec![ToolbarControl::search(
                "company-users-search",
                "search",
                "Search users",
                &self.list.search,
                "Search by name or email...",
            )],
        );
        filters.event = "users_filters";
        filters.target = &self.anchor;
        let body = render(&CardBody {
            count: render(&Badge {
                body: Html::text(&count.to_string()),
                ..Badge::default()
            })?,
            filters: render(&filters)?,
            table: render(&table)?,
            pagination: render(&pagination)?,
        })?;
        let card = render(&Card {
            id: "company-users-card",
            class: "mt-6",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        Ok(LiveBody {
            html: render(&View {
                id: &self.anchor,
                card,
            })?
            .to_string(),
            query: self.list.to_params(),
            rows: counts.total_entries as usize,
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for CompanyUsers {
    fn query_prefix(&self) -> &str {
        &self.list.spec().param_prefix
    }
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(value) => match event(value)? {
                    "users_filters" => self.list.apply_filters(
                        &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                            .map_err(Error::refused)?,
                    ),
                    "users_page" => self
                        .list
                        .put_page(value.get("page").and_then(serde_json::Value::as_str)),
                    "users_sort" => {
                        self.list.next_sort(
                            value
                                .get("sort")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(""),
                        );
                    }
                    "users_grid" => match self.columns.handle(
                        &params::string_values_checked(value, params::FIELD_LIMIT)
                            .map_err(Error::refused)?,
                    ) {
                        ColumnsOutcome::Sort(column) => {
                            self.list.next_sort(&column);
                        }
                        ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
                    },
                    _ => return Err(Error::Invalid),
                },
            }
            self.view(request.scope).await
        })
    }
}
