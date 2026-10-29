//! `/work-calendars/{code}`: one calendar read first. A person who may
//! manage calendars renames it and changes its holiday substitution in place,
//! retires it (confirmed) or reactivates it, sets the week from a date,
//! includes or leaves out holiday sets, and adds or removes (confirmed) dated
//! exceptions. Its exceptions page 400 at a time (`?page=`).
use super::*;
use crate::{
    contributions::{CALENDARS_PATH, SETS_PATH},
    resolve::in_force,
    Calendar, Exception, Listing, LIST, MANAGE, MAX_PAGE_SIZE, MAX_ROWS,
};
use bilibi_ui::{
    params, ActionLink, BackLink, BuiltinColumn, Button, ButtonVariant, Card, Choice,
    ColumnsOutcome, Combobox, CommitStates, Fact, Facts, Field, FlexRow, FlexTable, Header,
    IconButton, IconKind, InlineChoice, InlineEdit, InlineField, InputKind, ListColumns, Modal,
    ModalWidth, SectionHeading, TableEmpty,
};
use serde_json::json;

const NOUN: &str = "calendar";

fn rule_columns() -> Vec<BuiltinColumn> {
    let mut columns = vec![
        BuiltinColumn::new("from", "From"),
        BuiltinColumn::new("timezone", "Zone"),
    ];
    for (day, name) in week::DAYS {
        columns.push(BuiltinColumn::new(day, name));
    }
    columns
}
fn set_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("name", "Name"),
    ]
}
fn exception_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("dates", "Dates"),
        BuiltinColumn::new("name", "Name"),
        BuiltinColumn::new("type", "Type"),
        BuiltinColumn::new("hours", "Hours"),
    ]
}
/// "3 November 2026", or "28 December 2026 to 1 January 2027".
pub(super) fn dates(from: Date, to: Date) -> String {
    if from == to {
        long_date(from)
    } else {
        format!("{} to {}", long_date(from), long_date(to))
    }
}
enum Dialog {
    None,
    Rule(Draft),
    Exception(Draft),
    Include(Draft),
    /// A reviewed retirement: the plan the confirmation commits.
    Retire {
        plan: String,
        version: i64,
    },
    Remove {
        plan: String,
        version: i64,
        exception: Exception,
    },
}

