//! `/addresses` (Bilimbi `IndexLive`): bounded search, sort and paging kept
//! in the URL under Bilimbi's `sortBy`/`sortDir`/`perPage`, built-in columns
//! through `ListColumns`, and deletion confirmed on the listed record.
use super::*;
use crate::{
    contributions::{CREATE, DELETE, VIEW},
    Address, Direction, Search, Sort,
};
use bilibi_ui::{
    list_state::{InvalidPageSize, ParamNames, FILTERS_FORM},
    lists::RecordLink,
    Badge, BadgeKind, BuiltinColumn, Card, ColumnsOutcome, ConfirmDialog, CreateButton,
    FilterToolbar, FlexRow, FlexTable, Header, IconButton, IconContext, IconKind, ListColumns,
    ListSpec, ListState, PageCounts, Pagination, SortDir, TableEmpty, ToolbarControl,
};
use std::sync::LazyLock;

/// The page API refuses a page above 100 rows, so Bilimbi offers no 300.
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: [
            ("label".into(), SortDir::Asc),
            ("country_iso".into(), SortDir::Asc),
            ("verification_status".into(), SortDir::Asc),
        ]
        .into(),
        default_sort: Some("label".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "perPage".into(),
        invalid_page_size: InvalidPageSize::Default,
        param_names: ParamNames {
            search: "search".into(),
            page: "page".into(),
            sort_by: "sortBy".into(),
            sort_dir: "sortDir".into(),
        },
        ..ListSpec::default()
    }
    .shared()
    .expect("address list spec")
});
fn builtins() -> Vec<BuiltinColumn> {
    let sortable = |id: &str, label: &str, sort_id: &str| {
        let mut column = BuiltinColumn::new(id, label).sortable(id);
        column.sort_id = Some(sort_id.into());
        column
    };
    vec![
        sortable("label", "Label", "addresses-sort-label"),
        BuiltinColumn::new("address", "Address"),
        BuiltinColumn::new("locality", "Locality"),
        sortable("country_iso", "Country", "addresses-sort-country"),
        sortable("verification_status", "Status", "addresses-sort-status"),
    ]
}
pub(super) fn status_kind(status: &str) -> BadgeKind {
    match status {
        "verified" => BadgeKind::Success,
        "suggested" => BadgeKind::Warning,
        _ => BadgeKind::Neutral,
    }
}

#[derive(Template)]
#[template(path = "address_index.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
}

/// The listed address a delete confirmation names and acts on.
struct Pending {
    id: AddressId,
    version: i64,
    label: Option<String>,
    code: String,
}

