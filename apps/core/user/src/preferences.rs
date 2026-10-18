//! The signed-in account's appearance (theme, time display, regional locale)
//! and sidebar pins (Bilimbi `Core.User.DisplayPreferences`, `Pins`, `Pin`).
//!
//! Every call takes the sealed `Scope` of the session, so a request can never
//! name another account. Theme, time-display mode and locale are Settings
//! values at the account's own scope (`ui.theme`, `ui.timezone.mode`,
//! `ui.locale`); pins are rows of `user_pins` keyed by the current tenant and
//! account IDs. Neither touches account membership.
use crate::UserService;
use bilibi_database::{Change, Error, Result};
use bilibi_datetime::Mode;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_security::Scope;
use bilibi_settings::{Definition, ScopeKind, Setting, Target, Validator, ValueType, DEFINITIONS};
use serde::Serialize;
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

pub const THEME_KEY: &str = "ui.theme";
pub const THEMES: [&str; 3] = ["light", "dark", "system"];
/// Pins per account: storage stays bounded by the account count.
pub const MAX_PINS: i64 = 100;

fn theme_accepts(value: &Value) -> bool {
    value.as_str().is_some_and(|theme| THEMES.contains(&theme))
}
// `system` is the absence of an override, so the stored values are the two
// explicit choices; an editor is not contributed (the appearance page owns it).
inventory::submit! {
    Contribution::new(DEFINITIONS, THEME_KEY,
        Provenance { module: "core/user", source: file!(), line: line!() },
        &Setting::Definition(Definition {
            value_type: ValueType::String,
            scopes: &[ScopeKind::User],
            default: || json!("system"),
            nullable: false, encrypted: false, minimum: None, maximum: None,
            validator: Some(Validator { accepts: theme_accepts, message: "must be light, dark or system" }),
            editor: None,
        }))
}

/// Why an appearance or pin write did not happen, beyond infrastructure
/// failures: a page maps `Invalid` to "Choose a supported value.".
#[derive(Debug, thiserror::Error)]
pub enum PreferenceError {
    #[error("unsupported value")]
    Invalid,
    /// The session is an operator viewing as this account: its appearance
    /// and pins belong to the account, so they are read but never written.
    #[error("impersonating")]
    Impersonating,
    #[error(transparent)]
    Other(#[from] Error),
}
impl From<tokio_postgres::Error> for PreferenceError {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::from(error).into()
    }
}

/// One resolved snapshot for every surface that shows the preferences: the
/// shell's top-bar controls, the appearance page and the preference endpoint.
#[derive(Clone, Debug)]
pub struct Presentation {
    pub theme: &'static str,
    pub mode: Mode,
    pub timezone: String,
    pub locale: &'static str,
    pub language: &'static str,
    /// The account's own locale choice; empty follows the installation.
    pub own_locale: &'static str,
}

