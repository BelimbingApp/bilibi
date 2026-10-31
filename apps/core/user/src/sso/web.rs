//! Company identity administration; extends Bilimbi's User settings UI.
use super::*;
use askama::Template;
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Provenance,
};
use bilibi_ui::{
    page::*, Button, ButtonVariant, Choice, Field, Header, Html, InputKind, SecretInput,
};
const SERVICE: &str = "core/user.sso-pages";
const PRESETS: &str = include_str!("../../presets/oidc.json");
const UNREADABLE: &str = "The saved settings of this connection cannot be read, for example because the installation encryption key changed. Enter its settings and client secret again and save, or turn it off. Until then it cannot sign anyone in, and people linked to it still follow its password rule.";
pub(crate) struct Pages {
    pub users: Arc<UserService>,
    pub contracts: Arc<bilibi_agent_api::page::PageContracts>,
}
#[derive(Template)]
#[template(path = "sso.html")]
struct View {
    header: Html,
    notice: String,
    kind: bilibi_ui::StatusKind,
    body: Html,
}
#[derive(Template)]
#[template(
    source = "{% for field in fields %}{{ field }}{% endfor %}",
    ext = "html"
)]
struct Fragments {
    fields: Vec<Html>,
}
fn render(v: &impl Template) -> Result<Html> {
    Html::render(v).map_err(|_| Error::Internal)
}
fn button(label: &str, event: &str) -> Result<Html> {
    render(&Button {
        event,
        body: Html::text(label),
        ..Button::default()
    })
}
fn check(name: &str, label: &str, value: bool) -> Result<Html> {
    render(
        &Field::named(name)
            .kind(InputKind::Checkbox)
            .label(label)
            .value("true")
            .checked(value),
    )
}
struct State {
    users: Arc<UserService>,
    contracts: Arc<bilibi_agent_api::page::PageContracts>,
    company: Option<i64>,
    id: Option<i64>,
    config: IdentityConfiguration,
    public: String,
    editing: bool,
    notice: String,
    refused: Option<String>,
    error: bool,
    errors: std::collections::BTreeMap<String, String>,
    raw_mapping: Option<String>,
    confirm: bool,
    unlink: Option<i64>,
    scim_token: Option<String>,
    revoke_scim: bool,
    rotate_scim: bool,
    back: String,
}
impl Pages {
    async fn state(&self, r: &PageRequest<'_, Scope>) -> Result<State> {
        let scope = r.scope.ok_or(Error::Unauthorized)?;
        let company = r
            .path
            .get("id")
            .map(|v| v.parse().map_err(|_| Error::Invalid))
            .transpose()?;
        let id = r
            .query
            .get("connection")
            .map(|v| v.parse().map_err(|_| Error::Invalid))
            .transpose()?;
        let mut unreadable = false;
        let (config, public) = if let Some(company) = company {
            match self.users.identity_configuration(scope, company, id).await {
                Ok(read) => read,
                Err(Error::Invalid) if id.is_some() => {
                    unreadable = true;
                    (IdentityConfiguration::default(), String::new())
                }
                Err(error) => return Err(error),
            }
        } else {
            (IdentityConfiguration::default(), String::new())
        };
        Ok(State {
            users: self.users.clone(),
            contracts: self.contracts.clone(),
            company,
            id,
            config,
            public,
            editing: id.is_none() || unreadable,
            notice: if unreadable {
                UNREADABLE.into()
            } else {
                String::new()
            },
            refused: None,
            error: unreadable,
            errors: Default::default(),
            raw_mapping: None,
            confirm: r.operation == "confirm",
            unlink: None,
            scim_token: None,
            revoke_scim: false,
            rotate_scim: false,
            back: r
                .query
                .get("return")
                .and_then(|v| crate::safe_return(v))
                .unwrap_or_else(|| "/workspace".into()),
        })
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, _: &str) -> Option<&'static str> {
        Some("Company sign-in")
    }
    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let state = self.state(&r).await?;
            Ok(PageBody {
                html: state.view(r.scope.ok_or(Error::Unauthorized)?).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, r: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(
                Box::new(self.state(&r).await?) as Box<dyn LiveState<Scope, Error>>
            ))
        })
    }
}
impl State {
    fn field(&self, name: &str, label: &str, value: &str) -> Result<Html> {
        render(
            &Field::named(name)
                .label(label)
                .value(value)
                .errors(self.errors.get(name).cloned().into_iter().collect()),
        )
    }

