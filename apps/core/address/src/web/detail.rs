//! `/addresses/{id}` (Bilimbi `ShowLive`): the address read first. An
//! operator holding `admin.address.update` edits each fact in place; a
//! committed edit saves by itself and reports on that fact:
//!
//! - a text fact (label, phone, street lines, source, source reference)
//!   commits on Enter or leaving the field; Escape cancels;
//! - the verification status commits on change;
//! - country, division, postcode and locality depend on one another, so they
//!   commit together through one grouped editor with Apply and Cancel.
//!
//! Linked Entities lists every Organisation and Person using the address,
//! sorted in the URL (`linked_sort_by`/`linked_sort_dir`). `?company=ID` or
//! `?person=ID` naming a linked owner adds the way back to it.
use super::{
    location::{Ids, Labels, LocationDraft},
    *,
};
use crate::{
    contributions::UPDATE, AddressInput, Detail as Record, FieldError, LinkedOwner, Location,
};
use bilibi_ui::{
    BackLink, Badge, BuiltinColumn, Button, ButtonVariant, Card, Choice, ColumnsOutcome,
    CommitOutcome, CommitStates, CommitStatus, Fact, Facts, Field, FlexRow, FlexTable, Header,
    IconButton, IconContext, InlineChoice, InlineEdit, InlineField, InputKind, ListColumns,
    Restricted, SectionHeading, SortDir, TableEmpty,
};

const FORBIDDEN: &str = "You do not have permission to update addresses.";
const IDS: Ids = Ids {
    prefix: "address-location",
    country: "address-location-country",
    admin1: "address-location-admin1",
};
const LOCATION_LABELS: Labels = Labels {
    admin1: "State or Province",
    admin1_prompt: "Choose a division",
    postcode: "Postal Code",
    locality: "Locality / City",
};
/// The facts an inline edit may write, by the name the editor posts.
const TEXT_FACTS: [(&str, &str); 7] = [
    ("label", "Label"),
    ("phone", "Phone"),
    ("line1", "Address Line 1"),
    ("line2", "Address Line 2"),
    ("line3", "Address Line 3"),
    ("source", "Source"),
    ("source_ref", "Source Reference"),
];
const LINKED_SORTS: [&str; 7] = [
    "type",
    "name",
    "kind",
    "is_primary",
    "priority",
    "valid_from",
    "valid_to",
];
fn linked_columns() -> Vec<BuiltinColumn> {
    [
        ("type", "Entity Type", "sort-type"),
        ("name", "Name", "sort-name"),
        ("kind", "Kind", "sort-kind"),
        ("is_primary", "Primary", "sort-is-primary"),
        ("priority", "Priority", "sort-priority"),
        ("valid_from", "Valid From", "sort-valid-from"),
        ("valid_to", "Valid To", "sort-valid-to"),
    ]
    .into_iter()
    .map(|(id, label, sort_id)| {
        let mut column = BuiltinColumn::new(id, label).sortable(id);
        column.sort_id = Some(sort_id.into());
        column
    })
    .collect()
}
fn fact_slot(name: &str) -> fn(&mut AddressInput) -> &mut Option<String> {
    match name {
        "label" => |a| &mut a.label,
        "phone" => |a| &mut a.phone,
        "line1" => |a| &mut a.line1,
        "line2" => |a| &mut a.line2,
        "line3" => |a| &mut a.line3,
        "source" => |a| &mut a.source,
        _ => |a| &mut a.source_ref,
    }
}

#[derive(Template)]
#[template(path = "address_detail.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    address_id: String,
    header: Html,
    details: Html,
    location: Html,
    provenance: Html,
    linked: Html,
}
#[derive(Template)]
#[template(path = "address_actions.html")]
struct Actions {
    history: bool,
    back: Html,
}
#[derive(Template)]
#[template(path = "address_location_form.html")]
struct LocationForm {
    fields: Html,
    apply: Html,
    cancel: Html,
}

