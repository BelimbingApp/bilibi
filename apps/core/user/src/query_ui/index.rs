use super::*;
use bilibi_ui::{
    list_columns::{BuiltinColumn, ColumnsOutcome, ListColumns},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest},
    params, SortDir,
};
use serde_json::Value;
use std::sync::LazyLock;

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: ["name", "description", "created_at", "updated_at"]
            .map(|key| {
                (
                    key.into(),
                    if key.ends_with("_at") {
                        SortDir::Desc
                    } else {
                        SortDir::Asc
                    },
                )
            })
            .into(),
        default_sort: Some("updated_at".into()),
        page_size_aliases: vec!["perPage".into()],
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("database queries list spec")
});

pub(super) struct Index {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    pending: Option<queries::SavedQuery>,
    success: String,
    /// What the page a redirect lands on shows.
    landing: Option<(StatusKind, String)>,
    error: String,
}
impl Index {
    pub fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: parse(query, SPEC.clone())?,
            columns: ListColumns::mount(
                "database-queries-table",
                [
                    ("name", "Name"),
                    ("description", "Description"),
                    ("created_at", "Created"),
                    ("updated_at", "Updated"),
                ]
                .map(|(key, _label)| {
                    let mut column =
                        BuiltinColumn::new(key, super::fields::get(key).label).sortable(key);
                    if key.ends_with("_at") {
                        column.kind = bilibi_ui::flex_table::ColumnType::Datetime;
                    }
                    column
                })
                .to_vec(),
            )
            .map(|columns| columns.with_record_fields(super::fields::ALL.iter().copied()))
            .map_err(|_| Error::Internal)?,
            pending: None,
            success: String::new(),
            landing: None,
            error: String::new(),
        })
    }
    async fn event(
        &mut self,
        scope: &Scope,
        value: &Value,
        editor: bool,
    ) -> Result<Option<String>> {
        let get = |key| value.get(key).and_then(Value::as_str);
        let kind = get("type").ok_or(Error::Invalid)?;
        if ["duplicate", "request_delete", "delete"].contains(&kind) && !editor {
            self.error = "You are not authorized to modify queries.".into();
            self.pending = None;
            return Ok(None);
        }
        match kind {
            "filters" => self.list.apply_filters(
                &params::form_values_checked(value, "filters", params::FIELD_LIMIT)
                    .map_err(bilibi_database::Error::refused)?,
            ),
            "page" => self.list.put_page(get("page")),
            "sort" => {
                self.list.next_sort(get("sort").unwrap_or(""));
            }
            "grid" => {
                if let ColumnsOutcome::Sort(key) = self.columns.handle(&values(value)) {
                    self.list.next_sort(&key);
                }
            }
            "clear-flash" => {
                self.error.clear();
                self.success.clear();
            }
            "request_delete" | "duplicate" => {
                let id = params::positive_id(get("id")).ok_or(Error::Invalid)?;
                if kind == "request_delete" {
                    self.pending = Some(
                        self.pages
                            .queries
                            .get(scope, queries::QueryKey::Id(id))
                            .await
                            .map_err(database_error)?,
                    );
                } else {
                    match self
                        .pages
                        .queries
                        .duplicate(scope, queries::QueryKey::Id(id))
                        .await
                    {
                        Ok(query) => {
                            self.landing = Some((StatusKind::Success, "Query duplicated.".into()));
                            return Ok(Some(format!("{ROOT}/{}", query.slug)));
                        }
                        Err(error) => self.error = message(error),
                    }
                }
            }
            "cancel_delete" => self.pending = None,
            "delete" => {
                if let Some(query) = self.pending.take() {
                    match self
                        .pages
                        .queries
                        .delete(scope, queries::QueryKey::Id(query.id), query.version)
                        .await
                    {
                        Ok(_) => self.success = format!("Query “{}” was deleted.", query.name),
                        Err(error) => self.error = message(error),
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}
impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let editor = self.pages.editor(request.scope).await?;
            let mut redirect = None;
            match request.input {
                LiveInput::Params(query) => self.list = parse(query, SPEC.clone())?,
                LiveInput::Event(value) => {
                    redirect = self.event(request.scope, value, editor).await?
                }
                LiveInput::Refresh => {}
            }
            if !editor {
                self.pending = None;
            }
            // The shared URL state has a 255-char bound; C9 accepts 200 bytes.
            while self.list.search.len() > 200 {
                self.list.search.pop();
            }
            let query = queries::ListQuery {
                search: self.list.search.clone(),
                page: self.list.page as i64,
                per_page: self.list.page_size as i64,
                sort: match self.list.sort_by.as_deref() {
                    Some("description") => queries::Sort::Description,
                    Some("created_at") => queries::Sort::CreatedAt,
                    Some("updated_at") => queries::Sort::UpdatedAt,
                    _ => queries::Sort::Name,
                },
                descending: self.list.sort_dir == Some(SortDir::Desc),
            };
            let mut found = self
                .pages
                .queries
                .list(request.scope, &query)
                .await
                .map_err(database_error)?;
            if self.list.clamp_to_last_page(found.last_page as u64, true) {
                found = self
                    .pages
                    .queries
                    .list(
                        request.scope,
                        &queries::ListQuery {
                            page: self.list.page as i64,
                            ..query
                        },
                    )
                    .await
                    .map_err(database_error)?;
            }
            let html = view::index(
                &self.pages,
                request.scope,
                &self.list,
                &self.columns,
                found,
                editor,
                self.pending.as_ref(),
                &self.success,
                &self.error,
            )
            .await?;
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                flash: redirect.as_ref().and(self.landing.take()),
                redirect,
                ..LiveBody::default()
            })
        })
    }
}