/// The account's resolved theme, time display and locale. The locale is
/// resolved once and handed to the display policy.
pub async fn presentation(
    service: &UserService,
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<Presentation> {
    let own = Target::User { company: None };
    let theme = service.settings.get(client, scope, own, THEME_KEY).await?;
    let theme = match theme.value.as_str() {
        Some("light") if theme.overridden => "light",
        Some("dark") if theme.overridden => "dark",
        _ => "system",
    };
    let locale = service.locales.resolve(client, scope, None).await?;
    let own_locale = service
        .settings
        .get(client, scope, own, bilibi_locale::LOCALE_KEY)
        .await?;
    let own_locale = if own_locale.overridden {
        own_locale
            .value
            .as_str()
            .and_then(bilibi_locale::normalize)
            .unwrap_or("")
    } else {
        ""
    };
    let display = service
        .display
        .display(client, scope, None, locale.locale)
        .await?;
    Ok(Presentation {
        theme,
        mode: display.mode,
        timezone: display.timezone,
        locale: locale.locale,
        language: locale.language,
        own_locale,
    })
}

impl UserService {
    pub async fn presentation(&self, scope: &Scope) -> Result<Presentation> {
        let client = self.db.get().await?;
        presentation(self, &**client, scope).await
    }

    /// Stores one appearance preference of the signed-in account. `kind` is
    /// `theme`, `timezone` (the time-display mode) or `locale` (empty follows
    /// the installation). An unsupported kind or value is refused, never
    /// guessed.
    pub async fn save_preference(
        &self,
        token: &str,
        csrf: &str,
        kind: &str,
        value: &str,
    ) -> std::result::Result<(), PreferenceError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        if scope.impersonator().is_some() {
            return Err(PreferenceError::Impersonating);
        }
        let own = Target::User { company: None };
        match kind {
            "theme" if THEMES.contains(&value) => {
                let mut tx = self
                    .mutations
                    .begin_view_state(&mut client, scope.mutation_context("user.preference.theme"))
                    .await?;
                if value == "system" {
                    self.settings
                        .delete(&mut tx, &scope, own, THEME_KEY)
                        .await?;
                } else {
                    self.settings
                        .put(&mut tx, &scope, own, THEME_KEY, json!(value))
                        .await?;
                }
                tx.commit().await?;
            }
            "timezone" => {
                let mut tx = self
                    .mutations
                    .begin_view_state(
                        &mut client,
                        scope.mutation_context("user.preference.timezone"),
                    )
                    .await?;
                match self.display.put_mode(&mut tx, &scope, value).await {
                    Ok(_) => {}
                    Err(Error::Invalid) => return Err(PreferenceError::Invalid),
                    Err(error) => return Err(error.into()),
                }
                tx.commit().await?;
            }
            "locale" if value.is_empty() || bilibi_locale::supports(value) => {
                let mut tx = self
                    .mutations
                    .begin_view_state(
                        &mut client,
                        scope.mutation_context("user.preference.locale"),
                    )
                    .await?;
                if value.is_empty() {
                    self.locales.delete_user(&mut tx, &scope).await?;
                } else {
                    self.locales.put_user(&mut tx, &scope, value).await?;
                }
                tx.commit().await?;
            }
            _ => return Err(PreferenceError::Invalid),
        }
        Ok(())
    }
}

// ---- Pins -----------------------------------------------------------------

/// One pinned page of the sidebar, in the shape the shell and `/api/pins`
/// exchange.
#[derive(Clone, Debug, Serialize)]
pub struct Pin {
    pub id: i64,
    pub label: String,
    pub url: String,
    pub icon: Option<String>,
    pub sort_order: i32,
}

/// The last non-empty `/` segment of a breadcrumb label.
pub fn normalize_label(label: &str) -> String {
    label
        .split('/')
        .map(str::trim)
        .rfind(|segment| !segment.is_empty())
        .unwrap_or_else(|| label.trim())
        .to_owned()
}

/// The canonical path plus the deterministically sorted query: a scheme and
/// host, the fragment, trailing slashes and parameter order never make a
/// second pin of the same page. `None` when the result would leave the site.
pub fn normalize_url(url: &str) -> Option<String> {
    let url = url.trim();
    let url = url.split('#').next().unwrap_or("");
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (url, None),
    };
    // `http://host/a` keeps only `/a`, as the source's URI parse does.
    let path = match path.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |at| &rest[at..]),
        None => path,
    };
    let path = path.trim();
    let mut path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    // `//host/x` is a link to another site, never a page of this one.
    if path.starts_with("//") {
        return None;
    }
    let sorted = query
        .and_then(|query| serde_urlencoded::from_str::<Vec<(String, String)>>(query).ok())
        .map(|pairs| {
            // A repeated key keeps its last value, as a decoded map does.
            let mut kept: std::collections::BTreeMap<String, String> = Default::default();
            kept.extend(pairs);
            serde_urlencoded::to_string(kept).unwrap_or_default()
        })
        .unwrap_or_default();
    Some(if sorted.is_empty() {
        path
    } else {
        format!("{path}?{sorted}")
    })
}

/// The account's pins in display order.
pub async fn list_pins(client: &impl GenericClient, scope: &Scope) -> Result<Vec<Pin>> {
    Ok(client
        .query(
            "SELECT id,label,url,icon,sort_order FROM user_pins WHERE tenant_id=$1 AND user_id=$2 ORDER BY sort_order,id",
            &[&scope.tenant(), &scope.user()],
        )
        .await?
        .iter()
        .map(|row| Pin {
            id: row.get(0),
            label: row.get(1),
            url: row.get(2),
            icon: row.get(3),
            sort_order: row.get(4),
        })
        .collect())
}

