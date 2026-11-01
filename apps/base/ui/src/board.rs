//! The shared status board (BelimbingProject/bilibi#142): columns by a status,
//! cards in them, a card dragged to another column. A Domain passes its
//! columns and the cards it read and gets the page fragment back; it owns no
//! board markup, CSS or JavaScript.
//!
//! - **The component never writes.** A drop and the Move dialog end in one
//!   [`BoardMove`], decoded by [`BoardMove::from_event`]; the owner passes it,
//!   with the record's `version`, to its own operation (for a workflow subject,
//!   the one that follows the statuses' edges), which rechecks the capability,
//!   the version and the edge inside its mutation boundary. A refused or stale
//!   move changes nothing on the page: the browser never moves a card itself.
//! - **Hints, not rules.** A card's `targets` are the columns this viewer may
//!   move it to; a card with none is not movable and has no drag handle and no
//!   Move action. Other columns are not drop targets. The operation still
//!   decides.
//! - **Bounded.** A column shows [`COLUMN_CARDS`] cards at a time and "Show
//!   more"; its heading always carries the true total.
//! - **Phones.** The board shows one column at a time with a column switcher
//!   kept in the address; drag is off on coarse pointers and "Move…" is the
//!   path.
use crate::{
    Alert, Button, ButtonVariant, Choice, Field, Form, Html, InputKind, Modal, ModalWidth,
    StatusKind, Tab, Tabs,
};
use askama::Template;
use std::collections::HashMap;

/// Cards a column draws before "Show more"; each press shows this many more.
pub const COLUMN_CARDS: usize = 50;
/// Facts a card shows below its title.
pub const MAX_FACTS: usize = 3;
/// Columns a board draws at most.
pub const MAX_COLUMNS: usize = 12;
/// The page event a drop raises: `id`, `version`, `from`, `to`.
pub const MOVE: &str = "board_move";
/// The Move dialog's form event.
pub const MOVE_SUBMIT: &str = "board_move_submit";
/// A "Move…" action: `id` names the card whose dialog the owner opens.
pub const MOVE_OPEN: &str = "board_move_open";
/// The dialog's Cancel and Escape.
pub const MOVE_CANCEL: &str = "board_move_cancel";
/// "Show more" of one column: `column` names it.
pub const MORE: &str = "board_more";
/// The names of the events above, for an owner's `match`.
pub const EVENTS: [&str; 5] = [MOVE, MOVE_SUBMIT, MOVE_OPEN, MOVE_CANCEL, MORE];

const MOVE_FORM: &str = "move";
const KEY_LIMIT: usize = 128;

/// One column: a status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardColumn {
    pub key: String,
    pub label: String,
    /// How many cards the column holds in all, not how many were read.
    pub total: u64,
    /// The column can receive a card at all (a closed status cannot).
    pub accepts: bool,
}

impl BoardColumn {
    pub fn new(key: impl Into<String>, label: impl Into<String>, total: u64) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            total,
            accepts: true,
        }
    }

    /// The column cannot receive a card (drops and the Move dialog skip it).
    pub fn closed(mut self) -> Self {
        self.accepts = false;
        self
    }
}

/// One card. `version` is the owner's own version token for the record; it
/// comes back with every move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardCard {
    pub id: String,
    /// The key of the column it is in.
    pub column: String,
    pub title: String,
    /// Up to [`MAX_FACTS`] short lines below the title; more are dropped.
    pub facts: Vec<String>,
    pub href: String,
    pub version: String,
    /// The columns this viewer may move it to (for a workflow subject, the
    /// statuses its edges allow for this person). Empty: not movable.
    pub targets: Vec<String>,
}

