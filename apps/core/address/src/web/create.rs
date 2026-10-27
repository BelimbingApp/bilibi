//! `/addresses/create` (Bilimbi `CreateLive`): label, phone and street
//! lines, the shared location fields with their cascade, and provenance. It
//! validates as values change and creates through the `core.address.create` contract,
//! which is the authority on every refusal.
//!
//! Beyond Bilimbi, the page keeps rarely used parts out of the way: street
//! lines 2 and 3 appear on "+ Add address line", provenance is folded under a
//! one-line summary that opens itself on an error, and once Create is pressed
//! the problems are listed above the form, each naming its field.
use super::{
    location::{Ids, LocationDraft, LABELS},
    *,
};
use crate::{contributions::CREATE, model, AddressInput, FieldError, Location};
use bilibi_ui::{
    error_sentence, AddMore, BackLink, Button, ButtonVariant, Choice, ErrorSummary, Field, Header,
    InputKind, RestrictedField,
};

const IDS: Ids = Ids {
    prefix: "address",
    country: "address-country",
    admin1: "address-admin1",
};

/// Every field in form order: its error key, its DOM id and its label, so an
/// error names its field and the summary links to it in reading order.
const FIELDS: &[(&str, &str, &str)] = &[
    ("label", "address-label", "Label"),
    ("phone", "address-phone", "Phone"),
    ("line1", "address-line1", "Address line 1"),
    ("line2", "address-line2", "Address line 2"),
    ("line3", "address-line3", "Address line 3"),
    ("country_iso", "address-country", "Country"),
    ("postcode", "address-postcode", "Postcode"),
    ("admin1_code", "address-admin1", "State or province"),
    ("locality", "address-locality", "Locality"),
    ("source", "address-source", "Source"),
    ("source_ref", "address-source-ref", "Source reference"),
    ("parser_version", "address-parser-version", "Parser version"),
    (
        "parse_confidence",
        "address-parse-confidence",
        "Parse confidence",
    ),
    (
        "verification_status",
        "address-verification-status",
        "Verification status",
    ),
    ("raw_input", "address-raw-input", "Raw input"),
];
const PROVENANCE: [&str; 6] = [
    "source",
    "source_ref",
    "parser_version",
    "parse_confidence",
    "verification_status",
    "raw_input",
];
const VERIFICATION: [(&str, &str); 3] = [
    ("Unverified", "unverified"),
    ("Suggested", "suggested"),
    ("Verified", "verified"),
];

#[derive(Template)]
#[template(path = "address_create.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    summary: ErrorSummary,
    contact: Vec<Field>,
    restricted: Vec<RestrictedField>,
    lines: Vec<Field>,
    add_line: Option<AddMore>,
    location: Html,
    provenance: Vec<Field>,
    provenance_summary: String,
    provenance_open: bool,
    raw_input: Field,
    submit: Html,
}

