use super::*;
use crate::{contributions::CREATE, PersonKind, Search};
use bilibi_grid::{Outcome, PageColumns};
use bilibi_ui::{
    choices,
    list_state::{InvalidPageSize, ParamNames},
    lists::RecordLink,
    BuiltinColumn, Card, CreateButton, Filter, FilterToolbar, Header, ListSpec, ListState,
    Pagination, SortDir, TableEmpty, ToolbarControl,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::LazyLock};

const SIZES: [u64; 4] = [25, 50, 100, 300];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("kind".into(), SortDir::Asc),
            ("state".into(), SortDir::Asc),
            ("created_at".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        invalid_page_size: InvalidPageSize::Default,
        filters: BTreeMap::from([
            (
                "kind".into(),
                Filter::one_of(&["all", "human", "agent"], "all"),
            ),
            (
                "retired".into(),
                Filter::one_of(&["active", "all"], "active"),
            ),
        ]),
        param_names: ParamNames {
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
            ..ParamNames::default()
        },
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("person list spec")
});

pub(super) struct Index {
    pages: Pages,
    list: ListState,
    columns: PageColumns,
    masked: std::collections::HashSet<String>,
    framed: bool,
    /// Set while the new-contact dialog is open, including after `/contacts/new`.
    draft: Option<super::create::Draft>,
}
/// The contact fields withheld from this reader: restricted and not masked. A
/// masked contact keeps its column and shows its partial value.
async fn withheld(
    client: &tokio_postgres::Client,
    scope: &Scope,
) -> Result<std::collections::HashSet<String>> {
    let masks = bilibi_authz::fields::masks_for(client, scope, "persons").await?;
    Ok(bilibi_authz::restrictions_for(client, scope, "persons")
        .await?
        .into_iter()
        .filter(|field| !masks.contains_key(field))
        .collect())
}
// Bilimbi Person.IndexLive: contact columns with discovered work details. Identity
// filters and URL sorts remain the approved Person model's additional controls.
fn columns(pages: &Pages, masked: &std::collections::HashSet<String>) -> Result<PageColumns> {
    PageColumns::mount(
        pages.views.clone(),
        "persons",
        [
            BuiltinColumn::new("name", "Name").sortable("name"),
            BuiltinColumn::new("email", "Email"),
            BuiltinColumn::new("phone", "Phone"),
        ]
        .into_iter()
        .filter(|column| !masked.contains(&column.id))
        .collect(),
    )
    .map(|columns| {
        columns.with_defaults(&[
            "name",
            "work.work_email:list",
            "work.title:list",
            "work.company.name:list",
            "email",
            "phone",
        ])
    })
    .map_err(|_| Error::Internal)
}
impl Index {
    pub async fn new(
        pages: Pages,
        scope: &Scope,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Self> {
        let client = pages.db.get().await?;
        let masked = withheld(&client, scope).await?;
        let mut columns = columns(&pages, &masked)?;
        columns.from_params(scope, query).await?;
        let open = query.get("new").map(String::as_str) == Some("1")
            && pages.allowed(scope, CREATE).await?;
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns,
            masked,
            framed,
            draft: open.then(super::create::Draft::new),
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.pages.db.get().await?;
        let masked = withheld(&client, scope).await?;
        if masked != self.masked {
            let query = self.columns.params();
            self.columns = columns(&self.pages, &masked)?;
            self.columns.from_params(scope, &query).await?;
            self.masked = masked;
        }
        self.list.search = self.list.search.trim().chars().take(200).collect();
        let search = |list: &ListState| Search {
            search: list.search.trim().chars().take(200).collect(),
            kind: match list.filters["kind"].as_str() {
                "human" => Some(PersonKind::Human),
                "agent" => Some(PersonKind::Agent),
                _ => None,
            },
            include_retired: list.filters["retired"] == "all",
            page: list.page as i64,
            page_size: list.page_size as i64,
            sort: list.sort_by.clone().unwrap_or_else(|| "name".into()),
            descending: list.sort_dir == Some(SortDir::Desc),
        };
        let mut page = self
            .pages
            .persons
            .search(scope, &search(&self.list))
            .await?;
        let total_pages = (page.total as u64).div_ceil(self.list.page_size);
        if self.list.clamp_to_last_page(total_pages, true) {
            page = self
                .pages
                .persons
                .search(scope, &search(&self.list))
                .await?;
        }
        let keys: Vec<_> = page.entries.iter().map(|p| json!(p.id.get())).collect();
        let display = self.pages.display(scope).await?;
        // Bilimbi offers 300-row pages; Grid bounds each projection to 100
        // IDs. Compose the page from bounded batches without weakening that
        // shared query limit or dropping its walked columns on a large page.
        if keys.is_empty() {
            self.columns.load(scope, &[], &display).await?;
        } else {
            let mut rows = Vec::with_capacity(keys.len());
            for batch in keys.chunks(bilibi_grid::query::MAX_ROWS) {
                self.columns.load(scope, batch, &display).await?;
                rows.append(&mut self.columns.rows);
            }
            self.columns.rows = rows;
        }
        let mut actions = Html::EMPTY;
        if self.pages.allowed(scope, CREATE).await? {
            actions.push(&render(CreateButton {
                noun: "Contact",
                id: "person-new",
                class: "text-xs",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        let mut body = render(Header {
            title: Html::text("Contacts"),
            actions,
            ..Header::default()
        })?;
        let form = self.list.filters_form();
        body.push(&render(FilterToolbar::new(
            "persons-filters",
            vec![
                ToolbarControl::search(
                    "persons-search",
                    "search",
                    "Search contacts",
                    &form["search"],
                    "Search by name, email, phone, role, or company...",
                ),
                ToolbarControl::select(
                    "persons-kind",
                    "kind",
                    "Kind",
                    &form["kind"],
                    choices(&[
                        ("All kinds", "all"),
                        ("Human only", "human"),
                        ("Agent only", "agent"),
                    ]),
                ),
                ToolbarControl::select(
                    "persons-retired",
                    "retired",
                    "Identity",
                    &form["retired"],
                    choices(&[("Current identities", "active"), ("Include retired", "all")]),
                ),
            ],
        ))?);
        let mut rows = self.columns.rows.clone();
        for row in &mut rows {
            let Some(person) = page
                .entries
                .iter()
                .find(|p| p.id.get().to_string() == row.key)
            else {
                continue;
            };
            row.dom_id = Some(format!("person-{}", person.id.get()));
            row.slots.insert(
                "name".into(),
                render(RecordLink {
                    workspace: self.framed,
                    kind: "core/person",
                    record_id: &person.id.get().to_string(),
                    href: &format!("/contacts/{}", person.id.get()),
                    class: "font-medium text-ink-strong hover:underline",
                    body: Html::text(&person.name),
                })?,
            );
            for (key, value) in [("email", &person.email), ("phone", &person.phone)] {
                if !person.restricted.iter().any(|field| field == key) {
                    row.slots
                        .insert(key.into(), Html::text(value.as_deref().unwrap_or("—")));
                }
            }
        }
        let mut table = self
            .columns
            .flex_table("persons", rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Contacts";
        table.framed = false;
        if page.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text("No contacts found.")));
        }
        let mut card = render(table)?;
        card.push(&render(Pagination::new(
            "persons-pagination",
            bilibi_ui::PageCounts::new(self.list.page, self.list.page_size, page.total as u64),
            &SIZES,
        ))?);
        body.push(&render(Card {
            id: "persons-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        if self.draft.is_some() && !self.pages.allowed(scope, CREATE).await? {
            self.draft = None;
        }
        let pages = self.pages.clone();
        if let Some(draft) = &mut self.draft {
            body.push(&render(bilibi_ui::Modal {
                id: "person-create-modal",
                title: "New contact",
                cancel_event: "close_modal",
                body: draft.form(&pages, scope).await?,
                ..bilibi_ui::Modal::default()
            })?);
        }
        let html = render(bilibi_ui::Page {
            id: "persons-index",
            body,
            ..bilibi_ui::Page::default()
        })?
        .to_string();
        let mut query = self.list.to_params();
        query.extend(self.columns.params());
        if self.draft.is_some() {
            query.insert("new".into(), "1".into());
        }
        Ok(LiveBody {
            html,
            query,
            rows: page.entries.len(),
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.list =
                        ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.columns.from_params(request.scope, query).await?;
                    if query.get("new").map(String::as_str) == Some("1")
                        && self.draft.is_none()
                        && self.pages.allowed(request.scope, CREATE).await?
                    {
                        self.draft = Some(super::create::Draft::new());
                    }
                }
                LiveInput::Event(value) => match event(value)? {
                    "filters" => {
                        let filters = bilibi_ui::params::form_values_checked(
                            value,
                            bilibi_ui::list_state::FILTERS_FORM,
                            bilibi_ui::params::FIELD_LIMIT,
                        )
                        .map_err(bilibi_database::Error::refused)?;
                        self.list.apply_filters(&filters);
                    }
                    "page" => {
                        self.list
                            .put_page(value.get("page").and_then(Value::as_str));
                    }
                    "sort" => {
                        self.list.next_sort(value["sort"].as_str().unwrap_or(""));
                    }
                    "grid" => {
                        if let Outcome::Sort(key) =
                            self.columns.handle(request.scope, &values(value)).await?
                        {
                            self.list.next_sort(&key);
                        }
                    }
                    "new" => {
                        if self.pages.allowed(request.scope, CREATE).await? {
                            self.draft = Some(super::create::Draft::new());
                        }
                    }
                    "close_modal" => self.draft = None,
                    "validate" | "create" => {
                        let pages = self.pages.clone();
                        let Some(draft) = &mut self.draft else {
                            return self.view(request.scope).await;
                        };
                        if let Some(id) = draft
                            .submit(
                                &pages,
                                request.scope,
                                request.credential,
                                event(value)? == "create",
                                value,
                            )
                            .await?
                        {
                            self.draft = None;
                            return Ok(LiveBody {
                                redirect: Some(format!("/contacts/{}", id.get())),
                                flash: Some((
                                    bilibi_ui::StatusKind::Success,
                                    "Contact created.".into(),
                                )),
                                ..LiveBody::default()
                            });
                        }
                    }
                    "clear-flash" => {}
                    _ => return Err(Error::Invalid),
                },
            }
            self.view(request.scope).await
        })
    }
}