impl BoardCard {
    pub fn new(id: impl Into<String>, column: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            column: column.into(),
            title: title.into(),
            facts: Vec::new(),
            href: String::new(),
            version: String::new(),
            targets: Vec::new(),
        }
    }

    pub fn fact(mut self, fact: impl Into<String>) -> Self {
        if self.facts.len() < MAX_FACTS {
            self.facts.push(fact.into());
        }
        self
    }

    pub fn href(mut self, href: impl Into<String>) -> Self {
        self.href = href.into();
        self
    }

    /// The viewer may move it, to these columns. Set it only when they hold
    /// the update capability.
    pub fn movable(
        mut self,
        version: impl Into<String>,
        targets: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.version = version.into();
        self.targets = targets.into_iter().map(Into::into).collect();
        self
    }
}

/// The part of the board that lives in the address (`column`, the phone's
/// current column) and in the mounted page (how far each column is shown).
/// The owner's filters live in its own `ListState`.
#[derive(Clone, Debug, Default)]
pub struct BoardState {
    /// The column a phone shows; `None` is the first.
    pub column: Option<String>,
    shown: HashMap<String, usize>,
}

impl BoardState {
    /// Reads `column`; a key the board does not have is the first column.
    pub fn parse(query: &HashMap<String, String>, columns: &[BoardColumn]) -> Self {
        let column = query
            .get("column")
            .filter(|key| columns.iter().any(|column| &column.key == *key))
            .cloned();
        Self {
            column,
            shown: HashMap::new(),
        }
    }

    /// Re-reads `column` from the address, keeping how far each column is
    /// shown (a back or a filter change does not collapse "Show more").
    pub fn read_column(&mut self, query: &HashMap<String, String>, columns: &[BoardColumn]) {
        self.column = Self::parse(query, columns).column;
    }

    pub fn to_params(&self) -> HashMap<String, String> {
        self.column
            .iter()
            .map(|column| ("column".to_owned(), column.clone()))
            .collect()
    }

    /// The most cards the owner reads for `column`: fetch exactly this many.
    pub fn limit(&self, column: &str) -> usize {
        self.shown.get(column).copied().unwrap_or(COLUMN_CARDS)
    }

    /// The viewer pressed "Show more" on `column`.
    pub fn show_more(&mut self, column: &str) {
        let shown = self.limit(column);
        self.shown
            .insert(column.to_owned(), shown.saturating_add(COLUMN_CARDS));
    }
}

/// The board to draw.
#[derive(Clone, Debug)]
pub struct Board {
    pub id: String,
    /// The page's path; the phone's column switcher patches it.
    pub path: String,
    /// Other query values to keep in those links (the owner's filters).
    pub query: HashMap<String, String>,
    pub state: BoardState,
    pub columns: Vec<BoardColumn>,
    pub cards: Vec<BoardCard>,
    /// The result of the last move, announced politely; a refusal is an error.
    pub notice: Option<(StatusKind, String)>,
    /// The card whose Move dialog is open.
    pub moving: Option<String>,
    /// Errors of the open Move dialog.
    pub move_errors: Vec<String>,
}

impl Board {
    pub fn new(
        id: impl Into<String>,
        path: impl Into<String>,
        state: BoardState,
        columns: Vec<BoardColumn>,
    ) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            query: HashMap::new(),
            state,
            columns,
            cards: Vec::new(),
            notice: None,
            moving: None,
            move_errors: Vec::new(),
        }
    }

    pub fn render(&self) -> askama::Result<Html> {
        Html::render(&View::build(self)?)
    }
}

/// What a drop or the Move dialog asks the owner to do: change the record's
/// status from `from` to `to`. The owner passes `version` to its operation,
/// which refuses a stale one and checks that `from` is still the status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardMove {
    pub id: String,
    pub version: String,
    pub from: String,
    pub to: String,
}

