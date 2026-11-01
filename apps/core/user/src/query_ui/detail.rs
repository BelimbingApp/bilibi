use super::*;
use bilibi_audit::console::{ConsoleResult, Outcome};
use bilibi_ui::{
    list_columns::{BuiltinColumn, ColumnsOutcome, ListColumns},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest},
    params, SortDir,
};
use serde_json::Value;
use std::sync::LazyLock;

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_size_aliases: vec!["perPage".into()],
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("query results spec")
});

pub(super) struct Detail {
    pages: Pages,
    pub saved: Option<queries::SavedQuery>,
    pub draft: queries::QueryInput,
    pub parameters: queries::Parameters,
    pub detected: Vec<String>,
    pub list: ListState,
    pub result: Option<ConsoleResult>,
    pub columns: Option<ListColumns>,
    pub sort: Option<String>,
    pub descending: bool,
    pub error: String,
    pub success: String,
    /// What the page a redirect lands on shows.
    landing: Option<(StatusKind, String)>,
    pub pending: bool,
    pub dirty: bool,
}
impl Detail {
    pub async fn load(
        pages: Pages,
        scope: &Scope,
        slug: &str,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        let saved = if slug == "_new" {
            None
        } else {
            Some(
                pages
                    .queries
                    .get(scope, queries::QueryKey::Slug(slug))
                    .await
                    .map_err(database_error)?,
            )
        };
        let draft = saved
            .as_ref()
            .map(queries::SavedQuery::input)
            .unwrap_or_else(|| queries::QueryInput {
                name: "Untitled Query".into(),
                ..Default::default()
            });
        let detected = queries::extract_named_parameters(&draft.sql_query);
        let parameters = detected.iter().map(|key| (key.clone(), None)).collect();
        let mut state = Self {
            pages,
            saved,
            draft,
            detected,
            parameters,
            list: parse(query, SPEC.clone())?,
            result: None,
            columns: None,
            sort: None,
            descending: false,
            error: String::new(),
            success: String::new(),
            landing: None,
            pending: false,
            dirty: false,
        };
        if !state.draft.sql_query.trim().is_empty() {
            state.run(scope).await?;
        }
        Ok(state)
    }
    async fn run(&mut self, scope: &Scope) -> Result<()> {
        self.error.clear();
        self.result = None;
        let options = queries::ConsoleOptions {
            parameters: self.parameters.clone(),
            order_by: self.sort.clone(),
            descending: self.descending,
        };
        let answer = self
            .pages
            .queries
            .execute(
                scope,
                queries::ConsoleRequest {
                    sql: &self.draft.sql_query,
                    name: Some(&self.draft.name),
                    page: self.list.page as i64,
                    per_page: self.list.page_size as i64,
                },
                &options,
                None,
            )
            .await;
        let answer = match answer {
            Ok(answer) => answer,
            Err(_) => {
                // If the recorder fails, C9 returns no answer. Keep the draft
                // visible instead of turning a recoverable run into a fatal mount.
                self.error = "The query could not be completed or audited. Your draft is still here; try again.".into();
                return Ok(());
            }
        };
        match answer.outcome {
            Outcome::Succeeded { .. } => {
                if let Some(result) = answer.result {
                    if self.list.clamp_to_last_page(result.last_page as u64, true) {
                        // C9 returns an empty out-of-bounds page; repeat against the
                        // clamped URL while retaining this page's unsaved draft.
                        return Box::pin(self.run(scope)).await;
                    }
                    let builtins = result
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(i, label)| {
                            let column = BuiltinColumn::new(&format!("c{i}"), label);
                            // C9 resolves a name to its first position. A later
                            // duplicate cannot be independently sorted by name.
                            if result.columns[..i].contains(label) {
                                column
                            } else {
                                column.sortable(label)
                            }
                        })
                        .collect::<Vec<_>>();
                    if !builtins.is_empty() && builtins.len() <= 2048 {
                        if self
                            .columns
                            .as_ref()
                            .is_none_or(|state| state.builtins() != builtins)
                        {
                            self.columns = Some(
                                ListColumns::projection("query-results-table", builtins)
                                    .map_err(|_| Error::Internal)?,
                            );
                        }
                    } else {
                        self.columns = None;
                    }
                    self.result = Some(result);
                }
            }
            Outcome::Refused { message, .. } | Outcome::Failed { message } => self.error = message,
        }
        Ok(())
    }
    fn change(&mut self, value: &Value) -> Result<()> {
        let sql_changed = value
            .get("sql_query")
            .and_then(Value::as_str)
            .is_some_and(|sql| sql != self.draft.sql_query);
        for (key, target, max) in [
            ("name", &mut self.draft.name, 600),
            ("description", &mut self.draft.description, 400_000),
            ("sql_query", &mut self.draft.sql_query, 400_000),
        ] {
            if let Some(value) = value.get(key) {
                let value = value.as_str().ok_or(Error::Invalid)?;
                if value.len() > max || value.contains('\0') {
                    return Err(Error::Invalid);
                }
                *target = value.into();
            }
        }
        self.detected = queries::extract_named_parameters(&self.draft.sql_query);
        self.parameters.retain(|key, _| self.detected.contains(key));
        for key in &self.detected {
            self.parameters.entry(key.clone()).or_insert(None);
        }
        self.dirty = self.saved.as_ref().is_none_or(|old| {
            old.name != self.draft.name
                || old.description != self.draft.description
                || old.sql_query != self.draft.sql_query
        });
        // A new projection may no longer contain the selected result column.
        if sql_changed {
            self.sort = None;
            self.descending = false;
        }
        Ok(())
    }
    async fn event(
        &mut self,
        scope: &Scope,
        value: &Value,
        editor: bool,
    ) -> Result<Option<String>> {
        let get = |key| value.get(key).and_then(Value::as_str);
        let kind = get("type").ok_or(Error::Invalid)?;
        if ["save", "duplicate", "request_delete", "delete"].contains(&kind) && !editor {
            self.error = "You are not authorized to modify queries.".into();
            self.pending = false;
            return Ok(None);
        }
        match kind {
            "change_field" => self.change(value)?,
            "change_param" => {
                for (key, value) in params::form_values_checked(value, "parameters", 100_000)
                    .map_err(bilibi_database::Error::refused)?
                {
                    if self.detected.contains(&key) {
                        if value.len() > 100_000 {
                            return Err(Error::Invalid);
                        }
                        self.parameters
                            .insert(key, if value.is_empty() { None } else { Some(value) });
                    }
                }
            }
            "run_query" => {
                self.list.page = 1;
                self.run(scope).await?;
            }
            "page_results" => {
                self.list.put_page(get("page"));
                self.run(scope).await?;
            }
            "result_page_size" => {
                self.list.apply_filters(
                    &params::form_values_checked(value, "filters", params::FIELD_LIMIT)
                        .map_err(bilibi_database::Error::refused)?,
                );
                self.run(scope).await?;
            }
            "results_grid" => {
                let column = if let Some(columns) = &mut self.columns {
                    match columns.handle(&values(value)) {
                        ColumnsOutcome::Sort(column) => Some(column),
                        _ => None,
                    }
                } else if get("op") == Some("sort") {
                    get("sort")
                        .and_then(|key| key.strip_prefix('c'))
                        .and_then(|index| index.parse::<usize>().ok())
                        .and_then(|index| self.result.as_ref()?.columns.get(index).cloned())
                } else {
                    None
                };
                if let Some(column) = column {
                    self.descending = self.sort.as_ref() == Some(&column) && !self.descending;
                    self.sort = Some(column);
                    self.list.page = 1;
                    self.run(scope).await?;
                }
            }
            "save" => {
                let saved = match &self.saved {
                    Some(old) => {
                        self.pages
                            .queries
                            .update(
                                scope,
                                queries::QueryKey::Id(old.id),
                                old.version,
                                &self.draft,
                            )
                            .await
                    }
                    None => self.pages.queries.create(scope, &self.draft).await,
                };
                match saved {
                    Ok(saved) => {
                        let redirect = self
                            .saved
                            .is_none()
                            .then(|| format!("{ROOT}/{}", saved.slug));
                        self.draft = saved.input();
                        self.saved = Some(saved);
                        self.dirty = false;
                        self.error.clear();
                        self.success = "Query saved.".into();
                        if redirect.is_some() {
                            self.landing = Some((StatusKind::Success, self.success.clone()));
                        }
                        return Ok(redirect);
                    }
                    Err(error) => self.error = message(error),
                }
            }
            "duplicate" => {
                if let Some(old) = &self.saved {
                    match self
                        .pages
                        .queries
                        .duplicate(scope, queries::QueryKey::Id(old.id))
                        .await
                    {
                        Ok(saved) => {
                            self.landing = Some((StatusKind::Success, "Query duplicated.".into()));
                            return Ok(Some(format!("{ROOT}/{}", saved.slug)));
                        }
                        Err(error) => self.error = message(error),
                    }
                }
            }
            "request_delete" => self.pending = self.saved.is_some(),
            "cancel_delete" => self.pending = false,
            "delete" => {
                if let Some(old) = &self.saved {
                    if self.pending {
                        self.pending = false;
                        match self
                            .pages
                            .queries
                            .delete(scope, queries::QueryKey::Id(old.id), old.version)
                            .await
                        {
                            Ok(_) => {
                                self.landing = Some((
                                    StatusKind::Success,
                                    format!("Query “{}” was deleted.", old.name),
                                ));
                                return Ok(Some(ROOT.into()));
                            }
                            Err(error) => self.error = message(error),
                        }
                    }
                } else {
                    self.landing = Some((StatusKind::Info, "Query discarded.".into()));
                    return Ok(Some(ROOT.into()));
                }
            }
            "clear-flash" => {
                self.success.clear();
                self.error.clear();
            }
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
    pub fn sort_dir(&self) -> Option<SortDir> {
        self.sort.as_ref().map(|_| {
            if self.descending {
                SortDir::Desc
            } else {
                SortDir::Asc
            }
        })
    }
}
impl LiveState<Scope, Error> for Detail {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let editor = self.pages.editor(request.scope).await?;
            if !editor {
                self.pending = false;
            }
            let mut redirect = None;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.list = parse(query, SPEC.clone())?;
                    self.run(request.scope).await?;
                }
                LiveInput::Event(value) => {
                    redirect = self.event(request.scope, value, editor).await?
                }
            }
            Ok(LiveBody {
                html: view::detail(self, editor)?,
                // Only a saved query has an address that can restore result navigation.
                query: if self.saved.is_some() {
                    self.list.to_params()
                } else {
                    HashMap::new()
                },
                flash: redirect.as_ref().and(self.landing.take()),
                redirect,
                rows: self.result.as_ref().map_or(0, |result| result.rows.len()),
                ..LiveBody::default()
            })
        })
    }
}