/// The owner the page came from, for the way back.
#[derive(Clone, Copy)]
enum Context {
    Company(i64),
    Person(i64),
}
/// The grouped location editor: its draft, the version it began from and the
/// refusals Apply returned.
struct Editing {
    draft: LocationDraft,
    version: i64,
    errors: Vec<FieldError>,
}

pub(super) struct Detail {
    pages: Pages,
    id: AddressId,
    query: HashMap<String, String>,
    sort_by: String,
    sort_dir: SortDir,
    columns: ListColumns,
    statuses: CommitStates,
    editing: Option<Editing>,
    countries: Vec<Choice>,
    flash: Flash,
    history: bool,
}
impl Detail {
    pub fn new(pages: Pages, id: AddressId, query: &HashMap<String, String>) -> Result<Self> {
        let mut detail = Self {
            pages,
            id,
            query: HashMap::new(),
            sort_by: "type".into(),
            sort_dir: SortDir::Asc,
            columns: ListColumns::mount("address-linked-entities-table", linked_columns())
                .map_err(|_| Error::Internal)?,
            statuses: CommitStates::default(),
            editing: None,
            countries: Vec::new(),
            flash: Flash::default(),
            history: false,
        };
        detail.params(query);
        Ok(detail)
    }
    fn params(&mut self, query: &HashMap<String, String>) {
        let sort = query.get("linked_sort_by").map(String::as_str);
        self.sort_by = LINKED_SORTS
            .into_iter()
            .find(|key| Some(*key) == sort)
            .unwrap_or("type")
            .into();
        self.sort_dir = match query.get("linked_sort_dir").map(String::as_str) {
            Some("desc") => SortDir::Desc,
            _ => SortDir::Asc,
        };
        self.query = query
            .iter()
            .filter(|(key, _)| ["company", "person"].contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }
    fn query(&self) -> HashMap<String, String> {
        let mut query = self.query.clone();
        query.insert("linked_sort_by".into(), self.sort_by.clone());
        query.insert("linked_sort_dir".into(), self.sort_dir.key().into());
        query
    }
    /// A `?company=`/`?person=` that names one of this address's owners.
    fn context(&self, record: &Record) -> Option<Context> {
        let named =
            |key: &str| bilibi_ui::params::positive_id(self.query.get(key).map(String::as_str));
        record
            .owners
            .iter()
            .find_map(|owner| match owner.attachment.owner {
                PartyId::Organisation(id) if named("company") == Some(id.get()) => {
                    Some(Context::Company(id.get()))
                }
                PartyId::Person(id) if named("person") == Some(id.get()) => {
                    Some(Context::Person(id.get()))
                }
                _ => None,
            })
    }

    // --- Saving -----------------------------------------------------------

    /// One fact, one commit, one outcome on that fact. The rest of the input
    /// is the stored address, so a restricted phone stays absent and kept.
    async fn save_fact(
        &mut self,
        scope: &Scope,
        credential: &str,
        name: &str,
        submitted: &str,
        version: i64,
    ) -> Result<()> {
        let label = TEXT_FACTS
            .iter()
            .find(|(key, _)| *key == name)
            .map_or("Verification Status", |(_, label)| *label);
        let current = self.pages.addresses.read(scope, self.id).await?;
        let mut address = current.address;
        if name == "verification_status" {
            address.verification_status = Some(submitted.into());
        } else {
            *fact_slot(name)(&mut address) = Some(submitted.into());
        }
        let result = self
            .pages
            .submit(
                scope,
                credential,
                "save_field",
                "core.address.update",
                serde_json::json!({"address_id":self.id,"version":version,"address":address}),
            )
            .await?;
        let status = match result {
            Ok(_) => CommitStatus::Saved,
            Err(error) if fatal(&error) => return Err(Error::Unauthorized),
            Err(WriteError::Invalid(errors)) => {
                let reasons: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                CommitStatus::refusal(label, submitted, &reasons)
            }
            Err(WriteError::Stale) => CommitStatus::refusal(
                label,
                submitted,
                &["has changed since this edit began; reload before trying again"],
            ),
            Err(WriteError::Access(Error::Forbidden)) => {
                self.statuses.clear_saved();
                CommitStatus::Error(FORBIDDEN.into())
            }
            Err(error) => failure(&error),
        };
        self.statuses.put(name, status);
        Ok(())
    }
    async fn save_location(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<()> {
        let Some(editing) = &mut self.editing else {
            return Ok(());
        };
        editing
            .draft
            .change(
                &self.pages.addresses,
                &LocationDraft::posted(&form(value, "location")?),
            )
            .await?;
        let current = self.pages.addresses.read(scope, self.id).await?;
        let mut address = current.address;
        editing.draft.location.apply_to(&mut address);
        let version = editing.version;
        match self
            .pages
            .submit(
                scope,
                credential,
                "save_location",
                "core.address.update",
                serde_json::json!({"address_id":self.id,"version":version,"address":address}),
            )
            .await?
        {
            Ok(_) => {
                self.editing = None;
                self.statuses.put("location", CommitStatus::Saved);
            }
            Err(error) if fatal(&error) => return Err(Error::Unauthorized),
            // The grouped form reports each refused field on its own input,
            // so the operator corrects it where they typed it.
            Err(WriteError::Invalid(errors)) => {
                editing.errors = errors;
                self.statuses.put("location", CommitStatus::None);
            }
            Err(WriteError::Stale) => self.statuses.put(
                "location",
                CommitStatus::Error(
                    "The address changed while you edited its location. Cancel and apply again."
                        .into(),
                ),
            ),
            Err(WriteError::Access(Error::Forbidden)) => {
                self.editing = None;
                self.statuses
                    .put("location", CommitStatus::Error(FORBIDDEN.into()));
            }
            Err(error) => self.statuses.put("location", failure(&error)),
        }
        Ok(())
    }
    async fn handle(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<bool> {
        match event(value)? {
            "save_field" => {
                let name = value["field"].as_str().ok_or(Error::Invalid)?;
                if !(name == "verification_status" || TEXT_FACTS.iter().any(|(k, _)| *k == name))
                    || value["id"].as_str() != Some(&self.id.to_string())
                {
                    return Err(Error::Invalid);
                }
                let submitted = value["value"].as_str().ok_or(Error::Invalid)?;
                let version = value["version"]
                    .as_str()
                    .and_then(|v| v.parse().ok())
                    .ok_or(Error::Invalid)?;
                // Every write asks Authz again: what the page drew is not a decision.
                if self.pages.allowed(scope, UPDATE).await? {
                    self.save_fact(scope, credential, name, submitted, version)
                        .await?;
                } else {
                    self.statuses
                        .put(name, CommitStatus::Error(FORBIDDEN.into()));
                }
            }
            "edit_location" => {
                if self.pages.allowed(scope, UPDATE).await? {
                    let current = self.pages.addresses.read(scope, self.id).await?;
                    if self.countries.is_empty() {
                        self.countries = self.pages.countries().await?;
                    }
                    self.editing = Some(Editing {
                        draft: LocationDraft::new(
                            &self.pages.addresses,
                            Location::of(&current.address),
                        )
                        .await?,
                        version: current.version,
                        errors: Vec::new(),
                    });
                    self.statuses.put("location", CommitStatus::None);
                } else {
                    self.statuses
                        .put("location", CommitStatus::Error(FORBIDDEN.into()));
                }
            }
            "cancel_edit_location" => self.editing = None,
            "validate_location" => {
                if let Some(editing) = &mut self.editing {
                    editing
                        .draft
                        .change(
                            &self.pages.addresses,
                            &LocationDraft::posted(&form(value, "location")?),
                        )
                        .await?;
                }
            }
            "save_location" => {
                if self.pages.allowed(scope, UPDATE).await? {
                    self.save_location(scope, credential, value).await?;
                } else {
                    self.editing = None;
                    self.statuses
                        .put("location", CommitStatus::Error(FORBIDDEN.into()));
                }
            }
            "grid" => match self.columns.handle(
                &bilibi_ui::params::string_values(value)
                    .map_err(bilibi_database::Error::refused)?,
            ) {
                ColumnsOutcome::Sort(key) => {
                    self.sort_dir = if key == self.sort_by && self.sort_dir == SortDir::Asc {
                        SortDir::Desc
                    } else {
                        SortDir::Asc
                    };
                    self.sort_by = key;
                }
                ColumnsOutcome::Update | ColumnsOutcome::Noop => {}
            },
            "clear-flash" => self.flash.dismiss(value.get("key").and_then(Value::as_str)),
            _ => return Err(Error::Invalid),
        }
        Ok(true)
    }

    // --- Rendering ----------------------------------------------------------

    fn text_fact(&self, record: &Record, name: &str, label: &str, editable: bool) -> Result<Html> {
        let a = &record.address;
        if a.restricted.iter().any(|r| r == name) {
            return render(Restricted::new(format!("address-{name}-restricted")));
        }
        let mut stored = a.address.clone();
        let value = fact_slot(name)(&mut stored).clone().unwrap_or_default();
        let mut field = InlineField::new(
            format!("address-{}", name.replace('_', "-")),
            name,
            label,
            value,
        )
        .version(a.version)
        .status(self.statuses.get(name))
        .allow_empty();
        field.id_value = self.id.to_string();
        if name == "label" {
            field.class = "font-medium".into();
        }
        if !editable {
            field = field.readonly();
        }
        render(InlineEdit { field })
    }
    fn details(&self, record: &Record, editable: bool) -> Result<Html> {
        let a = &record.address;
        let status = a
            .address
            .verification_status
            .clone()
            .unwrap_or_else(|| "unverified".into());
        let mut choice = InlineField::new(
            "address-verification-status",
            "verification_status",
            "Edit verification status",
            &status,
        )
        .version(a.version)
        .status(self.statuses.get("verification_status"));
        choice.id_value = self.id.to_string();
        if !editable {
            choice = choice.readonly();
        }
        let display = render(Badge::status(
            super::index::status_kind(&status),
            &capitalize(&status),
        ))?;
        let editor = render(
            Field::named("verification_status")
                .id("address-verification-status-select")
                .label_class("sr-only")
                .label("Verification status")
                .kind(InputKind::Select)
                .value(&status)
                .options(bilibi_ui::choices(&[
                    ("Unverified", "unverified"),
                    ("Suggested", "suggested"),
                    ("Verified", "verified"),
                ])),
        )?;
        let mut items = Vec::new();
        for (name, label) in &TEXT_FACTS[..2] {
            items.push(Fact {
                title: super::fields::get(name).label,
                id: if *name == "label" {
                    "address-view-label"
                } else {
                    "address-view-phone"
                },
                value: self.text_fact(record, name, label, editable)?,
            });
        }
        items.push(Fact {
            title: super::fields::VERIFICATION_STATUS.label,
            id: "address-view-verification-status",
            value: render(InlineChoice {
                field: choice,
                display,
                editor,
            })?,
        });
        // The street lines start their own row, as on Create Address.
        let mut lines = Vec::new();
        for (name, label, id) in [
            ("line1", "Address Line 1", "address-view-line1"),
            ("line2", "Address Line 2", "address-view-line2"),
            ("line3", "Address Line 3", "address-view-line3"),
        ] {
            lines.push(Fact {
                title: super::fields::get(name).label,
                id,
                value: self.text_fact(record, name, label, editable)?,
            });
        }
        let mut body = render(SectionHeading {
            id: "address-details-heading",
            title: "Address Details",
            ..SectionHeading::default()
        })?;
        body.push(&render(Facts {
            id: "address-details-facts",
            items,
        })?);
        body.push(&render(Facts {
            id: "address-lines-facts",
            items: lines,
        })?);
        render(Card {
            id: "address-details-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "address-details-heading",
            body,
            ..Card::default()
        })
    }
    fn location(&self, record: &Record, editable: bool) -> Result<Html> {
        let a = &record.address.address;
        let edit = if editable && self.editing.is_none() {
            render(IconButton {
                context: IconContext::Inline,
                id: "address-edit-location-button",
                event: "edit_location",
                ..IconButton::new("edit", "Edit location")
            })?
        } else {
            Html::EMPTY
        };
        let mut body = render(SectionHeading {
            id: "address-location-heading",
            title: "Geographic Location",
            title_actions: edit,
            description: "Country, state, postcode and locality are edited together.",
            ..SectionHeading::default()
        })?;
        body.push(&render(CommitOutcome {
            id: "address-location-status".into(),
            status: self.statuses.get("location"),
        })?);
        match &self.editing {
            Some(editing) => body.push(&render(LocationForm {
                fields: editing.draft.render(
                    "location",
                    &IDS,
                    &LOCATION_LABELS,
                    &self.countries,
                    &editing.errors,
                )?,
                apply: render(Button {
                    id: "address-save-location",
                    button_type: "submit",
                    variant: ButtonVariant::Primary,
                    working: "Applying…",
                    body: Html::text("Apply Location"),
                    ..Button::default()
                })?,
                cancel: render(Button {
                    id: "address-cancel-location",
                    button_type: "button",
                    event: "cancel_edit_location",
                    body: Html::text("Cancel"),
                    ..Button::default()
                })?,
            })?),
            None => {
                let shown = |name: &Option<String>, code: &Option<String>| {
                    Html::text(name.as_deref().or(code.as_deref()).unwrap_or("—"))
                };
                body.push(&render(Facts {
                    id: "address-location-facts",
                    items: vec![
                        Fact {
                            title: super::fields::COUNTRY_ISO.label,
                            id: "address-view-country",
                            value: shown(&record.country_name, &a.country_iso),
                        },
                        Fact {
                            title: super::fields::ADMIN1_CODE.label,
                            id: "address-view-admin1",
                            value: shown(&record.admin1_name, &a.admin1_code),
                        },
                        Fact {
                            title: super::fields::POSTCODE.label,
                            id: "address-view-postcode",
                            value: Html::raw(format!(
                                "<span class=\"tabular-nums\">{}</span>",
                                Html::text(a.postcode.as_deref().unwrap_or("—"))
                            )),
                        },
                        Fact {
                            title: super::fields::LOCALITY.label,
                            id: "address-view-locality",
                            value: Html::text(a.locality.as_deref().unwrap_or("—")),
                        },
                    ],
                })?);
            }
        }
        render(Card {
            id: "address-location-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "address-location-heading",
            body,
            ..Card::default()
        })
    }
    fn provenance(&self, record: &Record, editable: bool) -> Result<Html> {
        let a = &record.address.address;
        let mut items = vec![
            Fact {
                title: super::fields::SOURCE.label,
                id: "address-view-source",
                value: self.text_fact(record, "source", "Source", editable)?,
            },
            Fact {
                title: super::fields::SOURCE_REF.label,
                id: "address-view-source-ref",
                value: self.text_fact(record, "source_ref", "Source Reference", editable)?,
            },
            Fact {
                title: super::fields::PARSER_VERSION.label,
                id: "address-view-parser-version",
                value: Html::text(a.parser_version.as_deref().unwrap_or("—")),
            },
            Fact {
                title: super::fields::PARSE_CONFIDENCE.label,
                id: "address-view-parse-confidence",
                value: Html::raw(format!(
                    "<span class=\"tabular-nums\">{}</span>",
                    Html::text(a.parse_confidence.as_deref().unwrap_or("—"))
                )),
            },
        ];
        if let Some(raw) = &a.raw_input {
            items.push(Fact {
                title: super::fields::RAW_INPUT.label,
                id: "address-view-raw-input",
                value: Html::raw(format!(
                    "<pre class=\"overflow-x-auto rounded-xl border border-line bg-surface-muted p-3 font-mono text-xs text-ink\">{}</pre>",
                    Html::text(raw)
                )),
            });
        }
        let mut body = render(SectionHeading {
            id: "address-provenance-heading",
            title: "Provenance",
            description: "See where this address came from and how it was entered.",
            ..SectionHeading::default()
        })?;
        body.push(&render(Facts {
            id: "address-provenance-facts",
            items,
        })?);
        render(Card {
            id: "address-provenance-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "address-provenance-heading",
            body,
            ..Card::default()
        })
    }
    fn linked(&mut self, record: &Record) -> Result<Html> {
        let mut owners: Vec<&LinkedOwner> = record.owners.iter().collect();
        sort_owners(&mut owners, &self.sort_by, self.sort_dir);
        let dash = || "<span class=\"text-ink-muted\">—</span>".to_owned();
        let mut rows = Vec::with_capacity(owners.len());
        for owner in owners {
            let a = &owner.attachment.attachment;
            let (kind, link) = match owner.attachment.owner {
                PartyId::Organisation(id) => ("Company", format!("<a href=\"/companies/{id}\" id=\"linked-company-{id}\" class=\"text-action hover:underline\">{}</a>", Html::text(&owner.name))),
                PartyId::Person(id) => ("Person", format!("<a href=\"/contacts/{id}\" id=\"linked-person-{id}\" class=\"text-action hover:underline\">{}</a>", Html::text(&owner.name))),
            };
            let mut kinds = String::new();
            for k in &a.kinds {
                kinds.push_str(
                    &render(Badge {
                        body: Html::text(k.label()),
                        ..Badge::default()
                    })?
                    .to_string(),
                );
            }
            if kinds.is_empty() {
                kinds = dash();
            }
            let date = |d: Option<&crate::CalendarDate>| {
                format!(
                    "<span class=\"whitespace-nowrap tabular-nums text-sm text-ink-muted\">{}</span>",
                    d.map_or("—", |d| d.as_str())
                )
            };
            rows.push(
                FlexRow::new(owner.attachment.id.to_string())
                    .slot("type", Html::raw(format!("<span class=\"whitespace-nowrap font-medium text-ink\">{kind}</span>")))
                    .slot("name", Html::raw(format!("<span class=\"whitespace-nowrap font-medium\">{link}</span>")))
                    .slot("kind", Html::raw(format!("<div class=\"flex flex-wrap gap-1\">{kinds}</div>")))
                    .slot("is_primary", Html::raw(format!("<span class=\"whitespace-nowrap text-sm text-ink-muted\">{}</span>", if a.is_primary { "Yes" } else { "No" })))
                    .slot("priority", Html::raw(format!("<span class=\"whitespace-nowrap tabular-nums text-sm text-ink-muted\">{}</span>", a.priority)))
                    .slot("valid_from", Html::raw(date(a.dates.from())))
                    .slot("valid_to", Html::raw(date(a.dates.to()))),
            );
        }
        let empty = rows.is_empty();
        let mut table = FlexTable::new(
            "address-linked-entities-table",
            self.columns.flex_columns(),
            rows,
        )
        .sorted(Some(&self.sort_by), Some(self.sort_dir));
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Linked entities";
        if empty {
            table.empty = Some(TableEmpty::Content(Html::text("No linked entities.")));
        }
        let mut body = Html::raw(format!(
            "<div class=\"px-5 pb-4 pt-5 sm:px-6\">{}</div>",
            render(SectionHeading {
                id: "address-linked-entities-heading",
                title: "Linked Entities",
                description: "See which companies and people use this address.",
                ..SectionHeading::default()
            })?
        ));
        body.push(&render(table)?);
        render(Card {
            id: "address-linked-entities-card",
            inner_class: "p-0",
            role: "region",
            labelledby: "address-linked-entities-heading",
            body,
            ..Card::default()
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let record = self.pages.addresses.detail(scope, self.id).await?;
        let editable = self.pages.allowed(scope, UPDATE).await?;
        if !editable {
            self.editing = None;
        }
        self.history = self.pages.allowed(scope, AUDIT_LIST).await?;
        let mut back = Html::EMPTY;
        match self.context(&record) {
            Some(Context::Company(id)) => back.push(&render(BackLink {
                href: &format!("/companies/{id}"),
                id: "address-back-company",
                title: "Back to company",
                ..BackLink::default()
            })?),
            Some(Context::Person(id)) => back.push(&render(BackLink {
                href: &format!("/contacts/{id}"),
                id: "address-back-person",
                title: "Back to person",
                ..BackLink::default()
            })?),
            None => {}
        }
        back.push(&render(BackLink {
            href: "/addresses",
            id: "address-back-list",
            title: "Back to addresses",
            ..BackLink::default()
        })?);
        let title = record
            .address
            .address
            .label
            .clone()
            .unwrap_or_else(|| format!("Address #{}", self.id));
        let header = render(Header {
            title: Html::text("Address Details"),
            subtitle: &title,
            actions: render(Actions {
                history: self.history,
                back,
            })?,
            ..Header::default()
        })?;
        let linked = self.linked(&record)?;
        let html = View {
            flash: self.flash.map(),
            address_id: self.id.to_string(),
            header,
            details: self.details(&record, editable)?,
            location: self.location(&record, editable)?,
            provenance: self.provenance(&record, editable)?,
            linked,
        }
        .render()
        .map_err(|_| Error::Internal)?;
        Ok(LiveBody {
            html,
            query: self.query(),
            rows: 1,
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for Detail {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => self.params(query),
                LiveInput::Event(value) => {
                    self.handle(request.scope, request.credential, value)
                        .await?;
                }
            }
            self.view(request.scope).await
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        if !self.history {
            return Vec::new();
        }
        vec![PanelSpec {
            id: "address-record-history".into(),
            embed: HISTORY_EMBED.into(),
            path: HashMap::from([
                ("panel".into(), "address-record-history".into()),
                ("kinds".into(), "address".into()),
                ("id".into(), self.id.to_string()),
            ]),
            query: HashMap::new(),
        }]
    }
}

/// The page's sentence for a refusal other than validation or staleness.
fn failure(error: &WriteError) -> CommitStatus {
    match error {
        WriteError::NotFound => CommitStatus::Error(
            "This address no longer exists. Return to the list to find its replacement.".into(),
        ),
        WriteError::Archived => CommitStatus::Error(
            "An archived company uses this address, so it is read-only and the change was not saved."
                .into(),
        ),
        _ => CommitStatus::failure(),
    }
}
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
fn sort_owners(owners: &mut [&LinkedOwner], by: &str, dir: SortDir) {
    owners.sort_by(|a, b| {
        let (x, y) = (&a.attachment, &b.attachment);
        let order = match by {
            "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            "kind" => {
                let keys = |o: &crate::Attachment| {
                    o.attachment
                        .kinds
                        .iter()
                        .map(|k| k.key())
                        .collect::<Vec<_>>()
                        .join(",")
                };
                keys(x).cmp(&keys(y))
            }
            "is_primary" => x.attachment.is_primary.cmp(&y.attachment.is_primary),
            "priority" => x.attachment.priority.cmp(&y.attachment.priority),
            "valid_from" => x
                .attachment
                .dates
                .from()
                .map(|d| d.as_str())
                .cmp(&y.attachment.dates.from().map(|d| d.as_str())),
            "valid_to" => x
                .attachment
                .dates
                .to()
                .map(|d| d.as_str())
                .cmp(&y.attachment.dates.to().map(|d| d.as_str())),
            _ => matches!(x.owner, PartyId::Person(_)).cmp(&matches!(y.owner, PartyId::Person(_))),
        };
        let order = if dir == SortDir::Desc {
            order.reverse()
        } else {
            order
        };
        order.then(x.id.cmp(&y.id))
    });
}
