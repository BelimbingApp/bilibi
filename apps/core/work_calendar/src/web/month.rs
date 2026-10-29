//! `/work-calendars/{code}/month?month=YYYY-MM`: one month of a calendar's
//! resolved days (what `core.work_calendar.days` answers), each a shared day
//! tile with its type, opening periods and reason. The previous and next
//! months are plain links, so each month has its own address; the default is
//! today's month in the company's zone.
use super::*;
use crate::{contributions::CALENDARS_PATH, Day, Reason};
use bilibi_ui::{ActionLink, BackLink, Card, DayMonth, DayTile, Header};
use jiff::ToSpan;

pub(super) struct Month {
    pages: Pages,
    code: String,
    /// The first day of the month shown; `None` until today is known.
    month: Option<Date>,
    name: Option<String>,
}
/// `YYYY-MM` as the first day of that month, in the years a holiday set's
/// page offers (1900 to 2999), so a month's whole weeks and the days a
/// holiday's substitute reaches stay well inside the date range. Any other
/// is no month, and the page shows today's.
fn parse(query: &HashMap<String, String>) -> Option<Date> {
    let text = query.get("month")?;
    let (year, month) = text.split_once('-')?;
    if year.len() != 4 || month.len() != 2 {
        return None;
    }
    let year = year
        .parse()
        .ok()
        .filter(|year| (1900..=2999).contains(year))?;
    Date::new(year, month.parse().ok()?, 1).ok()
}
fn address(month: Date) -> String {
    month.strftime("%Y-%m").to_string()
}
/// Why the date is what it is, in words; empty when the week alone decides.
fn reason(day: &Day) -> String {
    match &day.reason {
        Reason::Week => String::new(),
        Reason::Holiday { name, .. } | Reason::Exception { name } => name.clone(),
        Reason::Substitute { name, holiday, .. } => {
            format!("In lieu of {name} ({})", holiday.strftime("%-d %B"))
        }
    }
}
/// One resolved day as a shared tile: its opening periods, then why it is
/// what it is. `month` is the first day of the month shown; a date outside
/// it fills the first or last week. The description repeats the lines, so
/// the tint is never the only signal.
fn tile(day: &Day, month: Date, today: Date) -> Result<Html> {
    let kind = week::kind(day.day_type);
    let periods: Vec<String> = day.periods.iter().map(week::period).collect();
    let why = reason(day);
    let mut description = day.date.strftime("%A %-d %B %Y").to_string();
    if day.date == today {
        description.push_str(", today");
    }
    description.push_str(&format!(": {}", kind.name()));
    if !periods.is_empty() {
        description.push_str(&format!(", {}", periods.join(" and ")));
    }
    if !why.is_empty() {
        description.push_str(&format!(" ({why})"));
    }
    let mut lines = periods;
    if !why.is_empty() {
        lines.push(why);
    }
    render(DayTile {
        id: &format!("work-calendar-day-{}", day.date),
        kind,
        date: &day.date.day().to_string(),
        description: &description,
        outside: day.date.first_of_month() != month,
        lines,
    })
}

impl Month {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            pages,
            code: code(request.path)?,
            month: parse(request.query),
            name: None,
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<String> {
        let today = self.pages.today(scope).await?;
        let first = match self.month {
            Some(month) => month,
            None => today.first_of_month(),
        };
        self.month = Some(first);
        // The calendar read is the view check; the days follow from it.
        let calendar = self.pages.calendars.calendar(scope, &self.code, 1).await?;
        self.name = Some(calendar.name.clone());
        // Whole weeks, Monday to Sunday, as the grid lays them: the
        // neighbouring months' dates are resolved too and shown dimmed.
        let last = first.last_of_month();
        let start = first
            .checked_sub(i64::from(first.weekday().to_monday_zero_offset()).days())
            .map_err(|_| Error::Invalid)?;
        let end = last
            .checked_add(i64::from(6 - last.weekday().to_monday_zero_offset()).days())
            .map_err(|_| Error::Invalid)?;
        let days = self
            .pages
            .calendars
            .days(scope, &self.code, start, end)
            .await?;
        let zone = days
            .iter()
            .find(|day| day.date == first)
            .map_or("", |day| day.timezone.as_str());
        let tiles = days
            .iter()
            .map(|day| tile(day, first, today))
            .collect::<Result<_>>()?;
        let caption = first.strftime("%B %Y").to_string();
        let grid = render(DayMonth {
            id: "work-calendar-month",
            caption: &caption,
            tiles,
        })?;
        let base = format!("{CALENDARS_PATH}/{}", self.code);
        let link = |id: &'static str, month: Date, text: &str| {
            render(ActionLink {
                id,
                href: &format!("{base}/month?month={}", address(month)),
                body: Html::text(text),
                ..ActionLink::default()
            })
        };
        let previous = first.checked_sub(1.month()).map_err(|_| Error::Invalid)?;
        let next = first.checked_add(1.month()).map_err(|_| Error::Invalid)?;
        let mut actions = render(BackLink {
            href: &base,
            id: "work-calendar-month-back",
            title: "Back to calendar",
            ..BackLink::default()
        })?;
        actions.push(&link(
            "work-calendar-month-previous",
            previous,
            "Previous month",
        )?);
        if first != today.first_of_month() {
            actions.push(&link(
                "work-calendar-month-today",
                today.first_of_month(),
                "This month",
            )?);
        }
        actions.push(&link("work-calendar-month-next", next, "Next month")?);
        let subtitle = if zone.is_empty() {
            caption.clone()
        } else {
            format!("{caption} · hours in {zone}")
        };
        let header = render(Header {
            title: Html::text(&calendar.name),
            subtitle: &subtitle,
            actions,
            ..Header::default()
        })?;
        let card = render(Card {
            id: "work-calendar-month-card",
            body: grid,
            ..Card::default()
        })?;
        View {
            id: "work-calendar-month-page",
            variant: PageVariant::Detail,
            flash: NO_FLASH,
            record_kind: "core/work_calendar",
            record_id: &calendar.code,
            header,
            sections: vec![card],
            dialog: Html::EMPTY,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl Titled for Month {
    fn title(&self) -> Option<String> {
        self.name.clone()
    }
}
impl LiveState<Scope, Error> for Month {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.month = parse(values),
                // Nothing on the page writes; a flash dismiss is all there is.
                LiveInput::Event(value) if event(value)? == "clear-flash" => {}
                LiveInput::Event(_) => return Err(Error::Invalid),
            }
            let html = self.view(request.scope).await?;
            let query = self
                .month
                .map(|month| HashMap::from([("month".to_owned(), address(month))]))
                .unwrap_or_default();
            Ok(body(html, query, 1, ""))
        })
    }
}