impl BoardMove {
    /// Decodes [`MOVE`] (`id`, `version`, `from`, `to`) or [`MOVE_SUBMIT`]
    /// (the dialog's `move[...]` fields). The `Err` is a sentence for the
    /// person. Whether `to` is a column, and whether the move is allowed, is
    /// the owner's operation to decide.
    pub fn from_event(payload: &serde_json::Value) -> Result<Self, String> {
        let invalid = || "Choose a column to move the card to.".to_owned();
        let submitted = payload.get("type").and_then(|value| value.as_str()) == Some(MOVE_SUBMIT);
        let values: HashMap<String, String> = if submitted {
            crate::params::form_values_checked(payload, MOVE_FORM, KEY_LIMIT)
                .map_err(|_| invalid())?
        } else {
            ["id", "version", "from", "to"]
                .into_iter()
                .filter_map(|key| {
                    let value = payload.get(key)?.as_str()?;
                    Some((key.to_owned(), value.to_owned()))
                })
                .collect()
        };
        let read = |key: &str| values.get(key).cloned().unwrap_or_default();
        let change = Self {
            id: read("id"),
            version: read("version"),
            from: read("from"),
            to: read("to"),
        };
        let bounded = |text: &str| text.len() <= KEY_LIMIT;
        if change.id.is_empty()
            || change.to.is_empty()
            || ![&change.id, &change.version, &change.from, &change.to]
                .into_iter()
                .all(|text| bounded(text))
        {
            return Err(invalid());
        }
        Ok(change)
    }
}

/// The id of the card a [`MOVE_OPEN`] event names.
pub fn card_id(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty() && id.len() <= KEY_LIMIT)
}

