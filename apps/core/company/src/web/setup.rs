//! `/setup/platform-operator` (Bilimbi `PlatformOperatorSetupLive`, ported
//! from Belimbing's `Setup/PlatformOperator`): designate an existing company
//! of the operator tenant, or create one, as the operator's primary company.
//! `bilibi-web platform-provision` stays the command-line path. The route is
//! an operator page, and each command proves the operator again on its own
//! transaction. Once the operator stands on a company the page sends the
//! reader to that company, as Bilimbi's mount does.
use super::{
    views::{allowed, render},
    Flash,
};
use crate::{primary, CompanyService, CreateOutcome, FieldError, NewCompany, WriteError};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    BackLink, Button, ButtonVariant, Choice, Field, FlashMap, Header, Html, InputKind,
    RestrictedField, StatusKind,
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

/// The create form's fields as (name, label, kind, maxlength), in Bilimbi's
/// order: two identity columns, then three contact columns.
const FIELDS: [(&str, &str, InputKind, &str); 7] = [
    ("name", "Name", InputKind::Text, "255"),
    ("legal_name", "Legal Name", InputKind::Text, "255"),
    (
        "registration_number",
        "Registration Number",
        InputKind::Text,
        "255",
    ),
    ("tax_id", "Tax ID", InputKind::Text, "255"),
    ("jurisdiction", "Jurisdiction", InputKind::Text, "2"),
    ("email", "Email", InputKind::Email, "255"),
    ("website", "Website", InputKind::Text, "255"),
];

#[derive(Template)]
#[template(path = "platform_operator_setup.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    select_mode: bool,
    has_companies: bool,
    company: Html,
    designate: Html,
    identity: Vec<Html>,
    contacts: Vec<Html>,
    save: Html,
}

#[derive(Template)]
#[template(
    source = r#"<p id="platform-operator-settled" class="text-sm text-ink-muted">The platform operator stands on <a href="{{ href }}" class="text-action hover:underline">{{ name }}</a>.</p>"#,
    ext = "html"
)]
struct Settled<'a> {
    href: &'a str,
    name: &'a str,
}

pub(crate) struct Setup {
    db: Db,
    company: Arc<CompanyService>,
    /// `None` until the first draw decides it from whether any company exists.
    select_mode: Option<bool>,
    form: HashMap<String, String>,
    errors: Vec<FieldError>,
    flash: Flash,
}
impl Setup {
    pub fn new(db: Db, company: Arc<CompanyService>) -> Self {
        Self {
            db,
            company,
            select_mode: None,
            form: HashMap::new(),
            errors: Vec::new(),
            flash: Flash::default(),
        }
    }

    /// The page, or where to send the reader: the operator's company once it
    /// stands on one.
    async fn draw(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.db.get().await?;
        if let Some(company) = primary::platform_operator_company(&**client).await? {
            let href = format!("/companies/{}", company.id.get());
            // Without the live socket the page still says where to go.
            let html = render(&Settled {
                href: &href,
                name: &company.display_name,
            })?;
            return Ok(LiveBody {
                html: html.to_string(),
                redirect: Some(href),
                ..LiveBody::default()
            });
        }
        let companies = client
            .query(
                "SELECT id, name, legal_name FROM companies WHERE tenant_id = $1 AND deleted_at IS NULL ORDER BY id",
                &[&scope.tenant()],
            )
            .await?;
        let has_companies = !companies.is_empty();
        let select_mode = *self.select_mode.get_or_insert(has_companies);
        let masked = bilibi_authz::restrictions(&**client, scope).await?;
        let header = render(&Header {
            title: Html::text("Set Up Platform Operator"),
            subtitle: "Choose or create the company that runs this platform.",
            actions: render(&BackLink {
                id: "platform-operator-back",
                href: "/companies",
                title: "Back to companies",
                ..BackLink::default()
            })?,
            ..Header::default()
        })?;
        let options = companies
            .iter()
            .map(|row| {
                let id: i64 = row.get(0);
                Choice::new(
                    crate::organisation::display_name(row.get(2), row.get(1)),
                    id.to_string(),
                )
            })
            .collect();
        let company = render(
            &Field::named("company_id")
                .id("platform-operator-company")
                .kind(InputKind::Select)
                .label("Company")
                .prompt("Select a company...")
                .options(options),
        )?;
        let designate = render(&Button {
            id: "platform-operator-designate",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Set as Primary Company"),
            ..Button::default()
        })?;
        let mut fields = Vec::with_capacity(FIELDS.len());
        for (name, label, kind, maxlength) in FIELDS {
            let id = format!("platform-operator-{}", name.replace('_', "-"));
            if masked.contains(name) {
                fields.push(render(&RestrictedField::new(
                    format!("{id}-restricted"),
                    label,
                ))?);
                continue;
            }
            let mut field = Field::named(name)
                .id(id)
                .kind(kind)
                .label(label)
                .value(self.form.get(name).cloned().unwrap_or_default())
                .attr("maxlength", maxlength)
                .errors(
                    self.errors
                        .iter()
                        .filter(|error| error.field == name)
                        .map(|error| error.message.clone())
                        .collect(),
                );
            if name == "name" {
                field = field.required();
            }
            fields.push(render(&field)?);
        }
        let contacts = fields.split_off(4);
        let save = render(&Button {
            id: "platform-operator-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Creating…",
            body: Html::text("Create Primary Company"),
            ..Button::default()
        })?;
        let html = render(&View {
            flash: self.flash.map(),
            header,
            select_mode,
            has_companies,
            company,
            designate,
            identity: fields,
            contacts,
            save,
        })?;
        Ok(LiveBody {
            html: html.to_string(),
            rows: 1,
            ..LiveBody::default()
        })
    }