pub(super) struct Create {
    pages: Pages,
    countries: Vec<Choice>,
    input: AddressInput,
    location: LocationDraft,
    errors: Vec<FieldError>,
    flash: Flash,
    created: bool,
    /// Street lines the person asked to see; a line holding a value always shows.
    lines: usize,
    /// Create was pressed, so the problems are listed above the form.
    attempted: bool,
}
impl Create {
    pub async fn new(pages: Pages) -> Result<Self> {
        let location = LocationDraft::new(&pages.addresses, Location::default()).await?;
        Ok(Self {
            countries: pages.countries().await?,
            pages,
            input: AddressInput {
                source: Some("manual".into()),
                verification_status: Some("unverified".into()),
                ..AddressInput::default()
            },
            location,
            errors: Vec::new(),
            flash: Flash::default(),
            created: false,
            lines: 1,
            attempted: false,
        })
    }
    async fn take(&mut self, value: &Value) -> Result<()> {
        let fields = form(value, "address");
        let get = |key: &str| fields.get(key).cloned();
        self.location
            .change(&self.pages.addresses, &LocationDraft::posted(&fields))
            .await?;
        self.input = AddressInput {
            label: get("label"),
            phone: get("phone"),
            line1: get("line1"),
            line2: get("line2"),
            line3: get("line3"),
            raw_input: get("raw_input"),
            source: get("source"),
            source_ref: get("source_ref"),
            parser_version: get("parser_version"),
            parse_confidence: get("parse_confidence"),
            verification_status: get("verification_status"),
            ..AddressInput::default()
        };
        self.location.location.apply_to(&mut self.input);
        // The pure shape checks; Geonames membership is checked on save.
        self.errors = model::normalize(&self.input).err().unwrap_or_default();
        Ok(())
    }
    /// The street lines on screen: the ones asked for, and any holding a value.
    fn shown_lines(&self) -> usize {
        let filled = |v: &Option<String>| v.as_deref().is_some_and(|v| !v.trim().is_empty());
        let held = if filled(&self.input.line3) {
            3
        } else if filled(&self.input.line2) {
            2
        } else {
            1
        };
        self.lines.max(held)
    }
    async fn save(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        if !self.errors.is_empty() {
            return Ok(());
        }
        match self
            .pages
            .submit(
                scope,
                credential,
                "create",
                "core.address.create",
                serde_json::json!({"address":self.input}),
            )
            .await?
        {
            Ok(_) => self.created = true,
            Err(error) if fatal(&error) => return Err(Error::Unauthorized),
            Err(WriteError::Invalid(errors)) => self.errors = errors,
            Err(WriteError::Access(Error::Forbidden)) => self.flash.put(
                StatusKind::Error,
                "You do not have permission to create addresses or write these fields.",
            ),
            Err(_) => self
                .flash
                .put(StatusKind::Error, "The address was not created. Try again."),
        }
        Ok(())
    }
    async fn view(&self, scope: &Scope) -> Result<LiveBody> {
        if self.created {
            return Ok(LiveBody {
                redirect: Some("/addresses".into()),
                flash: Some((StatusKind::Success, "Address created successfully.".into())),
                ..LiveBody::default()
            });
        }
        let client = self.pages.db.get().await?;
        let masked = bilibi_authz::restrictions_for(&**client, scope, "addresses").await?;
        drop(client);
        let can_create = self.pages.allowed(scope, CREATE).await?;
        let label_of = |key: &str| {
            FIELDS
                .iter()
                .find(|(field, _, _)| *field == key)
                .map_or("", |(_, _, label)| *label)
        };
        let errors = |key: &str| -> Vec<String> {
            self.errors
                .iter()
                .filter(|e| e.field == key)
                .map(|e| error_sentence(label_of(key), &e.message))
                .collect()
        };
        let mut summary = ErrorSummary::new("address-problems");
        if self.attempted {
            for (key, id, label) in FIELDS {
                for error in self.errors.iter().filter(|e| e.field == *key) {
                    summary.push(*id, error_sentence(label, &error.message));
                }
            }
        }
        let value = |v: &Option<String>| v.clone().unwrap_or_default();
        let field = |key: &str, label: &str, v: &Option<String>| {
            Field::in_form("address", key)
                .id(format!("address-{}", key.replace('_', "-")))
                .label(label)
                .value(value(v))
                .attr("data-debounce", "300")
                .errors(errors(key))
        };
        let bounded = |mut f: Field| {
            f.attrs.maxlength = Some("255".into());
            f
        };
        let i = &self.input;
        let mut contact = vec![bounded(field("label", "Label", &i.label))];
        let mut restricted = Vec::new();
        // A restricted phone renders no input and is never written.
        if masked.contains("phone") {
            restricted.push(RestrictedField::new("address-phone", "Phone"));
        } else {
            contact.push(bounded(
                field("phone", "Phone", &i.phone).kind(InputKind::Tel),
            ));
        }
        let shown = self.shown_lines();
        let lines: Vec<Field> = [
            field("line1", "Address line 1", &i.line1),
            field("line2", "Address line 2", &i.line2),
            field("line3", "Address line 3", &i.line3),
        ]
        .into_iter()
        .take(shown)
        .collect();
        let add_line = (shown < 3).then(|| {
            AddMore::new(
                "address-add-line",
                format!("Add address line {}", shown + 1),
                "add_line",
            )
        });
        let mut confidence = field("parse_confidence", "Parse confidence", &i.parse_confidence)
            .kind(InputKind::Number);
        confidence.attrs.min = Some("0".into());
        confidence.attrs.max = Some("1".into());
        confidence.attrs.step = Some("0.0001".into());
        let provenance = vec![
            bounded(field("source", "Source", &i.source)),
            bounded(field("source_ref", "Source reference", &i.source_ref)),
            bounded(field("parser_version", "Parser version", &i.parser_version)),
            confidence,
            field(
                "verification_status",
                "Verification status",
                &i.verification_status,
            )
            .kind(InputKind::Select)
            .options(bilibi_ui::choices(&VERIFICATION))
            .required(),
        ];
        // The folded provenance still reads true: what it holds now, and
        // whether something in it needs fixing (which also opens it).
        let provenance_open = self.errors.iter().any(|e| PROVENANCE.contains(&e.field));
        let status = i.verification_status.as_deref().unwrap_or_default();
        let provenance_summary = [
            match i.source.as_deref().map(str::trim) {
                Some(source) if !source.is_empty() => format!("Source: {source}"),
                _ => "No source".to_owned(),
            },
            VERIFICATION
                .iter()
                .find(|(_, value)| *value == status)
                .map_or_else(String::new, |(label, _)| (*label).to_owned()),
            if provenance_open {
                "has a problem".to_owned()
            } else {
                String::new()
            },
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        let header = render(Header {
            title: Html::text("Create Address"),
            actions: render(BackLink {
                href: "/addresses",
                id: "address-back",
                title: "Back to addresses",
                ..BackLink::default()
            })?,
            ..Header::default()
        })?;
        let html = View {
            flash: self.flash.map(),
            header,
            summary,
            contact,
            restricted,
            lines,
            add_line,
            location: self.location.render(
                "address",
                &IDS,
                &LABELS,
                &self.countries,
                &self.errors,
            )?,
            provenance,
            provenance_summary,
            provenance_open,
            raw_input: field("raw_input", "Raw input", &i.raw_input)
                .kind(InputKind::Textarea)
                .attr("maxlength", "10000"),
            submit: render(Button {
                id: "address-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                disabled: !can_create,
                working: "Creating…",
                body: Html::text("Create Address"),
                ..Button::default()
            })?,
        }
        .render()
        .map_err(|_| Error::Internal)?;
        Ok(LiveBody {
            html,
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for Create {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                match event(value)? {
                    "validate" => self.take(value).await?,
                    "create" if !self.created => {
                        self.flash.clear();
                        self.attempted = true;
                        self.take(value).await?;
                        self.save(request.scope, request.credential).await?;
                    }
                    "add_line" => self.lines = (self.shown_lines() + 1).min(3),
                    "create" => {}
                    "clear-flash" => self.flash.dismiss(value.get("key").and_then(Value::as_str)),
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(request.scope).await
        })
    }
}