/// The column a [`MORE`] event names.
pub fn column_key(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("column")
        .and_then(|key| key.as_str())
        .filter(|key| !key.is_empty() && key.len() <= KEY_LIMIT)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn sanitized(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

struct CardView {
    dom_id: String,
    id: String,
    title: String,
    facts: Vec<String>,
    href: String,
    version: String,
    column: String,
    /// The columns it may move to, comma-separated.
    targets: String,
    movable: bool,
    name: String,
}

struct ColumnView {
    dom_id: String,
    key: String,
    label: String,
    total: u64,
    accepts: bool,
    /// The heading in words: "Planned, 12 cards".
    name: String,
    /// The phone shows this column.
    current: bool,
    cards: Vec<CardView>,
    more: u64,
}

#[derive(Template)]
#[template(path = "board.html")]
struct View {
    id: String,
    notice: Option<(StatusKind, String)>,
    switcher: Html,
    columns: Vec<ColumnView>,
    dialog: Option<Html>,
}

fn cards_word(total: u64) -> String {
    if total == 1 {
        "1 card".to_owned()
    } else {
        format!("{total} cards")
    }
}

impl View {
    fn build(board: &Board) -> askama::Result<Self> {
        let columns: Vec<&BoardColumn> = board.columns.iter().take(MAX_COLUMNS).collect();
        let current = board
            .state
            .column
            .as_deref()
            .filter(|key| columns.iter().any(|column| column.key == *key))
            .or_else(|| columns.first().map(|column| column.key.as_str()))
            .unwrap_or_default()
            .to_owned();
        let address = |key: &str| {
            let mut query = board.query.clone();
            query.insert("column".to_owned(), key.to_owned());
            crate::params::address(&board.path, &query)
        };
        let hrefs: Vec<String> = columns.iter().map(|column| address(&column.key)).collect();
        let labels: Vec<String> = columns
            .iter()
            .map(|column| format!("{} {}", column.label, column.total))
            .collect();
        let tabs = columns
            .iter()
            .zip(hrefs.iter().zip(&labels))
            .map(|(column, (href, label))| Tab::patch(href, label, column.key == current))
            .collect();
        let switcher = Html::render(&Tabs {
            id: &format!("{}-columns", board.id),
            label: "Board column",
            class: "md:hidden",
            tabs,
        })?;
        let views = columns
            .iter()
            .map(|column| {
                let cards: Vec<CardView> = board
                    .cards
                    .iter()
                    .filter(|card| card.column == column.key)
                    .take(board.state.limit(&column.key))
                    .map(|card| Self::card(board, card, &columns))
                    .collect();
                let shown = cards.len() as u64;
                ColumnView {
                    dom_id: format!("{}-col-{}", board.id, sanitized(&column.key)),
                    key: column.key.clone(),
                    label: column.label.clone(),
                    total: column.total,
                    accepts: column.accepts,
                    name: format!("{}, {}", column.label, cards_word(column.total)),
                    current: column.key == current,
                    more: column.total.saturating_sub(shown),
                    cards,
                }
            })
            .collect();
        let dialog = match &board.moving {
            Some(id) => board
                .cards
                .iter()
                .find(|card| &card.id == id && !card.targets.is_empty())
                .map(|card| Self::dialog(board, card, &columns))
                .transpose()?,
            None => None,
        };
        Ok(Self {
            id: board.id.clone(),
            notice: board.notice.clone(),
            switcher,
            columns: views,
            dialog,
        })
    }

    fn card(board: &Board, card: &BoardCard, columns: &[&BoardColumn]) -> CardView {
        let targets: Vec<&str> = card
            .targets
            .iter()
            .filter(|target| {
                **target != card.column
                    && columns
                        .iter()
                        .any(|column| column.key == **target && column.accepts)
            })
            .map(String::as_str)
            .collect();
        let status = columns
            .iter()
            .find(|column| column.key == card.column)
            .map(|column| column.label.as_str())
            .unwrap_or_default();
        let mut name = format!("{}, {status}", card.title);
        for fact in card.facts.iter().take(MAX_FACTS) {
            name.push_str(&format!(", {fact}"));
        }
        CardView {
            dom_id: format!("{}-card-{}", board.id, sanitized(&card.id)),
            id: card.id.clone(),
            title: card.title.clone(),
            facts: card.facts.iter().take(MAX_FACTS).cloned().collect(),
            href: card.href.clone(),
            version: card.version.clone(),
            column: card.column.clone(),
            movable: !targets.is_empty(),
            targets: targets.join(","),
            name,
        }
    }

    /// The Move dialog: the columns the card may move to.
    fn dialog(board: &Board, card: &BoardCard, columns: &[&BoardColumn]) -> askama::Result<Html> {
        let hidden = |key: &str, value: &str| {
            Field::in_form(MOVE_FORM, key)
                .id(format!("{}-move-{key}", board.id))
                .kind(InputKind::Hidden)
                .value(value)
        };
        let options: Vec<Choice> = columns
            .iter()
            .filter(|column| {
                column.accepts && column.key != card.column && card.targets.contains(&column.key)
            })
            .map(|column| Choice::new(column.label.clone(), column.key.clone()))
            .collect();
        let select = Field::in_form(MOVE_FORM, "to")
            .id(format!("{}-move-to", board.id))
            .label("Move to")
            .kind(InputKind::Select)
            .options(options)
            .required();
        let mut body = Html::EMPTY;
        if !board.move_errors.is_empty() {
            let mut sentences = Html::EMPTY;
            for error in &board.move_errors {
                sentences.push(&Html::text(error));
            }
            body.push(&Html::render(&Alert {
                kind: StatusKind::Error,
                id: &format!("{}-move-error", board.id),
                class: "mb-3",
                body: sentences,
            })?);
        }
        for field in [
            hidden("id", &card.id),
            hidden("version", &card.version),
            hidden("from", &card.column),
        ] {
            body.push(&Html::render(&field)?);
        }
        body.push(&Html::render(&select)?);
        let mut actions = Html::render(&Button {
            variant: ButtonVariant::Primary,
            id: &format!("{}-move-save", board.id),
            button_type: "submit",
            working: "Moving…",
            body: Html::text("Move"),
            ..Button::default()
        })?;
        actions.push(&Html::render(&Button {
            id: &format!("{}-move-cancel", board.id),
            button_type: "button",
            event: MOVE_CANCEL,
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        let form = Html::render(&Form {
            id: &format!("{}-move-form", board.id),
            event: MOVE_SUBMIT,
            change: false,
            class: "",
            body,
            actions,
        })?;
        Html::render(&Modal {
            id: &format!("{}-move-dialog", board.id),
            title: &format!("Move {}", card.title),
            cancel_event: MOVE_CANCEL,
            width: ModalWidth::Compact,
            description: "",
            role: "",
            flash: None,
            body: form,
        })
    }
}