    /// One event; `Some(url)` when the operator now stands on that company.
    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<Option<String>> {
        let get = |key| value.get(key).and_then(Value::as_str).unwrap_or("");
        match get("type") {
            "set_mode" => match get("mode") {
                "select" => self.select_mode = Some(true),
                "create" => self.select_mode = Some(false),
                _ => return Err(Error::Invalid),
            },
            "designate" => {
                let Some(id) = get("company_id").parse::<i64>().ok().filter(|id| *id > 0) else {
                    self.flash
                        .put(StatusKind::Error, "Select a company in this workspace.");
                    return Ok(None);
                };
                match self.company.designate_operator_company(scope, id).await {
                    Ok(_) => {
                        self.flash.put(
                            StatusKind::Success,
                            "Platform-operator primary company designated.",
                        );
                        return Ok(Some(format!("/companies/{id}")));
                    }
                    Err(WriteError::Access(Error::Unauthorized)) => {
                        return Err(Error::Unauthorized)
                    }
                    Err(WriteError::NotFound) => self
                        .flash
                        .put(StatusKind::Error, "That company is not in this workspace."),
                    Err(WriteError::Invalid(_)) => self.flash.put(
                        StatusKind::Error,
                        "This tenant already has a primary company.",
                    ),
                    Err(_) => self.flash.put(
                        StatusKind::Error,
                        "Could not designate the primary company.",
                    ),
                }
            }
            "create" => {
                self.form = FIELDS
                    .iter()
                    .filter_map(|(name, ..)| {
                        value
                            .get(*name)
                            .and_then(Value::as_str)
                            .map(|v| (name.to_string(), v.to_owned()))
                    })
                    .collect();
                self.flash.clear();
                self.errors = Vec::new();
                let text = |name: &str| {
                    self.form
                        .get(name)
                        .map(|v| v.trim())
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                };
                let Some(name) = text("name") else {
                    self.errors.push(FieldError::new("name", "can't be blank"));
                    return Ok(None);
                };
                // A restricted field is never rendered, so it never posts; the
                // owner refuses one anyway.
                let new = NewCompany {
                    name,
                    status: Some("active".into()),
                    legal_name: text("legal_name"),
                    registration_number: text("registration_number"),
                    tax_id: text("tax_id"),
                    jurisdiction: text("jurisdiction"),
                    email: text("email"),
                    website: text("website"),
                    ..NewCompany::default()
                };
                match self.company.create_operator_company(scope, &new).await {
                    Ok(CreateOutcome::Created(company)) => {
                        self.flash.put(
                            StatusKind::Success,
                            "Platform-operator primary company created.",
                        );
                        return Ok(Some(format!("/companies/{}", company.id)));
                    }
                    Ok(CreateOutcome::Refused(errors)) => self.errors = errors,
                    Err(WriteError::Access(Error::Unauthorized)) => {
                        return Err(Error::Unauthorized)
                    }
                    Err(WriteError::Invalid(_)) => self.flash.put(
                        StatusKind::Error,
                        "This tenant already has a primary company.",
                    ),
                    Err(_) => self
                        .flash
                        .put(StatusKind::Error, "Could not create the primary company."),
                }
            }
            "clear-flash" => self.flash.dismiss(Some(get("key"))),
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}
impl LiveState<Scope, Error> for Setup {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => {}
                LiveInput::Event(value) => {
                    if let Some(url) = self.event(request.scope, value).await? {
                        return Ok(LiveBody {
                            redirect: Some(url),
                            flash: self.flash.take(),
                            ..LiveBody::default()
                        });
                    }
                }
            }
            // The route admits only the operator holding `admin.company.update`;
            // a reader who lost it since mounting is refused here too.
            let client = self.db.get().await?;
            if !allowed(&**client, request.scope, crate::UPDATE).await? {
                return Err(Error::Forbidden);
            }
            drop(client);
            self.draw(request.scope).await
        })
    }
}