pub(super) struct Index {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    framed: bool,
    flash: Flash,
    entries: Vec<Address>,
    names: HashMap<String, String>,
    total: u64,
    pending: Option<Pending>,
}
impl Index {
    pub fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("addresses-table", builtins())
                .map(|columns| columns.with_record_fields(super::fields::ALL.iter().copied()))
                .map_err(|_| Error::Internal)?,
            framed,
            flash: Flash::default(),
            entries: Vec::new(),
            names: HashMap::new(),
            total: 0,
            pending: None,
        })
    }
    fn search(&self) -> Search {
        Search {
            q: Some(
                self.list
                    .search
                    .trim()
                    .chars()
                    .take(200)
                    .collect::<String>(),
            )
            .filter(|q| !q.is_empty()),
            sort: match self.list.sort_by.as_deref() {
                Some("country_iso") => Sort::CountryIso,
                Some("verification_status") => Sort::VerificationStatus,
                _ => Sort::Label,
            },
            dir: if self.list.sort_dir == Some(SortDir::Desc) {
                Direction::Desc
            } else {
                Direction::Asc
            },
            page: self.list.page as i64,
            page_size: self.list.page_size as i64,
        }
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let mut page = self.pages.addresses.search(scope, &self.search()).await?;
        let last = (page.total as u64).div_ceil(self.list.page_size);
        if self.list.clamp_to_last_page(last, true) {
            page = self.pages.addresses.search(scope, &self.search()).await?;
        }
        // The rows changed: ListColumns forgets an unfinished add box.
        self.columns.load(&page.entries, |a| a.id.to_string());
        self.total = page.total as u64;
        self.names.clear();
        self.entries = page.entries;
        self.admin1_names().await
    }

    async fn admin1_names(&mut self) -> Result<()> {
        if !self.columns.visible_ids().contains(&"admin1_code") {
            return Ok(());
        }
        for entry in &self.entries {
            if let Some(code) = &entry.address.admin1_code {
                if !self.names.contains_key(code) {
                    let name = self
                        .pages
                        .addresses
                        .geonames()
                        .get_admin1(code)
                        .await?
                        .map(|division| division.name)
                        .unwrap_or_else(|| code.clone());
                    self.names.insert(code.clone(), name);
                }
            }
        }
        Ok(())
    }
    async fn request_delete(
        &mut self,
        scope: &Scope,
        credential: &str,
        id: Option<&str>,
    ) -> Result<()> {
        let listed = bilibi_ui::params::positive_id(id)
            .and_then(|id| self.entries.iter().find(|a| a.id.get() == id));
        match listed {
            Some(address) => {
                let input = serde_json::json!({"address_id":address.id,"version":address.version});
                let code = match self
                    .pages
                    .preview(
                        scope,
                        credential,
                        "request_delete",
                        "core.address.delete",
                        input,
                    )
                    .await?
                {
                    Ok(code) => code,
                    Err(error) => {
                        let name = address_name(address.address.label.as_deref());
                        let message = match error {
                            WriteError::InUse => format!("{name} was not deleted: it is still attached to a company or person. Unlink it there first."),
                            WriteError::NotFound => "That address no longer exists.".into(),
                            WriteError::Access(Error::Forbidden) => "You do not have permission to delete addresses.".into(),
                            _ => format!("{name} was not deleted. Reload the page and try again."),
                        };
                        self.flash.put(StatusKind::Error, message);
                        return Ok(());
                    }
                };
                self.flash.clear();
                self.pending = Some(Pending {
                    code,
                    id: address.id,
                    version: address.version,
                    label: address.address.label.clone(),
                });
            }
            None => self
                .flash
                .put(StatusKind::Error, "That address no longer exists."),
        }
        Ok(())
    }
    async fn delete(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let name = address_name(pending.label.as_deref());
        match self
            .pages
            .commit(scope,credential,"delete","core.address.delete",&pending.code,serde_json::json!({"address_id":pending.id,"version":pending.version}))
            .await?
        {
            Ok(()) => self.flash.put(StatusKind::Success, "Address deleted."),
            Err(error) if fatal(&error) => return Err(Error::Unauthorized),
            Err(WriteError::InUse) => self.flash.put(
                StatusKind::Error,
                format!("{name} was not deleted: it is still attached to a company or person. Unlink it there first."),
            ),
            Err(WriteError::NotFound) => self
                .flash
                .put(StatusKind::Error, "That address no longer exists."),
            Err(WriteError::Access(Error::Forbidden)) => self.flash.put(
                StatusKind::Error,
                "You do not have permission to delete addresses.",
            ),
            Err(_) => self.flash.put(
                StatusKind::Error,
                format!("{name} was not deleted. Reload the page and try again."),
            ),
        }
        Ok(())
    }
    async fn view(&mut self, scope: &Scope) -> Result<String> {
        let can_view = self.pages.allowed(scope, VIEW).await?;
        let can_delete = self.pages.allowed(scope, DELETE).await?;
        let mut actions = Html::EMPTY;
        if self.pages.allowed(scope, CREATE).await? {
            actions = render(CreateButton {
                noun: "Address",
                href: "/addresses/create",
                id: "address-create",
                ..CreateButton::default()
            })?;
        }
        let header = render(Header {
            title: Html::text("Addresses"),
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "addresses-pin",
                nav_pin: "nav-admin-address",
                ..IconButton::new("bilimbi-pin", "Pin Addresses to sidebar")
            })?,
            actions,
            ..Header::default()
        })?;
        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "addresses-filters",
            vec![ToolbarControl::search(
                "addresses-search",
                "search",
                "Search addresses",
                &form["search"],
                "Search by label, address, locality, postcode, or country...",
            )],
        ))?;
        let normal = self.columns.mode() == bilibi_ui::RowMode::Normal;
        let mut rows = Vec::with_capacity(self.entries.len());
        for address in &self.entries {
            let id = address.id.get();
            let a = &address.address;
            let label = Html::text(a.label.as_deref().unwrap_or("Unlabeled"));
            let label = if can_view {
                render(RecordLink {
                    workspace: self.framed,
                    kind: "core/address",
                    record_id: &id.to_string(),
                    href: &format!("/addresses/{id}"),
                    class: "whitespace-nowrap font-medium text-action hover:underline",
                    body: label,
                })?
            } else {
                Html::raw(format!(
                    "<span class=\"whitespace-nowrap font-medium text-ink\">{label}</span>"
                ))
            };
            let second = |value: &Option<String>, extra: &str| match value {
                Some(value) if normal => format!(
                    "<span class=\"block text-xs {extra}text-ink-subtle\">{}</span>",
                    Html::text(value)
                ),
                _ => String::new(),
            };
            let mut row = FlexRow::new(id.to_string())
                .slot("label", label)
                .slot(
                    "address",
                    Html::raw(format!(
                        "<div class=\"min-w-56 text-ink-muted\"><span>{}</span>{}</div>",
                        Html::text(a.line1.as_deref().unwrap_or("—")),
                        second(&a.line2, "")
                    )),
                )
                .slot(
                    "locality",
                    Html::raw(format!(
                        "<div class=\"whitespace-nowrap text-ink-muted\"><span>{}</span>{}</div>",
                        Html::text(a.locality.as_deref().unwrap_or("—")),
                        second(&a.postcode, "tabular-nums ")
                    )),
                )
                .slot(
                    "country_iso",
                    Html::raw(format!(
                        "<span class=\"whitespace-nowrap font-mono text-xs text-ink-muted\">{}</span>",
                        Html::text(a.country_iso.as_deref().unwrap_or("—"))
                    )),
                );
            for field in super::fields::ALL {
                let Some(column) = field.column else { continue };
                if ["label", "country_iso", "locality", "verification_status"].contains(&column) {
                    continue;
                }
                let value = match field.id {
                    "phone" => &a.phone,
                    "line1" => &a.line1,
                    "line2" => &a.line2,
                    "line3" => &a.line3,
                    "postcode" => &a.postcode,
                    "admin1_code" => &a.admin1_code,
                    "source" => &a.source,
                    "source_ref" => &a.source_ref,
                    "parser_version" => &a.parser_version,
                    "parse_confidence" => &a.parse_confidence,
                    _ => continue,
                };
                let shown = value
                    .as_deref()
                    .map(|value| {
                        if field.id == "admin1_code" {
                            self.names.get(value).map_or(value, String::as_str)
                        } else {
                            value
                        }
                    })
                    .unwrap_or("—");
                row = row.slot(column, Html::text(shown));
            }
            let status = a.verification_status.as_deref().unwrap_or("unverified");
            row = row.slot(
                "verification_status",
                render(Badge::status(status_kind(status), status))?,
            );
            row.dom_id = Some(format!("address-{id}"));
            if can_delete {
                let label = format!("Delete {}", a.label.as_deref().unwrap_or("address"));
                let value = id.to_string();
                row.actions = render(IconButton {
                    kind: IconKind::Danger,
                    id: &format!("address-delete-{id}"),
                    event: "request_delete",
                    name: "id",
                    value: &value,
                    ..IconButton::new("delete", &label)
                })?;
            }
            rows.push(row);
        }
        let mut table = FlexTable::new("addresses-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Addresses";
        table.actions = can_delete;
        if self.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text("No addresses found.")));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "addresses-pagination",
            PageCounts::new(self.list.page, self.list.page_size, self.total),
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "addresses-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match &self.pending {
            Some(pending) => render(ConfirmDialog {
                id: "delete-address-confirm",
                consequence: &format!(
                    "{} will be deleted.",
                    address_name(pending.label.as_deref())
                ),
                detail:
                    "It can no longer be attached to a company or person. This cannot be undone.",
                confirm: "Delete",
                working: "Deleting…",
                confirm_event: "delete",
                cancel_event: "cancel_delete",
            })?,
            None => Html::EMPTY,
        };
        View {
            flash: self.flash.map(),
            header,
            toolbar,
            card,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope;
            let client = self.pages.db.get().await?;
            // A masked field keeps its column and shows its partial value.
            let masks = bilibi_authz::fields::masks_for(&**client, scope, "addresses").await?;
            let masked = bilibi_authz::restrictions_for(&**client, scope, "addresses").await?;
            self.columns.set_available(super::fields::ALL, |field| {
                !masked.contains(field.id) || masks.contains_key(field.id)
            });

            let mut requery = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(value) => match event(value)? {
                    "filters" => self.list.apply_filters(
                        &bilibi_ui::params::form_values_checked(
                            value,
                            FILTERS_FORM,
                            bilibi_ui::params::FIELD_LIMIT,
                        )
                        .map_err(bilibi_database::Error::refused)?,
                    ),
                    "sort" => {
                        if !self.list.next_sort(value["sort"].as_str().unwrap_or("")) {
                            return Err(Error::Invalid);
                        }
                    }
                    "grid" => match self.columns.handle(
                        &bilibi_ui::params::string_values_checked(
                            value,
                            bilibi_ui::params::FIELD_LIMIT,
                        )
                        .map_err(bilibi_database::Error::refused)?,
                    ) {
                        ColumnsOutcome::Sort(key) => {
                            if !self.list.next_sort(&key) {
                                return Err(Error::Invalid);
                            }
                        }
                        ColumnsOutcome::Update | ColumnsOutcome::Noop => {
                            requery = false;
                            self.admin1_names().await?;
                        }
                    },
                    "page" => self
                        .list
                        .put_page(value.get("page").and_then(Value::as_str)),
                    // The request holds the listed address whose consequence
                    // the dialog states; `delete` acts on that one, so what
                    // was confirmed is what runs.
                    "request_delete" => {
                        requery = false;
                        if self.pages.allowed(scope, DELETE).await? {
                            self.request_delete(
                                scope,
                                request.credential,
                                value.get("id").and_then(Value::as_str),
                            )
                            .await?;
                        } else {
                            self.flash.put(
                                StatusKind::Error,
                                "You do not have permission to delete addresses.",
                            );
                        }
                    }
                    "cancel_delete" => {
                        requery = false;
                        self.pending = None;
                    }
                    "delete" => self.delete(scope, request.credential).await?,
                    "clear-flash" => {
                        requery = false;
                        self.flash.dismiss(value.get("key").and_then(Value::as_str));
                    }
                    _ => return Err(Error::Invalid),
                },
            }
            if requery {
                self.load(scope).await?;
            }
            Ok(LiveBody {
                html: self.view(scope).await?,
                query: self.list.to_params(),
                rows: self.entries.len(),
                ..LiveBody::default()
            })
        })
    }
}
