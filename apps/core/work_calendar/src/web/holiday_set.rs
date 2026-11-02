//! `/holiday-sets/{code}?year=YYYY`: one holiday set read first, with its
//! holidays a year at a time and the calendars that include it. A person who
//! may manage calendars renames it and changes its country and division in
//! place, adds a holiday, renames one in its row and removes one (confirmed).
//! The default year is this year in the company's zone.
use super::holiday_sets::{countries, divisions, shown};
use super::*;
use crate::{
    contributions::{CALENDARS_PATH, SETS_PATH},
    Holiday, HolidaySet, MANAGE,
};
use bilibi_ui::{
    params, ActionLink, BackLink, BuiltinColumn, Button, Card, Choice, ColumnsOutcome, Combobox,
    CommitStates, ConfirmDialog, Fact, Facts, Field, FlexRow, FlexTable, Header, IconButton,
    IconKind, InlineChoice, InlineEdit, InlineField, InputKind, ListColumns, Modal, RecordLink,
    SectionHeading, TableEmpty,
};
use serde_json::json;

const NOUN: &str = "holiday set";
const SHARED_READONLY: &str = "The platform operator keeps this installation-wide holiday set. Copy it to make changes of your own.";

fn holiday_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("date", "Date"),
        BuiltinColumn::new("name", "Name"),
    ]
}
/// The years the page offers.
const YEARS: std::ops::RangeInclusive<i16> = 1900..=2999;
fn year(query: &HashMap<String, String>) -> Option<i16> {
    query
        .get("year")
        .and_then(|year| year.parse().ok())
        .filter(|year| YEARS.contains(year))
}
/// The calendars a set is included in: their links in a list sentence.
#[derive(Template)]
#[template(
    source = r#"{% for link in links %}{% if !loop.first %}, {% endif %}{{ link }}{% endfor %}{% if more > 0 %} and {{ more }} more{% endif %}"#,
    ext = "html"
)]
struct Including {
    links: Vec<Html>,
    more: i64,
}

/// The confirmed removal: the plan the dialog commits.
struct Removal {
    version: i64,
    holiday: Holiday,
}