pub(super) struct Record {
    pages: Pages,
    code: String,
    page: i64,
    calendar: Option<Calendar>,
    /// The tenant's holiday sets by code (the first 400), for names and the
    /// include choice; empty for a reader who may not list them.
    sets: Vec<(String, String)>,
    /// The sets are read again on a full load and after a set-related
    /// event, not on every click.
    sets_stale: bool,
    /// Whether the person may change calendars, as of the last load.
    manage: bool,
    /// Today where people work: the default for a new date.
    today: Option<Date>,
    /// The date "in force" is read on, as the calendars list reads it.
    rule_date: Option<Date>,
    statuses: CommitStates,
    notice: String,
    flash: Flash,
    dialog: Dialog,
    rules: ListColumns,
    included: ListColumns,
    exceptions: ListColumns,
    rows_changed: bool,
}
impl Record {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let mount = |id, columns| ListColumns::mount(id, columns).map_err(|_| Error::Internal);
        Ok(Self {
            pages,
            code: code(request.path)?,
            page: page(request.query),
            calendar: None,
            sets: Vec::new(),
            sets_stale: true,
            manage: false,
            today: None,
            rule_date: None,
            statuses: CommitStates::default(),
            notice: String::new(),
            flash: Flash::default(),
            dialog: Dialog::None,
            rules: mount("work-calendar-rules", rule_columns())?,
            included: mount("work-calendar-sets", set_columns())?,
            exceptions: mount("work-calendar-exceptions", exception_columns())?,
            rows_changed: true,
        })
    }
    fn calendar(&self) -> Result<&Calendar> {
        self.calendar.as_ref().ok_or(Error::Internal)
    }
    fn version(&self) -> Result<i64> {
        Ok(self.calendar()?.version)
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let mut calendar = self
            .pages
            .calendars
            .calendar(scope, &self.code, self.page)
            .await?;
        if calendar.exceptions.rows.is_empty() && self.page > 1 {
            self.page = 1;
            calendar = self.pages.calendars.calendar(scope, &self.code, 1).await?;
        }
        // The record needs only `.view`; the sets' names and the include
        // choice come from the listing, so only a reader who may list sets
        // reads it. Another reader sees each included set by its code.
        let capabilities = self.pages.capabilities(scope).await?;
        self.manage = capabilities.contains(MANAGE);
        if std::mem::take(&mut self.sets_stale) {
            self.sets.clear();
            // A page of the listing at a time, as far as the module's bound
            // for a nested collection.
            for page in 1..=MAX_ROWS / MAX_PAGE_SIZE {
                if !capabilities.contains(LIST) {
                    break;
                }
                let listing = Listing {
                    page,
                    ..Listing::default()
                };
                let rows = self.pages.calendars.holiday_sets(scope, &listing).await?;
                let done = rows.rows.len() < MAX_PAGE_SIZE as usize;
                self.sets
                    .extend(rows.rows.into_iter().map(|s| (s.code, s.name)));
                if done {
                    break;
                }
            }
        }
        self.today = Some(self.pages.today(scope).await?);
        self.rule_date = Some(self.pages.rule_date().await?);
        if self.rows_changed {
            self.rules
                .load(&calendar.rules.rows, |r| r.effective_from.to_string());
            self.included
                .load(&calendar.holiday_sets.rows, String::clone);
            self.exceptions
                .load(&calendar.exceptions.rows, |e| e.id.to_string());
        }
        self.calendar = Some(calendar);
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        self.notice.clear();
        let name = event(value)?;
        let grid = |columns: &mut ListColumns| -> Result<()> {
            match columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(_) => Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => Ok(()),
            }
        };
        match name {
            "rules_grid" => {
                self.rows_changed = false;
                return grid(&mut self.rules);
            }
            "sets_grid" => {
                self.rows_changed = false;
                return grid(&mut self.included);
            }
            "exceptions_grid" => {
                self.rows_changed = false;
                return grid(&mut self.exceptions);
            }
            "clear-flash" => {
                self.flash.dismiss(text(value, "key"));
                return Ok(());
            }
            "cancel_confirm" | "close_dialog" => {
                self.dialog = Dialog::None;
                return Ok(());
            }
            // The choice and the names follow sets kept elsewhere meanwhile.
            "new_include" | "include_set" | "leave_out" => self.sets_stale = true,
            _ => {}
        }
        // Every change asks Authz again: what the page drew is not a decision.
        if !self.pages.allowed(scope, MANAGE).await? {
            self.dialog = Dialog::None;
            self.statuses.clear_saved();
            self.notice = DENIED.into();
            self.flash.put(StatusKind::Error, DENIED);
            return Ok(());
        }
        let code = self.code.clone();
        match name {
            "save_calendar" | "save_substitute" => {
                let field = text(value, "field");
                let expected = if name == "save_calendar" {
                    "name"
                } else {
                    "substitute"
                };
                if field != expected || text(value, "id") != code {
                    return Err(Error::Invalid);
                }
                let submitted = text(value, "value");
                let mut input = json!({"code": code, "version": version(value)?});
                input[field] = json!(submitted.trim());
                let result = self
                    .pages
                    .submit(scope, credential, name, "core.work_calendar.update", input)
                    .await?;
                let label = if field == "name" {
                    "name"
                } else {
                    "holiday substitution"
                };
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
            "request_retire" => {
                let version = self.version()?;
                match self
                    .pages
                    .preview(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.update",
                        json!({"code": code, "version": version, "status": "retired"}),
                    )
                    .await?
                {
                    Ok(plan) => {
                        self.flash.clear();
                        self.dialog = Dialog::Retire { plan, version };
                    }
                    Err(refusal) => self.flash.put(StatusKind::Error, refusal.sentence(NOUN)),
                }
            }
            "retire" => {
                let Dialog::Retire { plan, version } =
                    std::mem::replace(&mut self.dialog, Dialog::None)
                else {
                    return Ok(());
                };
                let result = self
                    .pages
                    .commit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.update",
                        &plan,
                        json!({"code": code, "version": version, "status": "retired"}),
                    )
                    .await?;
                self.report(result, "Calendar retired. Records that use it keep it.");
            }
            "reactivate" => {
                let result = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.update",
                        json!({"code": code, "version": self.version()?, "status": "active"}),
                    )
                    .await?;
                self.report(result, "Calendar reactivated. It can be chosen again.");
            }
            "new_rule" => {
                let calendar = self.calendar()?;
                let today = self.today.ok_or(Error::Internal)?;
                let mut values = HashMap::new();
                // The week in force on the date the dialog starts from, so
                // saving it unchanged never puts back an earlier week.
                if let Some(rule) = in_force(&calendar.rules.rows, today) {
                    values = week::values(&rule.week);
                    values.insert("timezone".into(), rule.timezone.clone());
                }
                values.insert("effective_from".into(), today.to_string());
                self.flash.clear();
                self.dialog = Dialog::Rule(Draft {
                    values,
                    errors: BTreeMap::new(),
                });
            }
            "set_rule" => {
                let posted = form(value, "rule")?;
                let field = |key: &str| posted.get(key).map_or("", String::as_str).trim();
                let from = field("effective_from").to_owned();
                let input = json!({
                    "code": code, "version": self.version()?,
                    "effective_from": from, "timezone": field("timezone"),
                    "week": week::input(&posted),
                });
                let result = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.rule.set",
                        input,
                    )
                    .await?;
                let message = match from.parse::<Date>() {
                    Ok(date) => format!("The week from {} is set.", long_date(date)),
                    Err(_) => "The week is set.".into(),
                };
                self.dialog_outcome(result, posted, Dialog::Rule, &message);
            }
            "new_include" => {
                self.flash.clear();
                self.dialog = Dialog::Include(Draft::default());
            }
            "include_set" => {
                let posted = form(value, "include")?;
                let set = posted.get("holiday_set").cloned().unwrap_or_default();
                let result = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.include",
                        json!({"code": code, "version": self.version()?, "holiday_set": set, "included": true}),
                    )
                    .await?;
                let message = format!(
                    "{} is included. Its holidays close this calendar.",
                    self.set_name(&set)
                );
                self.dialog_outcome(result, posted, Dialog::Include, &message);
            }
            "leave_out" => {
                let set = text(value, "key").to_owned();
                let result = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.include",
                        json!({"code": code, "version": self.version()?, "holiday_set": set, "included": false}),
                    )
                    .await?;
                let message = format!(
                    "{} is left out. Its holidays no longer close this calendar.",
                    self.set_name(&set)
                );
                self.report(result, &message);
            }
            "new_exception" => {
                let today = self.today.ok_or(Error::Internal)?.to_string();
                self.flash.clear();
                self.dialog = Dialog::Exception(Draft {
                    values: HashMap::from([
                        ("starts_on".into(), today.clone()),
                        ("ends_on".into(), today),
                        ("type".into(), "off".into()),
                    ]),
                    errors: BTreeMap::new(),
                });
            }
            "add_exception" => {
                let posted = form(value, "exception")?;
                let field = |key: &str| posted.get(key).map_or("", String::as_str).trim();
                let day_type = field("type");
                let mut input = json!({
                    "code": code, "version": self.version()?,
                    "starts_on": field("starts_on"), "name": field("name"), "type": day_type,
                });
                if !field("ends_on").is_empty() {
                    input["ends_on"] = json!(field("ends_on"));
                }
                if day_type == "working" {
                    input["periods"] = json!(week::periods(field("periods")));
                }
                let result = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.exception.add",
                        input,
                    )
                    .await?;
                let message = format!("The exception “{}” is added.", field("name"));
                self.dialog_outcome(result, posted, Dialog::Exception, &message);
            }
            "request_remove_exception" => {
                let id: i64 = text(value, "key").parse().map_err(|_| Error::Invalid)?;
                let version = self.version()?;
                let Some(exception) = self
                    .calendar()?
                    .exceptions
                    .rows
                    .iter()
                    .find(|e| e.id == id)
                    .cloned()
                else {
                    self.flash
                        .put(StatusKind::Error, "That exception no longer exists.");
                    return Ok(());
                };
                match self
                    .pages
                    .preview(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.exception.remove",
                        json!({"code": code, "version": version, "exception_id": id}),
                    )
                    .await?
                {
                    Ok(plan) => {
                        self.flash.clear();
                        self.dialog = Dialog::Remove {
                            plan,
                            version,
                            exception,
                        };
                    }
                    Err(refusal) => self.flash.put(StatusKind::Error, refusal.sentence(NOUN)),
                }
            }
            "remove_exception" => {
                let Dialog::Remove {
                    plan,
                    version,
                    exception,
                } = std::mem::replace(&mut self.dialog, Dialog::None)
                else {
                    return Ok(());
                };
                let result = self
                    .pages
                    .commit(
                        scope,
                        credential,
                        name,
                        "core.work_calendar.exception.remove",
                        &plan,
                        json!({"code": code, "version": version, "exception_id": exception.id}),
                    )
                    .await?;
                self.report(
                    result,
                    &format!("The exception “{}” is removed.", exception.name),
                );
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    /// A finished write's message on the page.
    fn report(&mut self, result: Outcome, success: &str) {
        match result {
            Ok(_) => self.flash.put(StatusKind::Success, success),
            Err(refusal) => self.flash.put(StatusKind::Error, refusal.sentence(NOUN)),
        }
    }
    /// A dialog's write: closed on success, kept with the refusals otherwise.
    fn dialog_outcome(
        &mut self,
        result: Outcome,
        posted: HashMap<String, String>,
        dialog: fn(Draft) -> Dialog,
        success: &str,
    ) {
        match result {
            Ok(_) => {
                self.dialog = Dialog::None;
                self.flash.put(StatusKind::Success, success);
            }
            Err(refusal @ Refusal::Invalid(_)) => {
                // A field the dialog shows carries its refusal; the rest
                // (a calendar gone meanwhile) is said on the page.
                let (errors, rest) =
                    refusal.fields(|key| posted.contains_key(key) || week::is_field(key));
                if rest.is_empty() {
                    self.flash.clear();
                } else {
                    self.flash.put(StatusKind::Error, rest);
                }
                self.dialog = dialog(Draft {
                    values: posted,
                    errors,
                });
            }
            Err(refusal) => {
                self.flash.put(StatusKind::Error, refusal.sentence(NOUN));
                self.dialog = dialog(Draft {
                    values: posted,
                    errors: BTreeMap::new(),
                });
            }
        }
    }
    fn set_name(&self, code: &str) -> String {
        match self.sets.iter().find(|(c, _)| c == code) {
            Some((_, name)) => format!("{name} ({code})"),
            None => code.to_owned(),
        }
    }

    // ------------------------------------------------------------ the view

    fn facts(&self, calendar: &Calendar, manage: bool) -> Result<Html> {
        let mut name = InlineField::new("work-calendar-name", "name", "Edit name", &calendar.name)
            .version(calendar.version)
            .status(self.statuses.get("name"));
        name.id_value = calendar.code.clone();
        name.save_event = "save_calendar".into();
        let substitute = calendar.substitute.as_str();
        let mut choice = InlineField::new(
            "work-calendar-substitute",
            "substitute",
            "Edit holidays on a day off",
            substitute,
        )
        .version(calendar.version)
        .status(self.statuses.get("substitute"));
        choice.id_value = calendar.code.clone();
        choice.save_event = "save_substitute".into();
        if !manage {
            name = name.readonly();
            choice = choice.readonly();
        }
        let options = super::calendars::substitutes();
        let shown = options
            .iter()
            .find(|o| o.value == substitute)
            .map_or(substitute.to_owned(), |o| o.label.clone());
        let editor = render(
            Field::named("substitute")
                .id("work-calendar-substitute-select")
                .label_class("sr-only")
                .label("Holidays on a day off")
                .kind(InputKind::Select)
                .value(substitute)
                .options(options),
        )?;
        let zone = in_force(&calendar.rules.rows, self.rule_date.ok_or(Error::Internal)?)
            .map_or("—", |rule| rule.timezone.as_str());
        let facts = render(Facts {
            id: "work-calendar-facts",
            items: vec![
                Fact {
                    title: "Code",
                    id: "work-calendar-code",
                    value: Html::text(&calendar.code),
                },
                Fact {
                    title: "Name",
                    id: "work-calendar-name-fact",
                    value: render(InlineEdit { field: name })?,
                },
                Fact {
                    title: "Status",
                    id: "work-calendar-status",
                    value: super::calendars::status_badge(&calendar.status)?,
                },
                Fact {
                    title: "Holidays on a day off",
                    id: "work-calendar-substitute-fact",
                    value: render(InlineChoice {
                        field: choice,
                        display: Html::text(&shown),
                        editor,
                    })?,
                },
                Fact {
                    title: "Zone today",
                    id: "work-calendar-zone",
                    value: Html::text(zone),
                },
            ],
        })?;
        section(
            SectionHeading {
                id: "work-calendar-details-heading",
                title: "Details",
                ..SectionHeading::default()
            },
            vec![render(Card {
                id: "work-calendar-details",
                role: "region",
                labelledby: "work-calendar-details-heading",
                body: facts,
                ..Card::default()
            })?],
        )
    }

    /// A section: its heading, then the card framing its table, then what
    /// follows the table.
    fn table_section(
        id: &str,
        heading: SectionHeading,
        table: FlexTable,
        after: Vec<Html>,
    ) -> Result<Html> {
        let labelledby = heading.id;
        let mut cards = vec![render(Card {
            id,
            inner_class: "p-0",
            role: "region",
            labelledby,
            body: render(table)?,
            ..Card::default()
        })?];
        cards.extend(after);
        section(heading, cards)
    }
    fn rules_card(&self, calendar: &Calendar, manage: bool) -> Result<Html> {
        let date = self.rule_date.ok_or(Error::Internal)?;
        let current = in_force(&calendar.rules.rows, date).map(|rule| rule.effective_from);
        let mut rows = Vec::with_capacity(calendar.rules.rows.len());
        for rule in &calendar.rules.rows {
            let mut from = Html::text(&long_date(rule.effective_from));
            if current == Some(rule.effective_from) {
                from.push(&Html::text(" "));
                from.push(&render(bilibi_ui::Badge::status(
                    bilibi_ui::BadgeKind::Neutral,
                    "In force",
                ))?);
            }
            let mut row = FlexRow::new(rule.effective_from.to_string())
                .slot("from", from)
                .slot("timezone", Html::text(&rule.timezone));
            for ((day, _), weekday) in week::DAYS.iter().zip(&rule.week.0) {
                row = row.slot(day, Html::text(&week::summary(weekday)));
            }
            row.dom_id = Some(format!("work-calendar-rule-{}", rule.effective_from));
            rows.push(row);
        }
        let mut table = FlexTable::new("work-calendar-rules", self.rules.flex_columns(), rows);
        table.event = "rules_grid";
        table.zoom = self.rules.zoom;
        table.suggestions = self.rules.flex_suggestions();
        table.add_query = &self.rules.add_query;
        table.caption = "Weekly hours";
        table.framed = false;
        let actions = if manage {
            render(Button {
                id: "work-calendar-new-rule",
                event: "new_rule",
                button_type: "button",
                body: Html::text("Set week from a date"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        Self::table_section(
            "work-calendar-rules-card",
            SectionHeading {
                id: "work-calendar-rules-heading",
                title: "Weekly hours",
                count: Some(calendar.rules.total as usize),
                description: "Each week applies from its date until the next one.",
                actions,
                ..SectionHeading::default()
            },
            table,
            Vec::new(),
        )
    }
    fn sets_card(&self, calendar: &Calendar, manage: bool) -> Result<Html> {
        let mut rows = Vec::with_capacity(calendar.holiday_sets.rows.len());
        for set in &calendar.holiday_sets.rows {
            let name = self
                .sets
                .iter()
                .find(|(code, _)| code == set)
                .map_or("—", |(_, name)| name.as_str());
            let mut row = FlexRow::new(set.clone())
                .slot(
                    "code",
                    // A set is no workspace record kind: those are module
                    // ids, and `core/work_calendar` names the calendars.
                    render(bilibi_ui::RecordLink {
                        workspace: false,
                        kind: "",
                        record_id: set,
                        href: &format!("{SETS_PATH}/{set}"),
                        class: "whitespace-nowrap font-mono text-xs font-medium text-action hover:underline",
                        body: Html::text(set),
                    })?,
                )
                .slot("name", Html::text(name));
            if manage {
                row.actions = render(IconButton {
                    id: &format!("work-calendar-leave-out-{set}"),
                    kind: IconKind::Danger,
                    event: "leave_out",
                    name: "key",
                    value: set,
                    ..IconButton::new("x-mark", &format!("Leave out {set}"))
                })?;
            }
            row.dom_id = Some(format!("work-calendar-set-{set}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("work-calendar-sets", self.included.flex_columns(), rows);
        table.event = "sets_grid";
        table.zoom = self.included.zoom;
        table.suggestions = self.included.flex_suggestions();
        table.add_query = &self.included.add_query;
        table.caption = "Holiday sets";
        table.framed = false;
        table.actions = manage;
        if calendar.holiday_sets.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No holiday sets yet. Only the week and exceptions decide its days.",
            )));
        }
        let actions = if manage && !self.choices(calendar).is_empty() {
            render(Button {
                id: "work-calendar-new-include",
                event: "new_include",
                button_type: "button",
                body: Html::text("Include a set"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        Self::table_section(
            "work-calendar-sets-card",
            SectionHeading {
                id: "work-calendar-sets-heading",
                title: "Holiday sets",
                count: Some(calendar.holiday_sets.total as usize),
                description: "Holidays from these sets close the calendar.",
                actions,
                ..SectionHeading::default()
            },
            table,
            Vec::new(),
        )
    }
    /// The sets not yet included, name first and code after.
    fn choices(&self, calendar: &Calendar) -> Vec<Choice> {
        self.sets
            .iter()
            .filter(|(code, _)| !calendar.holiday_sets.rows.contains(code))
            .map(|(code, name)| Choice::new(name.clone(), code.clone()).detail(code))
            .collect()
    }
    fn exceptions_card(&self, calendar: &Calendar, manage: bool) -> Result<Html> {
        let mut rows = Vec::with_capacity(calendar.exceptions.rows.len());
        for exception in &calendar.exceptions.rows {
            let hours = exception
                .day
                .periods
                .iter()
                .map(week::period)
                .collect::<Vec<_>>()
                .join(", ");
            let mut row = FlexRow::new(exception.id.to_string())
                .slot(
                    "dates",
                    Html::text(&dates(exception.starts_on, exception.ends_on)),
                )
                .slot("name", Html::text(&exception.name))
                .slot(
                    "type",
                    Html::text(week::kind(exception.day.day_type).label()),
                )
                .slot(
                    "hours",
                    Html::text(if hours.is_empty() { "—" } else { &hours }),
                );
            if manage {
                let id = exception.id.to_string();
                row.actions = render(IconButton {
                    id: &format!("work-calendar-exception-remove-{id}"),
                    kind: IconKind::Danger,
                    event: "request_remove_exception",
                    name: "key",
                    value: &id,
                    ..IconButton::new("delete", &format!("Remove {}", exception.name))
                })?;
            }
            row.dom_id = Some(format!("work-calendar-exception-{}", exception.id));
            rows.push(row);
        }
        let mut table = FlexTable::new(
            "work-calendar-exceptions",
            self.exceptions.flex_columns(),
            rows,
        );
        table.event = "exceptions_grid";
        table.zoom = self.exceptions.zoom;
        table.suggestions = self.exceptions.flex_suggestions();
        table.add_query = &self.exceptions.add_query;
        table.caption = "Exceptions";
        table.framed = false;
        table.actions = manage;
        if calendar.exceptions.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text("No exceptions.")));
        }
        let actions = if manage {
            render(Button {
                id: "work-calendar-new-exception",
                event: "new_exception",
                button_type: "button",
                body: Html::text("Add exception"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        // Exceptions page 400 at a time: plain links to the neighbours.
        let shown = (self.page - 1) * MAX_ROWS + calendar.exceptions.rows.len() as i64;
        let mut links = Vec::new();
        if self.page > 1 {
            links.push(render(ActionLink {
                id: "work-calendar-exceptions-earlier",
                href: &format!("?page={}", self.page - 1),
                body: Html::text("Earlier exceptions"),
                ..ActionLink::default()
            })?);
        }
        if shown < calendar.exceptions.total {
            links.push(render(ActionLink {
                id: "work-calendar-exceptions-later",
                href: &format!("?page={}", self.page + 1),
                body: Html::text("Later exceptions"),
                ..ActionLink::default()
            })?);
        }
        let after = if links.is_empty() {
            Vec::new()
        } else {
            vec![render(Cluster { parts: links })?]
        };
        Self::table_section(
            "work-calendar-exceptions-card",
            SectionHeading {
                id: "work-calendar-exceptions-heading",
                title: "Exceptions",
                count: Some(calendar.exceptions.total as usize),
                description: "Dates that differ from the week: a shutdown, a special working day, a half day.",
                actions,
                ..SectionHeading::default()
            },
            table,
            after,
        )
    }

    fn rule_dialog(&self, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let rows = vec![vec![
            render(
                Field::in_form("rule", "effective_from")
                    .id("work-calendar-rule-from")
                    .label("Applies from")
                    .kind(InputKind::Date)
                    .value(value("effective_from"))
                    .hint("Until the next week's date. The same date replaces that week.")
                    .errors(errors("effective_from"))
                    .required(),
            )?,
            render(
                Field::in_form("rule", "timezone")
                    .id("work-calendar-rule-timezone")
                    .label("Zone")
                    .value(value("timezone"))
                    .hint("Opening hours are read in this time zone.")
                    .errors(errors("timezone"))
                    .required(),
            )?,
        ]];
        let body = render(DialogForm {
            id: "work-calendar-rule-form",
            event: "set_rule",
            rows,
            week_heading: "work-calendar-rule-week",
            week: week::editor("rule", &draft.values, &draft.errors)?,
            actions: dialog_actions("Set week", "Setting…", "close_dialog")?,
            ..DialogForm::default()
        })?;
        render(Modal {
            id: "work-calendar-rule-dialog",
            title: "Set the week from a date",
            description: "Earlier dates keep the week they had.",
            cancel_event: "close_dialog",
            width: ModalWidth::Wide,
            flash: Some(self.flash.map()),
            body,
            ..Modal::default()
        })
    }
    fn exception_dialog(&self, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let field = |key: &str, label: &str| {
            Field::in_form("exception", key)
                .id(format!("work-calendar-exception-{}", key.replace('_', "-")))
                .label(label)
                .value(value(key))
                .errors(errors(key))
        };
        let rows = vec![
            vec![render(
                field("name", "Name")
                    .hint("Why: Year-end shutdown, Special working Saturday, Half day.")
                    .attr("maxlength", "120")
                    .required(),
            )?],
            vec![
                render(field("starts_on", "From").kind(InputKind::Date).required())?,
                render(
                    field("ends_on", "To")
                        .kind(InputKind::Date)
                        .hint("The same date for one day; at most 366 days."),
                )?,
            ],
            vec![
                render(
                    field("type", "Type")
                        .kind(InputKind::Select)
                        .options(week::types()),
                )?,
                render(
                    field("periods", "Hours")
                        .placeholder("09:00-13:00")
                        .hint("A working day only: HH:MM-HH:MM, several separated by commas. Short hours make a half day."),
                )?,
            ],
        ];
        let body = render(DialogForm {
            id: "work-calendar-exception-form",
            event: "add_exception",
            rows,
            actions: dialog_actions("Add exception", "Adding…", "close_dialog")?,
            ..DialogForm::default()
        })?;
        render(Modal {
            id: "work-calendar-exception-dialog",
            title: "Add an exception",
            description: "These dates follow it instead of the week and holidays.",
            cancel_event: "close_dialog",
            flash: Some(self.flash.map()),
            body,
            ..Modal::default()
        })
    }
    fn include_dialog(&self, calendar: &Calendar, draft: &Draft) -> Result<Html> {
        let combo = Combobox::new("work-calendar-include-set", "include[holiday_set]")
            .label("Holiday set")
            .options(self.choices(calendar))
            .value(draft.values.get("holiday_set").cloned().unwrap_or_default())
            .placeholder("Choose a holiday set")
            .errors(draft.errors.get("holiday_set").cloned().unwrap_or_default());
        let body = render(DialogForm {
            id: "work-calendar-include-form",
            event: "include_set",
            rows: vec![vec![render(combo)?]],
            actions: dialog_actions("Include", "Including…", "close_dialog")?,
            ..DialogForm::default()
        })?;
        render(Modal {
            id: "work-calendar-include-dialog",
            title: "Include a holiday set",
            description: "Its holidays close this calendar, from now and in the past.",
            cancel_event: "close_dialog",
            flash: Some(self.flash.map()),
            body,
            ..Modal::default()
        })
    }
    fn dialog(&self, calendar: &Calendar) -> Result<Html> {
        match &self.dialog {
            Dialog::None => Ok(Html::EMPTY),
            Dialog::Rule(draft) => self.rule_dialog(draft),
            Dialog::Exception(draft) => self.exception_dialog(draft),
            Dialog::Include(draft) => self.include_dialog(calendar, draft),
            Dialog::Retire { .. } => render(bilibi_ui::ConfirmDialog {
                id: "work-calendar-retire-confirm",
                consequence: &format!("Retire {}?", calendar.name),
                detail: "It can no longer be chosen for new records. Records that use it keep computing with it, and you can reactivate it later.",
                confirm: "Retire",
                working: "Retiring…",
                confirm_event: "retire",
                cancel_event: "cancel_confirm",
            }),
            Dialog::Remove { exception, .. } => render(bilibi_ui::ConfirmDialog {
                id: "work-calendar-exception-remove-confirm",
                consequence: &format!(
                    "Remove the exception “{}” ({})?",
                    exception.name,
                    dates(exception.starts_on, exception.ends_on)
                ),
                detail: "Those dates follow the week and holidays again. This cannot be undone.",
                confirm: "Remove",
                working: "Removing…",
                confirm_event: "remove_exception",
                cancel_event: "cancel_confirm",
            }),
        }
    }

    fn view(&self) -> Result<String> {
        let calendar = self.calendar()?;
        let manage = self.manage;
        let mut actions = render(BackLink {
            href: CALENDARS_PATH,
            id: "work-calendar-back",
            title: "Back to work calendars",
            ..BackLink::default()
        })?;
        actions.push(&render(ActionLink {
            id: "work-calendar-month",
            icon: "calendar-days",
            href: &format!("{CALENDARS_PATH}/{}/month", calendar.code),
            body: Html::text("Month view"),
            ..ActionLink::default()
        })?);
        if manage {
            actions.push(&render(if calendar.status == "active" {
                Button {
                    id: "work-calendar-retire",
                    variant: ButtonVariant::Danger,
                    event: "request_retire",
                    button_type: "button",
                    working: "Checking…",
                    body: Html::text("Retire"),
                    ..Button::default()
                }
            } else {
                Button {
                    id: "work-calendar-reactivate",
                    event: "reactivate",
                    button_type: "button",
                    working: "Reactivating…",
                    body: Html::text("Reactivate"),
                    ..Button::default()
                }
            })?);
        }
        let header = render(Header {
            title: Html::text(&calendar.name),
            subtitle: "Work calendar",
            actions,
            ..Header::default()
        })?;
        View {
            id: "work-calendar-page",
            variant: PageVariant::Detail,
            flash: self.flash.map(),
            record_kind: "core/work_calendar",
            record_id: &calendar.code,
            header,
            sections: vec![
                self.facts(calendar, manage)?,
                self.rules_card(calendar, manage)?,
                self.sets_card(calendar, manage)?,
                self.exceptions_card(calendar, manage)?,
            ],
            dialog: self.dialog(calendar)?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

/// `?page=` of the exceptions, from 1 (the contract's bound).
fn page(query: &HashMap<String, String>) -> i64 {
    params::positive_id(query.get("page").map(String::as_str))
        .filter(|page| *page <= 100_000)
        .unwrap_or(1)
}

impl Titled for Record {
    fn title(&self) -> Option<String> {
        self.calendar.as_ref().map(|calendar| calendar.name.clone())
    }
}
impl LiveState<Scope, Error> for Record {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => self.sets_stale = true,
                LiveInput::Params(values) => self.page = page(values),
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            self.load(request.scope).await?;
            let html = self.view()?;
            let mut query = HashMap::new();
            if self.page > 1 {
                query.insert("page".into(), self.page.to_string());
            }
            let rows = self.calendar()?.exceptions.rows.len();
            Ok(body(html, query, rows, &self.notice))
        })
    }
}
