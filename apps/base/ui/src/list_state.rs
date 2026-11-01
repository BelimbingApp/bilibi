//! Bounded URL state for operational lists, independent of storage and Grid.
//! A page supplies one validated spec, parses on HTTP/mount/Params, and returns
//! `to_params()` in LiveBody.query after every event. Partial toolbar forms go
//! through `apply_filters`, never through `parse` (which resets missing keys).
use crate::params;
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::Arc,
};

pub const MAX_PAGE: u64 = 1_000_000;
pub const MAX_PAGE_SIZE: u64 = 1_000;
pub const MAX_TEXT: usize = 255;
/// The form namespace of the toolbar and page-size fields (`filters[search]`); see `params::form_values`.
pub const FILTERS_FORM: &str = "filters";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}
impl SortDir {
    pub fn key(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
    fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            Some("asc") => Some(Self::Asc),
            Some("desc") => Some(Self::Desc),
            _ => None,
        }
    }
    fn flipped(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InvalidPageSize {
    #[default]
    Keep,
    Default,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Filter {
    OneOf {
        allowed: Vec<String>,
        default: String,
    },
    String {
        default: String,
        rewrite: BTreeMap<String, String>,
    },
}
impl Filter {
    pub fn one_of(allowed: &[&str], default: &str) -> Self {
        Self::OneOf {
            allowed: allowed.iter().map(|s| (*s).into()).collect(),
            default: default.into(),
        }
    }
    pub fn string(default: &str) -> Self {
        Self::String {
            default: default.into(),
            rewrite: BTreeMap::new(),
        }
    }
    pub fn default_value(&self) -> &str {
        match self {
            Self::OneOf { default, .. } | Self::String { default, .. } => default,
        }
    }
    fn parse(&self, value: Option<&str>) -> String {
        let Some(value) = value else {
            return self.default_value().into();
        };
        match self {
            Self::OneOf { allowed, default } => {
                if allowed.iter().any(|s| s == value) {
                    value.into()
                } else {
                    default.clone()
                }
            }
            Self::String { rewrite, .. } => {
                bounded(rewrite.get(value).map_or(value, String::as_str))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParamNames {
    pub search: String,
    pub page: String,
    pub sort_by: String,
    pub sort_dir: String,
}
impl Default for ParamNames {
    fn default() -> Self {
        Self {
            search: "search".into(),
            page: "page".into(),
            sort_by: "sort_by".into(),
            sort_dir: "sort_dir".into(),
        }
    }
}

/// Preserve a page's existing URL names. Prefixes allow independent lists to
/// share one query. Unknown input keys are ignored and never copied back out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListSpec {
    pub sortable: BTreeMap<String, SortDir>,
    pub default_sort: Option<String>,
    pub page_sizes: Vec<u64>,
    pub default_page_size: u64,
    pub page_size_param: String,
    pub page_size_aliases: Vec<String>,
    pub invalid_page_size: InvalidPageSize,
    pub filters: BTreeMap<String, Filter>,
    pub omit_blank: Vec<String>,
    pub param_prefix: String,
    pub param_names: ParamNames,
    pub omit_defaults: bool,
}
impl Default for ListSpec {
    fn default() -> Self {
        Self {
            sortable: BTreeMap::new(),
            default_sort: None,
            page_sizes: vec![25, 50, 100, 300],
            default_page_size: 25,
            page_size_param: "page_size".into(),
            page_size_aliases: vec![],
            invalid_page_size: InvalidPageSize::Keep,
            filters: BTreeMap::new(),
            omit_blank: vec![],
            param_prefix: String::new(),
            param_names: ParamNames::default(),
            omit_defaults: false,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpecError(pub &'static str);
impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for SpecError {}

impl ListSpec {
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.page_sizes.is_empty()
            || self
                .page_sizes
                .iter()
                .any(|n| *n == 0 || *n > MAX_PAGE_SIZE)
            || !self.page_sizes.contains(&self.default_page_size)
        {
            return Err(SpecError(
                "page sizes must be 1..=1000 and contain the default",
            ));
        }
        if !match &self.default_sort {
            Some(key) => self.sortable.contains_key(key),
            None => self.sortable.is_empty(),
        } {
            return Err(SpecError("default sort must name an allowed column"));
        }
        let names = &self.param_names;
        let mut keys = vec![
            names.search.as_str(),
            names.page.as_str(),
            names.sort_by.as_str(),
            names.sort_dir.as_str(),
            self.page_size_param.as_str(),
        ];
        keys.extend(self.page_size_aliases.iter().map(String::as_str));
        keys.extend(self.filters.keys().map(String::as_str));
        let count = keys.len();
        if keys.iter().any(|key| key.is_empty() || key.len() > 80)
            || self.param_prefix.len() > 80
            || count > 64
        {
            return Err(SpecError("query names must be nonempty and bounded"));
        }
        keys.sort_unstable();
        keys.dedup();
        if keys.len() != count {
            return Err(SpecError("query names and aliases must be unique"));
        }
        if self
            .omit_blank
            .iter()
            .any(|key| key != "search" && !self.filters.contains_key(key))
        {
            return Err(SpecError("omit_blank must name search or a filter"));
        }
        if self
            .filters
            .values()
            .any(|filter| filter.default_value().chars().count() > MAX_TEXT)
        {
            return Err(SpecError("filter defaults exceed the text bound"));
        }
        Ok(())
    }
    pub fn shared(self) -> Result<Arc<Self>, SpecError> {
        self.validate()?;
        Ok(Arc::new(self))
    }
    fn key(&self, key: &str) -> String {
        format!("{}{key}", self.param_prefix)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListState {
    pub search: String,
    pub page: u64,
    pub page_size: u64,
    pub sort_by: Option<String>,
    pub sort_dir: Option<SortDir>,
    pub filters: BTreeMap<String, String>,
    spec: Arc<ListSpec>,
}
impl ListState {
    /// Untrusted values always fall back; invalid application specs return an error.
    pub fn parse(values: &HashMap<String, String>, spec: Arc<ListSpec>) -> Result<Self, SpecError> {
        spec.validate()?;
        let get = |name: &str| values.get(&spec.key(name)).map(String::as_str);
        let sort_by = get(&spec.param_names.sort_by)
            .filter(|key| spec.sortable.contains_key(*key))
            .map(str::to_owned)
            .or_else(|| spec.default_sort.clone());
        let sort_dir = sort_by.as_ref().map(|key| {
            SortDir::parse(get(&spec.param_names.sort_dir)).unwrap_or(spec.sortable[key])
        });
        let size = get(&spec.page_size_param)
            .or_else(|| spec.page_size_aliases.iter().find_map(|alias| get(alias)));
        Ok(Self {
            search: bounded(get(&spec.param_names.search).unwrap_or("")),
            page: bounded_page(get(&spec.param_names.page)),
            page_size: accept_size(size, spec.default_page_size, &spec),
            sort_by,
            sort_dir,
            filters: spec
                .filters
                .iter()
                .map(|(key, filter)| (key.clone(), filter.parse(get(key))))
                .collect(),
            spec,
        })
    }
    pub fn spec(&self) -> &ListSpec {
        &self.spec
    }
    pub fn to_params(&self) -> HashMap<String, String> {
        let spec = &self.spec;
        let mut params = HashMap::new();
        let mut put = |key: &str, value: String, omit: bool| {
            if !omit {
                params.insert(spec.key(key), value);
            }
        };
        put(
            &spec.param_names.page,
            self.page.to_string(),
            spec.omit_defaults && self.page == 1,
        );
        put(
            &spec.page_size_param,
            self.page_size.to_string(),
            spec.omit_defaults && self.page_size == spec.default_page_size,
        );
        if let Some(key) = &self.sort_by {
            put(
                &spec.param_names.sort_by,
                key.clone(),
                spec.omit_defaults && Some(key) == spec.default_sort.as_ref(),
            );
            if let Some(dir) = self.sort_dir {
                put(
                    &spec.param_names.sort_dir,
                    dir.key().into(),
                    spec.omit_defaults && Some(&dir) == spec.sortable.get(key),
                );
            }
        }
        put(
            &spec.param_names.search,
            self.search.clone(),
            self.search.is_empty()
                && (spec.omit_defaults || spec.omit_blank.iter().any(|key| key == "search")),
        );
        for (key, value) in &self.filters {
            put(
                key,
                value.clone(),
                (spec.omit_defaults && value == spec.filters[key].default_value())
                    || (value.is_empty() && spec.omit_blank.contains(key)),
            );
        }
        params
    }
    /// Form names are search/perPage and unprefixed filter names, regardless of
    /// query aliases. Missing fields retain current values; every submit resets page.
    pub fn apply_filters(&mut self, posted: &HashMap<String, String>) {
        if let Some(search) = posted.get("search") {
            self.search = bounded(search);
        }
        if let Some(size) = posted.get("perPage") {
            let fallback = match self.spec.invalid_page_size {
                InvalidPageSize::Keep => self.page_size,
                InvalidPageSize::Default => self.spec.default_page_size,
            };
            self.page_size = accept_size(Some(size), fallback, &self.spec);
        }
        for (key, filter) in &self.spec.filters {
            if let Some(value) = posted.get(key) {
                self.filters.insert(key.clone(), filter.parse(Some(value)));
            }
        }
        self.page = 1;
    }
    pub fn filters_form(&self) -> HashMap<String, String> {
        let mut form: HashMap<_, _> = self.filters.clone().into_iter().collect();
        form.insert("search".into(), self.search.clone());
        form.insert("perPage".into(), self.page_size.to_string());
        form
    }
    pub fn next_sort(&mut self, column: &str) -> bool {
        let Some(default) = self.spec.sortable.get(column).copied() else {
            return false;
        };
        self.sort_dir = Some(if self.sort_by.as_deref() == Some(column) {
            self.sort_dir.unwrap_or(default).flipped()
        } else {
            default
        });
        self.sort_by = Some(column.into());
        self.page = 1;
        true
    }
    pub fn put_page(&mut self, page: Option<&str>) {
        self.page = bounded_page(page);
    }
    pub fn offset(&self) -> u64 {
        self.page.saturating_sub(1).saturating_mul(self.page_size)
    }
    /// Returns whether the owner should requery/patch the corrected page.
    pub fn clamp_to_last_page(&mut self, total_pages: u64, reset_empty: bool) -> bool {
        let old = self.page;
        if total_pages > 0 {
            self.page = self.page.min(total_pages);
        } else if reset_empty {
            self.page = 1;
        }
        old != self.page
    }
    /// For already filtered/sorted in-memory rows; this never queries storage.
    pub fn paginate<'a, T>(&mut self, rows: &'a [T]) -> ListPage<'a, T> {
        let total = rows.len() as u64;
        let pages = total.div_ceil(self.page_size);
        self.clamp_to_last_page(pages, true);
        let start = self.offset().min(total) as usize;
        let end = (self.offset().saturating_add(self.page_size)).min(total) as usize;
        ListPage {
            entries: &rows[start..end],
            counts: PageCounts::new(self.page, self.page_size, total),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageCounts {
    pub page: u64,
    pub page_size: u64,
    pub total_entries: u64,
    pub total_pages: u64,
}
impl PageCounts {
    pub fn new(page: u64, page_size: u64, total_entries: u64) -> Self {
        let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
        let total_pages = total_entries.div_ceil(page_size);
        Self {
            page: if total_pages == 0 {
                1
            } else {
                page.clamp(1, total_pages)
            },
            page_size,
            total_entries,
            total_pages,
        }
    }
    pub fn has_prev(&self) -> bool {
        self.total_pages > 0 && self.page > 1
    }
    pub fn has_next(&self) -> bool {
        self.page < self.total_pages
    }
    pub fn summary(&self) -> String {
        if self.total_entries == 0 {
            return "No results".into();
        }
        let first = (self.page - 1)
            .saturating_mul(self.page_size)
            .saturating_add(1);
        let last = self
            .page
            .saturating_mul(self.page_size)
            .min(self.total_entries);
        format!(
            "Showing {first} to {last} of {} results",
            self.total_entries
        )
    }
    pub fn steps(&self) -> Vec<Option<u64>> {
        if self.total_pages == 0 {
            return vec![];
        }
        let mut pages: Vec<_> = if self.total_pages <= 5 {
            (1..=self.total_pages).collect()
        } else {
            vec![
                1,
                2,
                self.page.saturating_sub(1),
                self.page,
                self.page.saturating_add(1),
                self.total_pages,
            ]
        };
        pages.retain(|p| *p >= 1 && *p <= self.total_pages);
        pages.sort_unstable();
        pages.dedup();
        let mut steps = vec![];
        let mut previous = 0;
        for page in pages {
            if previous != 0 && page > previous + 1 {
                steps.push(None)
            }
            steps.push(Some(page));
            previous = page;
        }
        steps
    }
}
pub struct ListPage<'a, T> {
    pub entries: &'a [T],
    pub counts: PageCounts,
}
fn bounded_page(value: Option<&str>) -> u64 {
    params::positive_integer(value).unwrap_or(1).min(MAX_PAGE)
}
fn accept_size(value: Option<&str>, fallback: u64, spec: &ListSpec) -> u64 {
    params::positive_integer(value)
        .filter(|n| spec.page_sizes.contains(n))
        .unwrap_or(fallback)
}
fn bounded(value: &str) -> String {
    value.chars().take(MAX_TEXT).collect()
}