pub(super) struct Record {
    pages: Pages,
    code: String,
    year: Option<i16>,
    set: Option<HolidaySet>,
    statuses: CommitStates,
    notice: String,
    flash: Flash,
    /// The add form's values and refusals, kept after a refused add.
    draft: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
    confirm: Option<Removal>,
    copy: Option<Draft>,
    redirect: Option<String>,
    columns: ListColumns,
    rows_changed: bool,
    framed: bool,
    /// The place choices of the in-place editors, read on the first load
    /// and the divisions again when the set's country changes.
    countries: Vec<Choice>,
    divisions: (Option<String>, Vec<Choice>),
}
impl Record {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            pages,
            code: code(request.path)?,
            year: year(request.query),
            set: None,
            statuses: CommitStates::default(),
            notice: String::new(),
            flash: Flash::default(),
            draft: HashMap::new(),
            errors: BTreeMap::new(),
            confirm: None,
            copy: None,
            redirect: None,
            columns: ListColumns::mount("holiday-set-holidays", holiday_columns())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            framed: request.framed,
            countries: Vec::new(),
            divisions: (None, Vec::new()),
        })
    }
    fn set(&self) -> Result<&HolidaySet> {
        self.set.as_ref().ok_or(Error::Internal)
    }
    async fn writable(&self, scope: &Scope) -> Result<bool> {
        if !self.pages.allowed(scope, MANAGE).await? {
            return Ok(false);
        }
        if !self.set()?.installation {
            return Ok(true);
        }
        let client = self.pages.db.get().await?;
        match bilibi_tenancy::prove_operator(&**client, scope).await {
            Ok(_) => Ok(effective_capabilities(&**client, scope)
                .await?
                .contains(crate::INSTALLATION_MANAGE)),
            Err(Error::Forbidden) => Ok(false),
            Err(error) => Err(error),
        }
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let year = match self.year {
            Some(year) => year,
            None => self.pages.today(scope).await?.year(),
        };
        self.year = Some(year);
        let (from, to) = (
            Date::new(year, 1, 1).map_err(|_| Error::Invalid)?,
            Date::new(year, 12, 31).map_err(|_| Error::Invalid)?,
        );
        let set = self
            .pages
            .calendars
            .holiday_set(scope, &self.code, Some(from), Some(to), 1)
            .await?;
        if self.rows_changed {
            self.columns
                .load(&set.holidays.rows, |h| h.date.to_string());
        }
        if self.countries.is_empty() {
            self.countries = countries(&self.pages.geonames).await?;
        }
        if self.divisions.0 != set.country_iso {
            let country = set.country_iso.clone().unwrap_or_default();
            self.divisions = (
                set.country_iso.clone(),
                divisions(&self.pages.geonames, &country).await?,
            );
        }
        self.set = Some(set);
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        self.notice.clear();
        let name = event(value)?;
        match name {
            "holidays_grid" => {
                self.rows_changed = false;
                return match self.columns.handle(
                    &params::string_values_checked(value, params::FIELD_LIMIT)
                        .map_err(Error::refused)?,
                ) {
                    ColumnsOutcome::Sort(_) => Err(Error::Invalid),
                    ColumnsOutcome::Update | ColumnsOutcome::Noop => Ok(()),
                };
            }
            "clear-flash" => {
                self.flash.dismiss(text(value, "key"));
                return Ok(());
            }
            "cancel_confirm" => {
                self.confirm = None;
                return Ok(());
            }
            "close_copy" => {
                self.copy = None;
                return Ok(());
            }
            _ => {}
        }
        if !self.pages.allowed(scope, MANAGE).await? {
            self.confirm = None;
            self.statuses.clear_saved();
            self.notice = DENIED.into();
            self.flash.put(StatusKind::Error, DENIED);
            return Ok(());
        }
        let code = self.code.clone();
        if name == "open_copy" {
            self.copy = Some(Draft::default());
            return Ok(());
        }
        if name == "copy_holiday_set" {
            let posted = form(value, "copy")?;
            let field = |key: &str| posted.get(key).map_or("", String::as_str).trim();
            let destination = field("code").to_owned();
            let mut input = json!({"source": code, "code": destination, "name": field("name")});
            for key in ["from", "to"] {
                if !field(key).is_empty() {
                    input[key] = json!(field(key));
                }
            }
            match self
                .pages
                .submit(
                    scope,
                    credential,
                    name,
                    "core.work_calendar.holiday_set.copy",
                    input,
                )
                .await?
            {
                Ok(_) => self.redirect = Some(format!("{SETS_PATH}/{destination}")),
                Err(refusal @ Refusal::Invalid(_)) => {
                    let (errors, rest) =
                        refusal.fields(|key| ["code", "name", "from", "to"].contains(&key));
                    if !rest.is_empty() {
                        self.flash.put(StatusKind::Error, rest);
                    }
                    self.copy = Some(Draft {
                        values: posted,
                        errors,
                    });
                }
                Err(refusal) => self.flash.put(StatusKind::Error, refusal.sentence(NOUN)),
            }
            return Ok(());
        }
        if !self.writable(scope).await? {
            self.confirm = None;
            self.notice = SHARED_READONLY.into();
            self.flash.put(StatusKind::Error, SHARED_READONLY);
            return Ok(());
        }
        let contract = "core.work_calendar.holiday_set.holidays";
        match name {
            "save_holiday_set" | "save_place" => {
                let field = text(value, "field");
                if text(value, "id") != code
                    || !match name {
                        "save_holiday_set" => field == "name",
                        _ => field == "country_iso" || field == "admin1_code",
                    }
                {
                    return Err(Error::Invalid);
                }
                let submitted = text(value, "value").trim();
                let mut input = json!({"code": code, "version": version(value)?});
                let label = match field {
                    "name" => {
                        input["name"] = json!(submitted);
                        "name"
                    }
                    // A new country starts with no division.
                    "country_iso" => {
                        input["country_iso"] = json!(submitted);
                        "country"
                    }
                    _ => {
                        let country = self.set()?.country_iso.clone().unwrap_or_default();
                        input["country_iso"] = json!(country);
                        input["admin1_code"] = json!(submitted);
                        "division"
                    }
                };
                let result = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.holiday_set.update",
                        input,
                    )
                    .await?;
                let status = commit_status(
                    result,
                    label,
                    submitted,
                    NOUN,
                    &mut self.notice,
                    &mut self.statuses,
                );
                self.statuses.put(field, status);
            }
            "add_holiday" => {
                let posted = form(value, "holiday")?;
                let field = |key: &str| posted.get(key).map_or("", String::as_str).trim();
                let input = json!({
                    "code": code, "version": self.set()?.version,
                    "add": [{"date": field("date"), "name": field("name")}],
                });
                let added = format!("{} is added.", field("name"));
                let year = field("date")
                    .parse::<Date>()
                    .ok()
                    .map(|date| date.year())
                    .filter(|year| YEARS.contains(year));
                match self
                    .pages
                    .submit(scope, credential, name, contract, input)
                    .await?
                {
                    Ok(_) => {
                        // The page moves to the holiday's year, so what was
                        // added is shown.
                        let message = match year {
                            Some(year) if Some(year) != self.year => {
                                self.year = Some(year);
                                format!("{added} Showing {year}.")
                            }
                            Some(_) => added,
                            None => format!(
                                "{added} This page shows the years {} to {}, so it is not listed.",
                                YEARS.start(),
                                YEARS.end()
                            ),
                        };
                        self.flash.put(StatusKind::Success, message);
                        self.errors.clear();
                        self.draft.clear();
                    }
                    Err(refusal @ Refusal::Invalid(_)) => {
                        let (errors, rest) = refusal.fields(|key| key == "date" || key == "name");
                        if rest.is_empty() {
                            self.flash.clear();
                        } else {
                            self.flash.put(StatusKind::Error, rest);
                        }
                        self.errors = errors;
                        self.draft = posted;
                    }
                    Err(refusal) => {
                        self.flash.put(StatusKind::Error, refusal.sentence(NOUN));
                        self.draft = posted;
                    }
                }
            }
            "save_holiday" => {
                let date: Date = text(value, "id").parse().map_err(|_| Error::Invalid)?;
                if text(value, "field") != "name" {
                    return Err(Error::Invalid);
                }
                let submitted = text(value, "value").trim();
                let input = json!({
                    "code": code, "version": version(value)?,
                    "add": [{"date": date.to_string(), "name": submitted}],
                });
                let result = self
                    .pages
                    .submit(scope, credential, name, contract, input)
                    .await?;
                let status = commit_status(
                    result,
                    "name",
                    submitted,
                    NOUN,
                    &mut self.notice,
                    &mut self.statuses,
                );
                self.statuses.put(format!("holiday-{date}"), status);
            }
            "request_remove_holiday" => {
                let date: Date = text(value, "key").parse().map_err(|_| Error::Invalid)?;
                let set = self.set()?;
                let version = set.version;
                let Some(holiday) = set.holidays.rows.iter().find(|h| h.date == date).cloned()
                else {
                    self.flash
                        .put(StatusKind::Error, "That holiday is no longer in this set.");
                    return Ok(());
                };
                match self
                    .pages
                    .preview(
                        scope,
                        credential,
                        name,
                        contract,
                        json!({"code": code, "version": version, "remove": [date.to_string()]}),
                    )
                    .await?
                {
                    Ok(()) => {
                        self.flash.clear();
                        self.confirm = Some(Removal { version, holiday });
                    }
                    Err(refusal) => self.flash.put(StatusKind::Error, refusal.sentence(NOUN)),
                }
            }
            "remove_holiday" => {
                let Some(removal) = self.confirm.take() else {
                    return Ok(());
                };
                let result = self
                    .pages
                    .commit(
                        scope,
                        credential,
                        name,
                        contract,
                        json!({"code": code, "version": removal.version, "remove": [removal.holiday.date.to_string()]}),
                    )
                    .await?;
                match result {
                    Ok(_) => self.flash.put(
                        StatusKind::Success,
                        format!("{} is removed.", removal.holiday.name),
                    ),
                    Err(refusal) => self.flash.put(StatusKind::Error, refusal.sentence(NOUN)),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    fn facts(&self, set: &HolidaySet, manage: bool) -> Result<Html> {
        let inline = |id: &str, name: &str, label: &str, value: &str| {
            let mut field = InlineField::new(id, name, label, value)
                .version(set.version)
                .status(self.statuses.get(name));
            field.id_value = set.code.clone();
            field.save_event = if name == "name" {
                "save_holiday_set"
            } else {
                "save_place"
            }
            .into();
            if !manage {
                field = field.readonly();
            }
            field
        };
        let country = set.country_iso.clone().unwrap_or_default();
        let division = set.admin1_code.clone().unwrap_or_default();
        // The editor of a choice fact: its name is the fact's field.
        let editor = |name: &str, label: &str, value: &str, options: &[Choice]| {
            render(
                Combobox::new(
                    format!("holiday-set-{}-select", name.replace('_', "-")),
                    name,
                )
                .options(options.to_vec())
                .value(value)
                .placeholder(format!("Choose a {}", label.to_lowercase()))
                .aria_label(label)
                .wrapper_class("mb-0"),
            )
        };
        let mut division_field = inline(
            "holiday-set-division",
            "admin1_code",
            "Edit division",
            &division,
        )
        .allow_empty();
        if country.is_empty() {
            division_field = division_field.readonly();
        }
        // The calendars as links, as their list draws them.
        let mut links = Vec::with_capacity(set.calendars.rows.len());
        for code in &set.calendars.rows {
            links.push(render(RecordLink {
                workspace: self.framed,
                kind: "core/work_calendar",
                record_id: code,
                href: &format!("{CALENDARS_PATH}/{code}"),
                class: "font-mono text-xs font-medium text-action hover:underline",
                body: Html::text(code),
            })?);
        }
        let including = if links.is_empty() {
            Html::text("No calendar in your workspace includes it yet.")
        } else {
            render(Including {
                links,
                more: (set.calendars.total - set.calendars.rows.len() as i64).max(0),
            })?
        };
        let facts = render(Facts {
            id: "holiday-set-facts",
            items: vec![
                Fact {
                    title: "Code",
                    id: "holiday-set-code",
                    value: Html::text(&set.code),
                },
                Fact {
                    title: "Name",
                    id: "holiday-set-name-fact",
                    value: render(InlineEdit {
                        field: inline("holiday-set-name", "name", "Edit name", &set.name),
                    })?,
                },
                Fact {
                    title: "Country",
                    id: "holiday-set-country-fact",
                    value: render(InlineChoice {
                        field: inline(
                            "holiday-set-country",
                            "country_iso",
                            "Edit country",
                            &country,
                        )
                        .allow_empty(),
                        display: Html::text(&shown(&self.countries, &country)),
                        editor: editor("country_iso", "Country", &country, &self.countries)?,
                    })?,
                },
                Fact {
                    title: "Division",
                    id: "holiday-set-division-fact",
                    value: render(InlineChoice {
                        field: division_field,
                        display: Html::text(&shown(&self.divisions.1, &division)),
                        editor: editor("admin1_code", "Division", &division, &self.divisions.1)?,
                    })?,
                },
                Fact {
                    title: "Included in your workspace",
                    id: "holiday-set-calendars",
                    value: including,
                },
            ],
        })?;
        section(
            SectionHeading {
                id: "holiday-set-details-heading",
                title: "Details",
                description: if set.installation && !manage {
                    SHARED_READONLY
                } else {
                    ""
                },
                ..SectionHeading::default()
            },
            vec![render(Card {
                id: "holiday-set-details",
                role: "region",
                labelledby: "holiday-set-details-heading",
                body: facts,
                ..Card::default()
            })?],
        )
    }
    fn holidays(&self, set: &HolidaySet, manage: bool, year: i16) -> Result<Html> {
        let mut rows = Vec::with_capacity(set.holidays.rows.len());
        for holiday in &set.holidays.rows {
            let date = holiday.date.to_string();
            let key = format!("holiday-{date}");
            let mut field = InlineField::new(
                format!("holiday-set-holiday-{date}"),
                "name",
                format!("Edit the name of {}", long_date(holiday.date)),
                &holiday.name,
            )
            .version(set.version)
            .status(self.statuses.get(&key));
            field.id_value = date.clone();
            field.save_event = "save_holiday".into();
            if !manage {
                field = field.readonly();
            }
            let mut row = FlexRow::new(date.clone())
                .slot(
                    "date",
                    Html::text(&holiday.date.strftime("%a %-d %B %Y").to_string()),
                )
                .slot("name", render(InlineEdit { field })?);
            if manage {
                row.actions = render(IconButton {
                    id: &format!("holiday-set-remove-{date}"),
                    kind: IconKind::Danger,
                    event: "request_remove_holiday",
                    name: "key",
                    value: &date,
                    ..IconButton::new("delete", &format!("Remove {}", holiday.name))
                })?;
            }
            row.dom_id = Some(format!("holiday-set-row-{date}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("holiday-set-holidays", self.columns.flex_columns(), rows);
        table.event = "holidays_grid";
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Holidays";
        table.framed = false;
        table.actions = manage;
        if set.holidays.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(&format!(
                "No holidays in {year}."
            ))));
        }
        let link = |id: &'static str, year: i16| {
            render(ActionLink {
                id,
                href: &format!("{SETS_PATH}/{}?year={year}", set.code),
                body: Html::text(&year.to_string()),
                ..ActionLink::default()
            })
        };
        let mut parts = Vec::new();
        if YEARS.contains(&(year - 1)) {
            parts.push(link("holiday-set-year-previous", year - 1)?);
        }
        if YEARS.contains(&(year + 1)) {
            parts.push(link("holiday-set-year-next", year + 1)?);
        }
        let years = render(Cluster { parts })?;
        let mut cards = Vec::new();
        if manage {
            let value = |key: &str| self.draft.get(key).cloned().unwrap_or_default();
            let errors = |key: &str| self.errors.get(key).cloned().unwrap_or_default();
            let form = render(DialogForm {
                id: "holiday-set-add-form",
                event: "add_holiday",
                rows: vec![vec![
                    render(
                        Field::in_form("holiday", "date")
                            .id("holiday-set-add-date")
                            .label("Date")
                            .kind(InputKind::Date)
                            .value(value("date"))
                            .errors(errors("date"))
                            .required(),
                    )?,
                    render(
                        Field::in_form("holiday", "name")
                            .id("holiday-set-add-name")
                            .label("Holiday")
                            .value(value("name"))
                            .attr("maxlength", "120")
                            .hint("A date the set already holds is renamed.")
                            .errors(errors("name"))
                            .required(),
                    )?,
                ]],
                actions: render(Button {
                    id: "holiday-set-add",
                    button_type: "submit",
                    working: "Adding…",
                    body: Html::text("Add holiday"),
                    ..Button::default()
                })?,
                ..DialogForm::default()
            })?;
            cards.push(render(Card {
                id: "holiday-set-add-card",
                body: form,
                ..Card::default()
            })?);
        }
        cards.push(render(Card {
            id: "holiday-set-holidays-card",
            inner_class: "p-0",
            role: "region",
            labelledby: "holiday-set-holidays-heading",
            body: render(table)?,
            ..Card::default()
        })?);
        section(
            SectionHeading {
                id: "holiday-set-holidays-heading",
                title: &format!("Holidays in {year}"),
                count: Some(set.holidays.total as usize),
                description: "Whole days. Calendars that include this set close on them.",
                actions: years,
                ..SectionHeading::default()
            },
            cards,
        )
    }

    async fn view(&self, scope: &Scope) -> Result<String> {
        let set = self.set()?;
        let year = self.year.ok_or(Error::Internal)?;
        let manage = self.writable(scope).await?;
        let mut actions = render(BackLink {
            href: SETS_PATH,
            id: "holiday-set-back",
            title: "Back to holiday sets",
            ..BackLink::default()
        })?;
        if self.pages.allowed(scope, MANAGE).await? {
            actions.push(&render(Button {
                id: "holiday-set-copy",
                event: "open_copy",
                body: Html::text("Copy"),
                ..Button::default()
            })?);
        }
        let header = render(Header {
            title: Html::text(&set.name),
            subtitle: if set.installation {
                "Installation-wide holiday set · Available to every company"
            } else {
                "Holiday set"
            },
            actions,
            ..Header::default()
        })?;
        let dialog = match &self.confirm {
            Some(removal) => render(ConfirmDialog {
                id: "holiday-set-remove-confirm",
                consequence: &format!(
                    "Remove {} ({})?",
                    removal.holiday.name,
                    long_date(removal.holiday.date)
                ),
                detail: if set.installation {
                    "This changes calendars in every company that includes this installation-wide set. They work that day again, unless another set or an exception closes it."
                } else {
                    "Calendars that include this set work that day again, unless another set or an exception closes it."
                },
                confirm: "Remove",
                working: "Removing…",
                confirm_event: "remove_holiday",
                cancel_event: "cancel_confirm",
            })?,
            None => match &self.copy {
                Some(draft) => self.copy_dialog(draft)?,
                None => Html::EMPTY,
            },
        };
        View {
            id: "holiday-set-page",
            variant: PageVariant::Detail,
            flash: self.flash.map(),
            // A set is no workspace record kind (see the list).
            record_kind: "",
            record_id: "",
            header,
            sections: vec![self.facts(set, manage)?, self.holidays(set, manage, year)?],
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn copy_dialog(&self, draft: &Draft) -> Result<Html> {
        let field = |key: &str, label: &str| {
            Field::in_form("copy", key)
                .id(format!("holiday-set-copy-{key}"))
                .label(label)
                .value(draft.values.get(key).cloned().unwrap_or_default())
                .errors(draft.errors.get(key).cloned().unwrap_or_default())
        };
        render(Modal {
            id: "holiday-set-copy-dialog",
            title: "Copy holiday set",
            description: "Create a local set that no longer follows the original. Calendars keep using the original until you change their included sets.",
            cancel_event: "close_copy",
            flash: Some(self.flash.map()),
            body: render(DialogForm {
                id: "holiday-set-copy-form",
                event: "copy_holiday_set",
                rows: vec![
                    vec![render(field("code", "Code").attr("maxlength", "64").required())?, render(field("name", "Name").attr("maxlength", "120").required())?],
                    vec![render(field("from", "From").kind(InputKind::Date))?, render(field("to", "To").kind(InputKind::Date).hint("Optional dates. Copy at most 400 holidays; narrow the dates for a larger set."))?],
                ],
                actions: dialog_actions("Copy set", "Copying…", "close_copy")?,
                ..DialogForm::default()
            })?,
            ..Modal::default()
        })
    }
}
impl Titled for Record {
    fn title(&self) -> Option<String> {
        self.set.as_ref().map(|set| set.name.clone())
    }
}
impl LiveState<Scope, Error> for Record {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.year = year(values),
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            if let Some(path) = self.redirect.take() {
                return Ok(LiveBody {
                    redirect: Some(path),
                    flash: Some((StatusKind::Success, "Holiday set copied. Calendars still use the original until you change their included sets.".into())),
                    ..LiveBody::default()
                });
            }
            self.load(request.scope).await?;
            let html = self.view(request.scope).await?;
            let query = self
                .year
                .map(|year| HashMap::from([("year".to_owned(), year.to_string())]))
                .unwrap_or_default();
            let rows = self.set()?.holidays.rows.len();
            Ok(body(html, query, rows, &self.notice))
        })
    }
}