impl UserService {
    /// Pins the page when it is not pinned and unpins it when it is; returns
    /// whether it is now pinned, and the account's pins.
    pub async fn toggle_pin(
        &self,
        token: &str,
        csrf: &str,
        label: &str,
        url: &str,
        icon: Option<&str>,
    ) -> std::result::Result<(bool, Vec<Pin>), PreferenceError> {
        let label = normalize_label(label);
        let icon = icon.filter(|icon| !icon.is_empty()).map(str::to_owned);
        if url.len() > 500
            || label.is_empty()
            || label.chars().count() > 150
            || icon.as_ref().is_some_and(|icon| icon.chars().count() > 100)
        {
            return Err(PreferenceError::Invalid);
        }
        let url = normalize_url(url)
            .filter(|url| url.len() <= 500)
            .ok_or(PreferenceError::Invalid)?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        if scope.impersonator().is_some() {
            return Err(PreferenceError::Impersonating);
        }
        let mut tx = self
            .mutations
            .begin_view_state(&mut client, scope.mutation_context("user.pin.toggle"))
            .await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        // Serialize this account's pin writes: two toggles of one page must
        // not both insert, and the cap must hold under concurrency.
        tx.sql()
            .query_one(
                "SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR NO KEY UPDATE",
                &[&tenant, &user],
            )
            .await?;
        let pinned = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let existing = sql
                        .query_opt(
                            "DELETE FROM user_pins WHERE tenant_id=$1 AND user_id=$2 AND url_hash=md5($3) RETURNING id,label,url",
                            &[&tenant, &user, &url],
                        )
                        .await?;
                    if let Some(row) = existing {
                        let change = Change {
                            kind: "user_pin".into(),
                            id: row.get(0),
                            old: json!({"label": row.get::<_, String>(1), "url": row.get::<_, String>(2)}),
                            new: json!({}),
                        };
                        return Ok((false, vec![change]));
                    }
                    let held: i64 = sql
                        .query_one(
                            "SELECT count(*) FROM user_pins WHERE tenant_id=$1 AND user_id=$2",
                            &[&tenant, &user],
                        )
                        .await?
                        .get(0);
                    if held >= MAX_PINS {
                        return Err(Error::Invalid);
                    }
                    let row = sql
                        .query_one(
                            "INSERT INTO user_pins(tenant_id,user_id,label,url,url_hash,icon,sort_order) VALUES($1,$2,$3,$4,md5($4),$5,COALESCE((SELECT max(sort_order)+1 FROM user_pins WHERE tenant_id=$1 AND user_id=$2),0)) RETURNING id",
                            &[&tenant, &user, &label, &url, &icon],
                        )
                        .await?;
                    Ok((
                        true,
                        vec![Change {
                            kind: "user_pin".into(),
                            id: row.get(0),
                            old: json!({}),
                            new: json!({"label": label, "url": url}),
                        }],
                    ))
                })
            })
            .await
            .map_err(|error| match error {
                Error::Invalid => PreferenceError::Invalid,
                other => other.into(),
            })?;
        let pins = list_pins(tx.sql(), &scope).await?;
        tx.commit().await?;
        Ok((pinned, pins))
    }

    /// Renumbers the listed pins in the order given. The order is a display
    /// preference of the account's own, not a business fact, so it is a plain
    /// write rather than an audited mutation (pinning and unpinning stay
    /// audited). Ids that are not the account's are ignored.
    pub async fn reorder_pins(
        &self,
        token: &str,
        csrf: &str,
        ordered: &[i64],
    ) -> std::result::Result<Vec<Pin>, PreferenceError> {
        if ordered.len() as i64 > MAX_PINS {
            return Err(PreferenceError::Invalid);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        if scope.impersonator().is_some() {
            return Err(PreferenceError::Impersonating);
        }
        let tx = client.transaction().await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        tx.query_one(
            "SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR NO KEY UPDATE",
            &[&tenant, &user],
        )
        .await?;
        for (index, id) in ordered.iter().enumerate() {
            let index = index as i32;
            tx.execute(
                "UPDATE user_pins SET sort_order=$4,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
                &[&tenant, &user, id, &index],
            )
            .await?;
        }
        let pins = list_pins(tx.client(), &scope).await?;
        tx.commit().await?;
        Ok(pins)
    }
}