    async fn view(&self, scope: &Scope) -> Result<String> {
        let mut parts = Vec::new();
        if !self.errors.is_empty() {
            let mut summary = bilibi_ui::ErrorSummary::new("sso-errors");
            for (target, message) in &self.errors {
                summary.push(target, message);
            }
            parts.push(render(&summary)?);
        }
        if let Some(company) = self.company {
            let client = self.users.db.get().await?;
            self.users.sso_company(&**client, scope, company).await?;
            let rows=client.query("SELECT id FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 ORDER BY id LIMIT 100",&[&scope.tenant(),&company]).await?;
            drop(client);
            let mut choices = vec![Choice::new("New connection", "")];
            for row in rows {
                let id: i64 = row.get(0);
                let label = match self
                    .users
                    .identity_configuration(scope, company, Some(id))
                    .await
                {
                    Ok((c, _)) => c.label,
                    Err(Error::Invalid) => format!("Connection {id} (unreadable)"),
                    Err(error) => return Err(error),
                };
                choices.push(Choice::new(label, id.to_string()));
            }
            let picker = render(
                &Field::named("connection")
                    .kind(InputKind::Select)
                    .label("Connection")
                    .options(choices)
                    .value(self.id.map(|v| v.to_string()).unwrap_or_default()),
            )?;
            parts.push(render(&bilibi_ui::Form {
                id: "sso-connections",
                event: "select",
                body: picker,
                actions: button("Open", "")?,
                ..Default::default()
            })?);
            if !self.editing {
                let issuer = self.config.protocol["issuer"].as_str().unwrap_or("");
                let path = format!("/login?sso={}", self.public);
                parts.push(render(&bilibi_ui::Facts {
                    id: "sso-details",
                    items: vec![
                        bilibi_ui::Fact {
                            title: "Connection",
                            id: "sso-label",
                            value: Html::text(&self.config.label),
                        },
                        bilibi_ui::Fact {
                            title: "Issuer",
                            id: "sso-issuer",
                            value: Html::text(issuer),
                        },
                        bilibi_ui::Fact {
                            title: "Enabled",
                            id: "sso-enabled",
                            value: Html::text(if self.config.enabled { "Yes" } else { "No" }),
                        },
                        bilibi_ui::Fact {
                            title: "Sign-in address",
                            id: "sso-address",
                            value: render(&bilibi_ui::ActionLink {
                                href: &path,
                                title: "Company sign-in",
                                body: Html::text(&path),
                                ..Default::default()
                            })?,
                        },
                    ],
                })?);
                parts.push(button("Change connection", "edit")?);
                let active = self
                    .users
                    .scim_token_present(scope, company, self.id.ok_or(Error::Invalid)?)
                    .await?;
                parts.push(render(&bilibi_ui::SectionHeading {
                    title: "Automatic provisioning (SCIM)",
                    ..Default::default()
                })?);
                parts.push(render(&bilibi_ui::Facts {
                    id: "scim-details",
                    items: vec![
                        bilibi_ui::Fact {
                            title: "Status",
                            id: "scim-status",
                            value: Html::text(if active { "Active" } else { "Off — no token" }),
                        },
                        bilibi_ui::Fact {
                            title: "SCIM address",
                            id: "scim-address",
                            value: Html::text(&format!(
                                "{}/scim/v2/{}",
                                self.users.origin, self.public
                            )),
                        },
                    ],
                })?);
                if let Some(token) = &self.scim_token {
                    parts.push(render(&SecretInput::new("scim-new-token", "scim_new_token", "SCIM token").label("New SCIM token").value(token).readonly().hint("Copy and store this token now. It will not be shown again after leaving this page."))?);
                }
                parts.push(button(
                    if active {
                        "Rotate SCIM token"
                    } else {
                        "Create SCIM token"
                    },
                    if active {
                        "scim-ask-rotate"
                    } else {
                        "scim-rotate"
                    },
                )?);
                if active {
                    parts.push(button("Revoke SCIM token", "scim-ask-revoke")?);
                }
                if self.rotate_scim {
                    parts.push(render(&bilibi_ui::ConfirmDialog {
                        id: "scim-rotate", consequence: "Replace the SCIM token?", detail: "The old token will stop working immediately. Update your identity provider with the new token.", confirm: "Replace token", confirm_event: "scim-rotate", cancel_event: "scim-cancel-rotate", ..Default::default()
                    })?);
                }
                if self.revoke_scim {
                    parts.push(render(&bilibi_ui::ConfirmDialog {
                        id: "scim-revoke", consequence: "Revoke SCIM token?", detail: "Automatic provisioning will stop immediately. Existing people and their access will be kept.", confirm: "Revoke token", confirm_event: "scim-revoke", cancel_event: "scim-cancel-revoke", ..Default::default()
                    })?);
                }
            } else {
                let presets: Vec<Value> =
                    serde_json::from_str(PRESETS).map_err(|_| Error::Internal)?;
                let choices = presets
                    .iter()
                    .map(|p| {
                        Choice::new(
                            p["name"].as_str().unwrap_or(""),
                            p["id"].as_str().unwrap_or(""),
                        )
                    })
                    .collect();
                parts.push(render(&bilibi_ui::Form {
                    id: "sso-presets",
                    event: "preset",
                    body: render(
                        &Field::named("preset")
                            .kind(InputKind::Select)
                            .label("Starting configuration")
                            .options(choices),
                    )?,
                    actions: button("Use preset", "")?,
                    ..Default::default()
                })?);
                let c = &self.config;
                let client = self.users.db.get().await?;
                let roles = bilibi_authz::roles::list(
                    &**client,
                    scope,
                    &[company],
                    &bilibi_authz::roles::Query {
                        search: String::new(),
                        sort: bilibi_authz::roles::Sort::Name,
                        descending: false,
                        page: 1,
                        page_size: 300,
                    },
                )
                .await?;
                drop(client);
                let role_choices = std::iter::once(Choice::new("No default role", ""))
                    .chain(
                        roles
                            .entries
                            .iter()
                            .map(|role| Choice::new(&role.name, role.id.to_string())),
                    )
                    .collect();
                let accounts = self.users.list_company_accounts(scope, company).await?;
                let account_choices = std::iter::once(Choice::new("Choose an administrator", ""))
                    .chain(accounts.iter().map(|account| {
                        Choice::new(
                            format!("{} ({})", account.name, account.email),
                            account.id.to_string(),
                        )
                    }))
                    .collect();
                let mut fields = vec![
                    self.field("label", "Connection name", &c.label)?,
                    check("enabled", "Enable company sign-in", c.enabled)?,
                    self.field("method", "Sign-in method", &c.method)?,
                    self.field(
                        "issuer",
                        "Issuer address",
                        c.protocol["issuer"].as_str().unwrap_or(""),
                    )?,
                    self.field(
                        "client_id",
                        "Client ID",
                        c.protocol["client_id"].as_str().unwrap_or(""),
                    )?,
                    self.field(
                        "scopes",
                        "Requested scopes (space separated)",
                        &c.protocol["scopes"]
                            .as_array()
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .unwrap_or_else(|| "openid email profile".into()),
                    )?,
                    render(
                        &Field::named("token_auth")
                            .kind(InputKind::Select)
                            .label("Client authentication")
                            .options(vec![
                                Choice::new("Automatic", "auto"),
                                Choice::new(
                                    "Client secret in authorization header",
                                    "client_secret_basic",
                                ),
                                Choice::new("Client secret in request body", "client_secret_post"),
                                Choice::new("Public client (no secret)", "none"),
                            ])
                            .value(c.protocol["token_auth"].as_str().unwrap_or("auto"))
                            .errors(self.errors.get("token_auth").cloned().into_iter().collect()),
                    )?,
                    render(
                        &SecretInput::new("client_secret", "client_secret", "Client secret")
                            .label("Client secret")
                            .hint("Leave blank to keep the saved secret."),
                    )?,
                    self.field("email_claim", "Email claim", &c.email_claim)?,
                    self.field(
                        "email_verified_claim",
                        "Verified email claim",
                        &c.email_verified_claim,
                    )?,
                    self.field("name_claim", "Name claim", &c.name_claim)?,
                    self.field("groups_claim", "Groups or roles claim", &c.groups_claim)?,
                    check(
                        "just_in_time",
                        "Create accounts on first sign-in",
                        c.just_in_time,
                    )?,
                    self.field(
                        "allowed_email_domains",
                        "Allowed email domains for new accounts (comma separated)",
                        &c.allowed_email_domains.join(","),
                    )?,
                    render(
                        &Field::named("default_role")
                            .kind(InputKind::Select)
                            .label("Default role")
                            .options(role_choices)
                            .value(c.default_role.map(|v| v.to_string()).unwrap_or_default()),
                    )?,
                    render(
                        &Field::named("role_mapping")
                            .label("Group to role IDs (JSON)")
                            .errors(
                                self.errors
                                    .get("role_mapping")
                                    .cloned()
                                    .into_iter()
                                    .collect(),
                            )
                            .kind(InputKind::Textarea)
                            .value(
                                self.raw_mapping.clone().unwrap_or(
                                    serde_json::to_string_pretty(&c.role_mapping)
                                        .map_err(|_| Error::Internal)?,
                                ),
                            ),
                    )?,
                    check(
                        "enforce_sso",
                        "Require company sign-in for linked accounts",
                        c.enforce_sso,
                    )?,
                    render(
                        &Field::named("break_glass_user")
                            .kind(InputKind::Select)
                            .label("Emergency administrator")
                            .options(account_choices)
                            .value(
                                c.break_glass_user
                                    .map(|v| v.to_string())
                                    .unwrap_or_default(),
                            ),
                    )?,
                    self.field(
                        "mfa_amr",
                        "2FA authentication methods (comma separated)",
                        &c.mfa_amr.join(","),
                    )?,
                    self.field(
                        "mfa_acr",
                        "2FA assurance levels (comma separated)",
                        &c.mfa_acr.join(","),
                    )?,
                    self.field(
                        "step_up_acr",
                        "Assurance level for confirming access",
                        c.step_up_acr.as_deref().unwrap_or(""),
                    )?,
                    check(
                        "provider_logout",
                        "Also sign out of the provider",
                        c.provider_logout,
                    )?,
                    self.field(
                        "api_audience",
                        "API access-token audience (blank disables access)",
                        &c.api_audience,
                    )?,
                    self.field(
                        "mcp_audience",
                        "MCP access-token audience (blank disables access)",
                        &c.mcp_audience,
                    )?,
                    self.field(
                        "access_token_scopes",
                        "Required access-token scopes (space separated)",
                        &c.access_token_scopes.join(" "),
                    )?,
                ];
                if c.method != "oidc" {
                    fields.push(render(
                        &Field::named("protocol")
                            .label("Method configuration (JSON)")
                            .kind(InputKind::Textarea)
                            .value(c.protocol.to_string()),
                    )?);
                }
                parts.push(render(&bilibi_ui::Form {
                    id: "sso-configuration",
                    event: "configure",
                    body: render(&Fragments { fields })?,
                    actions: render(&Button {
                        button_type: "submit",
                        variant: ButtonVariant::Primary,
                        body: Html::text("Save"),
                        ..Default::default()
                    })?,
                    ..Default::default()
                })?);
            }
            if self.id.is_some() {
                let accounts = self.users.list_company_accounts(scope, company).await?;
                let choices: Vec<Choice> = accounts
                    .iter()
                    .map(|a| Choice::new(format!("{} ({})", a.name, a.email), a.id.to_string()))
                    .collect();
                parts.push(render(&bilibi_ui::Form {
                    id: "sso-link",
                    event: "link",
                    body: render(&Fragments {
                        fields: vec![
                            render(
                                &Field::named("user")
                                    .id("sso-link-user")
                                    .kind(InputKind::Select)
                                    .label("Account")
                                    .options(choices.clone()),
                            )?,
                            self.field("subject", "Provider subject", "")?,
                        ],
                    })?,
                    actions: button("Link account", "")?,
                    ..Default::default()
                })?);
                parts.push(render(&bilibi_ui::Form {
                    id: "sso-unlink",
                    event: "ask-unlink",
                    body: render(
                        &Field::named("user")
                            .id("sso-unlink-user")
                            .kind(InputKind::Select)
                            .label("Account to unlink")
                            .options(choices),
                    )?,
                    actions: button("Unlink account", "")?,
                    ..Default::default()
                })?);
                if self.unlink.is_some() {
                    parts.push(render(&bilibi_ui::ConfirmDialog{id:"sso-unlink-confirm",consequence:"Unlink this account and sign it out?",detail:"This removes the provider's role grants. Roles assigned by an administrator remain.",confirm:"Unlink account",working:"Unlinking…",confirm_event:"unlink",cancel_event:"cancel-unlink"})?);
                }
            }
        } else {
            let connections = self.users.external_identity_connections(scope).await?;
            for (public, label) in connections {
                if self.confirm {
                    let client = self.users.db.get().await?;
                    let c = self
                        .users
                        .readable_connection_in(&**client, &public)
                        .await?;
                    drop(client);
                    if c.is_none_or(|c| c.config.step_up_acr.is_none()) {
                        continue;
                    }
                }
                let destination = format!(
                    "/login/sso/{public}/start?purpose={}&back={}",
                    if self.confirm { "step_up" } else { "link" },
                    percent_encoding::utf8_percent_encode(
                        &self.back,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                );
                parts.push(render(&Button {
                    href: &destination,
                    body: Html::text(&label),
                    ..Default::default()
                })?);
            }
            if parts.is_empty() {
                parts.push(Html::text("Your company has no sign-in connection. Ask your administrator to connect one."));
            }
            let client = self.users.db.get().await?;
            if bilibi_authz::decide(&**client, scope, config::MANAGE)
                .await?
                .allowed
            {
                if let Some(company) = scope.current_company() {
                    parts.push(render(&bilibi_ui::ActionLink {
                        href: &format!("/companies/{}/sso", company.get()),
                        title: "Manage company sign-in",
                        body: Html::text("Manage company sign-in"),
                        ..Default::default()
                    })?);
                }
            }
        }
        View {
            header: render(&Header {
                title: Html::text(if self.confirm {
                    "Confirm access"
                } else {
                    "Company sign-in"
                }),
                ..Default::default()
            })?,
            notice: self.notice.clone(),
            kind: if self.error {
                bilibi_ui::StatusKind::Error
            } else {
                bilibi_ui::StatusKind::Success
            },
            body: render(&Fragments { fields: parts })?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
    async fn event(&mut self, r: &LiveRequest<'_, Scope>, event: &Value) -> Result<Option<String>> {
        let company = self.company.ok_or(Error::Invalid)?;
        let client = self.users.db.get().await?;
        self.users.sso_company(&**client, r.scope, company).await?;
        drop(client);
        let get = |key: &str| event[key].as_str().unwrap_or("");
        self.scim_token = None;
        match get("type") {
            "scim-ask-rotate" => self.rotate_scim = true,
            "scim-cancel-rotate" => self.rotate_scim = false,
            "scim-rotate" => {
                if !self.rotate_scim
                    && self
                        .users
                        .scim_token_present(r.scope, company, self.id.ok_or(Error::Invalid)?)
                        .await?
                {
                    return Err(Error::Invalid);
                }
                self.scim_token = self
                    .users
                    .rotate_scim_token(r.scope, company, self.id.ok_or(Error::Invalid)?, false)
                    .await?;
                self.rotate_scim = false;
                self.notice = "SCIM token created. Copy and store it now.".into();
            }
            "scim-ask-revoke" => self.revoke_scim = true,
            "scim-cancel-revoke" => self.revoke_scim = false,
            "scim-revoke" => {
                if !self.revoke_scim {
                    return Err(Error::Invalid);
                }
                self.users
                    .rotate_scim_token(r.scope, company, self.id.ok_or(Error::Invalid)?, true)
                    .await?;
                self.revoke_scim = false;
                self.notice = "SCIM token revoked. Automatic provisioning is off.".into();
            }
            "edit" => self.editing = true,
            "select" => {
                let id = get("connection");
                let path = if id.is_empty() {
                    format!("/companies/{company}/sso")
                } else {
                    let id: i64 = id.parse().map_err(|_| Error::Invalid)?;
                    format!("/companies/{company}/sso?connection={id}")
                };
                return Ok(Some(path));
            }
            "preset" => {
                let presets: Vec<Value> =
                    serde_json::from_str(PRESETS).map_err(|_| Error::Internal)?;
                let preset = presets
                    .iter()
                    .find(|p| p["id"].as_str() == Some(get("preset")))
                    .ok_or(Error::Invalid)?;
                self.config = IdentityConfiguration::default();
                self.config.label = preset["name"].as_str().unwrap_or("").into();
                self.config.protocol = json!({"issuer":preset["issuer"],"client_id":""});
                self.config.groups_claim = preset["groups_claim"].as_str().unwrap_or("").into();
            }
            "configure" => {
                self.errors.clear();
                self.raw_mapping = Some(get("role_mapping").into());
                let mut c = self.config.clone();
                c.label = get("label").into();
                c.method = get("method").into();
                c.enabled = get("enabled") == "true";
                c.protocol = if c.method == "oidc" {
                    let mut protocol = json!({"issuer":get("issuer"),"client_id":get("client_id")});
                    let scopes = get("scopes").split_whitespace().collect::<Vec<_>>();
                    if scopes != ["openid", "email", "profile"] {
                        protocol["scopes"] = json!(scopes);
                    }
                    if !get("token_auth").is_empty() && get("token_auth") != "auto" {
                        protocol["token_auth"] = json!(get("token_auth"));
                    }
                    protocol
                } else {
                    serde_json::from_str(get("protocol")).map_err(|_| Error::Invalid)?
                };
                c.email_claim = get("email_claim").into();
                c.name_claim = get("name_claim").into();
                c.email_verified_claim = get("email_verified_claim").into();
                c.groups_claim = get("groups_claim").into();
                c.just_in_time = get("just_in_time") == "true";
                c.allowed_email_domains = get("allowed_email_domains")
                    .split(',')
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                c.default_role = if get("default_role").is_empty() {
                    None
                } else {
                    Some(get("default_role").parse().map_err(|_| Error::Invalid)?)
                };
                c.role_mapping = match serde_json::from_str(get("role_mapping")) {
                    Ok(value) => value,
                    Err(_) => {
                        self.errors.insert(
                            "role_mapping".into(),
                            "Role mapping must be a JSON object of group names and role ID arrays."
                                .into(),
                        );
                        Default::default()
                    }
                };
                c.enforce_sso = get("enforce_sso") == "true";
                c.break_glass_user = if get("break_glass_user").is_empty() {
                    None
                } else {
                    Some(
                        get("break_glass_user")
                            .parse()
                            .map_err(|_| Error::Invalid)?,
                    )
                };
                c.mfa_amr = get("mfa_amr")
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                c.mfa_acr = get("mfa_acr")
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                c.step_up_acr = (!get("step_up_acr").is_empty()).then(|| get("step_up_acr").into());
                c.provider_logout = get("provider_logout") == "true";
                c.api_audience = get("api_audience").into();
                c.mcp_audience = get("mcp_audience").into();
                c.access_token_scopes = get("access_token_scopes")
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                self.config = c.clone();
                self.errors.extend(
                    c.problems()
                        .into_iter()
                        .map(|(field, message)| (field.into(), message.into())),
                );
                if !self.errors.is_empty() {
                    return Err(Error::Invalid);
                }
                self.raw_mapping = None;
                let mut input = json!({"company_id":company,"configuration":serde_json::to_string(&c).map_err(|_|Error::Invalid)?});
                if let Some(id) = self.id {
                    input["id"] = json!(id);
                }
                if !get("client_secret").is_empty() {
                    input["client_secret"] = json!(get("client_secret"));
                }
                let result = match self
                    .contracts
                    .submit(
                        &bilibi_agent_api::page::Call {
                            scope: r.scope,
                            credential: r.credential,
                            service: SERVICE,
                            event: "configure",
                            contract: config::CONFIGURE,
                        },
                        input,
                    )
                    .await
                {
                    Ok(result) => result,
                    Err(bilibi_agent_api::Error::InvalidInput(mut fields)) => {
                        self.refused = fields
                            .remove("configuration")
                            .and_then(|messages| messages.into_iter().next());
                        return Err(Error::Invalid);
                    }
                    Err(bilibi_agent_api::Error::Backend(error)) => return Err(error),
                    Err(_) => return Err(Error::Invalid),
                };
                let id = result["result"]["id"].as_i64().ok_or(Error::Internal)?;
                return Ok(Some(format!("/companies/{company}/sso?connection={id}")));
            }
            "link" => {
                let user: i64 = get("user").parse().map_err(|_| Error::Invalid)?;
                self.contracts.submit(&bilibi_agent_api::page::Call{scope:r.scope,credential:r.credential,service:SERVICE,event:"link",contract:agent::LINK},json!({"provider":self.public,"user_id":user,"issuer":self.config.protocol["issuer"],"subject":get("subject")})).await.map_err(agent_error)?;
                self.notice = "Account linked.".into();
            }
            "ask-unlink" => {
                self.unlink = Some(get("user").parse().map_err(|_| Error::Invalid)?);
            }
            "cancel-unlink" => {
                self.unlink = None;
            }
            "unlink" => {
                let user = self.unlink.take().ok_or(Error::Invalid)?;
                self.contracts
                    .submit(
                        &bilibi_agent_api::page::Call {
                            scope: r.scope,
                            credential: r.credential,
                            service: SERVICE,
                            event: "unlink",
                            contract: agent::UNLINK,
                        },
                        json!({"provider":self.public,"user_id":user}),
                    )
                    .await
                    .map_err(agent_error)?;
                self.notice = "Account unlinked and signed out.".into();
            }
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut redirect = None;
            if let LiveInput::Event(event) = r.input {
                match self.event(&r, event).await {
                    Ok(path) => {
                        redirect = path;
                        self.error = false
                    }
                    Err(e @ Error::StepUpRequired { .. }) => return Err(e),
                    Err(Error::Invalid | Error::Conflict | Error::NotFound) => {
                        self.notice = self.refused.take().unwrap_or_else(|| "Could not save company sign-in. Check the fields, permissions and emergency administrator.".into());
                        self.error = true;
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(LiveBody {
                html: self.view(r.scope).await?,
                redirect,
                query: self
                    .id
                    .map(|id| {
                        std::collections::HashMap::from([("connection".into(), id.to_string())])
                    })
                    .unwrap_or_default(),
                ..Default::default()
            })
        })
    }
}
macro_rules! route{($id:literal,$path:literal,$op:literal,$access:expr)=>{inventory::submit!{Contribution::new(ROUTES,$id,Provenance{module:"core/user",source:file!(),line:line!()},&Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,method:Method::Get,path:$path,service:SERVICE,operation:$op,access:$access,framed:true})}}}
route!(
    "core/user.sso.manage",
    "/companies/{id}/sso",
    "manage",
    Access::Capability(config::MANAGE)
);
route!(
    "core/user.sso.settings",
    "/settings/sso",
    "settings",
    Access::SignedIn
);
route!(
    "core/user.sso.confirm",
    "/settings/sso/confirm",
    "confirm",
    Access::SignedIn
);
inventory::submit! {Contribution::new(PAGE_WRITES,SERVICE,Provenance{module:"core/user",source:file!(),line:line!()},&PageWrites{service:SERVICE,contracts:&[("configure",config::CONFIGURE),("link",agent::LINK),("unlink",agent::UNLINK)],pending:Pending::Events { events: &["scim-rotate", "scim-revoke"], reason: "SCIM credentials are browser-only identity infrastructure; token lifecycle calls the User facade without exposing credentials to agent contracts." }})}

fn agent_error(error: bilibi_agent_api::Error) -> Error {
    match error {
        bilibi_agent_api::Error::Backend(e) => e,
        bilibi_agent_api::Error::Forbidden => Error::Forbidden,
        _ => Error::Invalid,
    }
}
