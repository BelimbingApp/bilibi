//! The authenticated chrome around a discovered page (`bilibi_ui::AppShell`,
//! `Framed`) and the shell's own preference write. Web decides what the shell
//! shows from the sealed scope: the menu through the actor's effective
//! capabilities, operator standing through the explicit proof, the theme,
//! clock and pins through the account's own preferences (Core User). Base UI
//! only renders it.
use crate::{origin, token, App, HttpError};
use askama::Template;
use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bilibi_base::{authz, menu, tenancy};
use bilibi_database::{Error, Result};
use bilibi_module_registry::contributions::{
    Access, Embed, EmbedContext, Method, Route, Viewer, EMBEDS,
};
use bilibi_security::Scope;
use bilibi_ui::page::{PageRequest, PageService};
use bilibi_ui::{AppShell, DisplayPreferences, Framed, Html, NavNode, SearchPage, ShellContext};
use bilibi_user::{PreferenceError, Presentation};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, sync::Arc};

/// A rendered page body and the explicit theme to stamp on `<html>`.
pub struct Rendered {
    pub html: String,
    pub theme: &'static str,
}

/// Renders `body` inside the full shell, the chromeless framed root, or as is
/// for a public page outside a frame.
pub async fn render(
    app: &App,
    scope: Option<&Scope>,
    path: &str,
    framed: bool,
    live: bool,
    body: &str,
    topbar_actions: Html,
) -> Result<Rendered> {
    let presentation = match scope {
        Some(scope) => Some(app.user.presentation(scope).await?),
        None => None,
    };
    let preferences = presentation
        .as_ref()
        .map_or_else(DisplayPreferences::default, preferences);
    let theme = presentation.as_ref().map_or("system", |p| p.theme);
    let html = if framed {
        Framed {
            body,
            live,
            tenant_id: scope.map_or(0, Scope::tenant),
            user_id: scope.map_or(0, Scope::user),
            preferences: &preferences,
        }
        .render()
    } else if let Some(scope) = scope {
        let context = context(app, scope, path, preferences, topbar_actions).await?;
        AppShell {
            context: &context,
            live,
            body,
        }
        .render()
    } else {
        return Ok(Rendered {
            html: body.to_owned(),
            theme,
        });
    };
    Ok(Rendered {
        html: html.map_err(|_| Error::Internal)?,
        theme,
    })
}

/// The account's theme and time display: its own saved choices over the
/// company zone.
fn preferences(presentation: &Presentation) -> DisplayPreferences {
    DisplayPreferences {
        theme: presentation.theme.into(),
        mode: presentation.mode.as_str().into(),
        timezone: presentation.timezone.clone(),
    }
}

async fn context<'a>(
    app: &'a App,
    scope: &'a Scope,
    path: &str,
    preferences: DisplayPreferences,
    topbar_actions: Html,
) -> Result<ShellContext<'a>> {
    let client = app.db.get().await?;
    let instance = bilibi_base::system::Instance::resolve(&app.settings, &**client).await?;
    let capabilities = authz::effective_capabilities(&**client, scope).await?;
    // Owner-only items answer the live owner proof, never a capability.
    let owner = match tenancy::prove_owner(&**client, scope).await {
        Ok(_) => true,
        Err(Error::Forbidden) => false,
        Err(error) => return Err(error),
    };
    let tree = app.menu.visible_tree(&|key| {
        if key == menu::OWNER_STANDING {
            owner
        } else {
            capabilities.contains(key)
        }
    });
    let active = active_item(&tree, path);
    let nodes: Vec<_> = tree.iter().map(|node| nav_node(node, active)).collect();
    // The tenant's standing, from the explicit operator proof only.
    let platform_operator = match tenancy::prove_operator(&**client, scope).await {
        Ok(_) => true,
        Err(Error::Forbidden) => false,
        Err(error) => return Err(error),
    };
    // The company the account signs in under, through its affiliation
    // (Bilimbi `company_name_for`): display only, so a company the directory
    // cannot name never blocks an otherwise proven session.
    let company = match scope.actor().organisation() {
        Some(id) => app
            .services
            .get::<tenancy::PartyProofs>("base/tenancy.parties")?
            .organisation_name(&**client, scope, id)
            .await
            .ok()
            .flatten()
            .map(|organisation| organisation.name),
        None => None,
    };
    // Bilimbi warns only on the operator tenant, and only while it stands on
    // no primary company; a failed lookup paints no warning.
    let operator_company_missing = platform_operator
        && matches!(
            bilibi_core::company::platform_operator_company(&**client).await,
            Ok(None)
        );
    let pins = bilibi_user::list_pins(&**client, scope).await?;
    // Everything below takes connections of its own. Holding this one across
    // them would let as many simultaneous renders as the pool holds take every
    // connection and wait on each other for good (every live reply renders
    // the shell), so it goes back to the pool first.
    drop(client);
    if platform_operator {
        adopt_installation_locale(app, scope).await;
    }
    // A module contributes the bell through the `shell.notifications` embed,
    // the search dialog's records through `shell.search.records` and the
    // status bar's AI tag through `shell.ai`; an account an embed refuses has
    // none.
    let mut notifications = Html::default();
    let mut records = Html::default();
    let mut ai = Html::default();
    for slot in embeds(app)? {
        let rendered = match slot.embed.slot {
            "shell.notifications" => &mut notifications,
            "shell.search.records" => &mut records,
            "shell.ai" => &mut ai,
            _ => continue,
        };
        if permits(app, scope, slot.embed.access).await? {
            *rendered = render_embed(app, scope, &slot.embed).await?;
        }
    }
    // Display only and unlogged: the editor and its save authorize for real.
    let manage_instance = platform_operator
        && capabilities.contains(bilibi_base::system::INSTANCE_MANAGE)
        && app
            .services
            .get::<bilibi_base::system::Instances>(bilibi_base::system::SERVICE)?
            .is_core_administrator(scope)
            .await?;
    // The search lists the pages the person may open, decided by the shared
    // display check (`Access::admits`, as tiles and record hits are): display
    // only and unlogged, since each route checks its own access again. Two
    // pages the status bar links to under a condition use the same condition
    // here: Instance settings also
    // needs the Core Administrator standing, and the operator setup page only
    // helps while the operator stands on no primary company (afterwards it
    // sends everyone to that company).
    let viewer = Viewer {
        capabilities: &capabilities,
        operator: platform_operator,
        owner,
    };
    let may_open = |access: Access| match access.base() {
        // Sign-in and the other public pages are no place to send someone
        // already signed in.
        Access::Public => false,
        Access::OperatorCapability(bilibi_base::system::INSTANCE_MANAGE) => manage_instance,
        access => access.admits(&viewer),
    };
    let pages = search_pages(app, &tree, |route| {
        may_open(route.access) && (route.path != OPERATOR_SETUP || operator_company_missing)
    });
    let served = served_routes(app);
    let pins: Vec<_> = pins
        .into_iter()
        .filter(|pin| is_served(&served, &pin.url))
        .collect();
    let mut pin_icons: Vec<String> = pins
        .iter()
        .filter_map(|pin| pin.icon.clone())
        .filter(|icon| bilibi_ui::Icons::is_known(icon))
        .collect();
    pin_icons.sort();
    pin_icons.dedup();
    let home = if served.contains(&"/dashboard") {
        "/dashboard"
    } else {
        "/"
    };
    Ok(ShellContext {
        user: &scope.user_name,
        email: &scope.email,
        tenant: &scope.tenant_name,
        tenant_id: scope.tenant(),
        user_id: scope.user(),
        company,
        platform_operator,
        operator_company_missing,
        impersonator: scope.impersonator().map(|operator| operator.name()),
        csrf: &scope.csrf,
        preferences,
        navigation: bilibi_ui::navigation(&nodes, true).map_err(|_| Error::Internal)?,
        notifications,
        ai,
        home,
        served_routes: serde_json::to_string(&served).map_err(|_| Error::Internal)?,
        pins: serde_json::to_string(&pins).map_err(|_| Error::Internal)?,
        pin_icons,
        instance: if instance.role == bilibi_base::system::Role::Dev {
            Html::EMPTY
        } else {
            instance.badge().map_err(|_| Error::Internal)?
        },
        instance_status: instance
            .status(manage_instance)
            .map_err(|_| Error::Internal)?,
        instance_title: instance.label,
        listen_address: if instance.role == bilibi_base::system::Role::Dev {
            app.listen_address.as_deref()
        } else {
            None
        },
        versions: bilibi_ui::version_badges(&crate::composition()),
        topbar_actions,
        page_search: bilibi_ui::page_search(&pages, records).map_err(|_| Error::Internal)?,
    })
}

/// The address of Company's operator setup page, which the status bar links to
/// while the operator stands on no primary company (`shell.html`).
const OPERATOR_SETUP: &str = "/setup/platform-operator";

/// The pages the shell's search lists: each page route the person `may` open,
/// named by its title, else its menu row, and placed where it lives: at its
/// menu row, else under the deepest row its address sits beneath. A personal
/// settings page takes its tab's label under `SETTINGS_LOCATION`. A route with
/// a path parameter and a page with no name are left out. A route that only
/// redirects is listed under its title when the person may also open where it
/// leads (Create Contact opens the Contacts list's dialog), so an old address
/// that redirects returns no title.
fn search_pages(app: &App, tree: &[menu::Node], may: impl Fn(&Route) -> bool) -> Vec<SearchPage> {
    let mut rows = Vec::new();
    menu_rows(tree, &[], &mut rows);
    let settings = bilibi_ui::SettingsTab::pages();
    let mut pages = Vec::new();
    let mut account = Vec::new();
    for bound in app.routes.iter() {
        let route = bound.route;
        if route.method != Method::Get || route.path.contains('{') || !may(route) {
            continue;
        }
        if let Some(target) = bound.page.0.redirect(route.operation) {
            let path = target.split('?').next().unwrap_or(target);
            match crate::routes::resolve(app, path) {
                Ok((to, _)) if may(to.route) => {}
                _ => continue,
            }
        }
        if let Some(at) = settings.iter().position(|(_, href)| *href == route.path) {
            let label = settings[at].0;
            account.push((
                at,
                SearchPage {
                    label: label.to_owned(),
                    location: bilibi_ui::SETTINGS_LOCATION
                        .iter()
                        .chain([&label])
                        .map(|part| (*part).to_owned())
                        .collect(),
                    href: route.path.to_owned(),
                    account: true,
                },
            ));
            continue;
        }
        let row = rows.iter().find(|(path, _)| *path == route.path);
        let Some(label) = bound
            .page
            .0
            .title(route.operation)
            .or(row.and_then(|(_, labels)| labels.last().copied()))
        else {
            continue;
        };
        let location: Vec<&str> = match row {
            Some((_, labels)) => labels.clone(),
            None => rows
                .iter()
                .filter(|(path, _)| {
                    *path != "/"
                        && route
                            .path
                            .strip_prefix(path)
                            .is_some_and(|rest| rest.starts_with('/'))
                })
                .max_by_key(|(path, _)| path.len())
                .map(|(_, labels)| labels.iter().copied().chain([label]).collect())
                .unwrap_or_default(),
        };
        pages.push(SearchPage {
            label: label.to_owned(),
            location: location.into_iter().map(str::to_owned).collect(),
            href: route.path.to_owned(),
            account: false,
        });
    }
    // Sections read alphabetically, as the menu does; settings in tab order.
    pages.sort_by_cached_key(|page| {
        if page.location.is_empty() {
            page.label.to_lowercase()
        } else {
            page.path().to_lowercase()
        }
    });
    account.sort_by_key(|(at, _)| *at);
    pages.extend(account.into_iter().map(|(_, page)| page));
    pages
}

/// Each visible menu row that opens a page, with the labels from its section
/// down to it.
fn menu_rows(
    nodes: &[menu::Node],
    above: &[&'static str],
    rows: &mut Vec<(&'static str, Vec<&'static str>)>,
) {
    for node in nodes {
        let item = node.item.item;
        let mut labels = above.to_vec();
        labels.push(item.label);
        if let Some(route) = item.route {
            rows.push((route, labels.clone()));
        }
        menu_rows(&node.children, &labels, rows);
    }
}

/// One `EmbedContext::Shell` contribution and the id of the element it
/// renders: slot `shell.notifications` is `#app-notifications`,
/// `shell.search.records` is `#app-search-records` and `shell.ai` is
/// `#app-ai`. The live socket mounts it as a panel under that id, outside
/// framed pages.
pub struct ShellEmbed {
    pub id: String,
    pub embed_id: &'static str,
    pub embed: Embed,
}

pub fn embeds(app: &App) -> Result<Vec<ShellEmbed>> {
    let mut found = Vec::new();
    for entry in app.services.catalog()?.entries::<Embed>(EMBEDS)? {
        let embed = *entry.get::<Embed>()?;
        if !matches!(embed.context, EmbedContext::Shell) {
            continue;
        }
        let name = embed.slot.strip_prefix("shell.").ok_or(Error::Internal)?;
        found.push(ShellEmbed {
            id: format!("app-{}", name.replace('.', "-")),
            embed_id: entry.id,
            embed,
        });
    }
    Ok(found)
}

/// The embed's access for an already proven scope: a refusal is `false`.
async fn permits(app: &App, scope: &Scope, access: Access) -> Result<bool> {
    let client = app.db.get().await?;
    let client = &**client;
    let result = match access.base() {
        Access::Public | Access::SignedIn | Access::StepUp { .. } => Ok(()),
        Access::Capability(key) => authz::authorize_key(client, scope, key).await,
        Access::Operator => tenancy::prove_operator(client, scope).await.map(|_| ()),
        Access::AnyOf(keys) => authz::authorize_any(client, scope, keys).await,
        Access::Owner => tenancy::prove_owner(client, scope).await.map(|_| ()),
        Access::OperatorCapability(key) => authz::authorize_operator(client, scope, key)
            .await
            .map(|_| ()),
    };
    match result {
        Ok(()) => Ok(true),
        Err(Error::Forbidden) => Ok(false),
        Err(error) => Err(error),
    }
}

async fn render_embed(app: &App, scope: &Scope, embed: &Embed) -> Result<Html> {
    let service: Arc<PageService<Scope, Error>> = app.services.get(embed.service)?;
    let empty = HashMap::new();
    let body = service
        .0
        .render(PageRequest {
            operation: embed.operation,
            scope: Some(scope),
            path: &empty,
            query: &empty,
            body: &[],
            framed: false,
        })
        .await?;
    // The owner rendered and escaped it, as for a page body.
    Ok(Html::raw(body.html))
}

/// The page's own menu row: an exact route, else the deepest route this path
/// sits under (a company's detail page keeps Companies current).
fn active_item(tree: &[menu::Node], path: &str) -> Option<&'static str> {
    fn walk(nodes: &[menu::Node], path: &str, best: &mut Option<(&'static str, usize)>) {
        for node in nodes {
            let item = node.item.item;
            if let Some(route) = item.route {
                let under = route == path
                    || route == "/"
                    || path
                        .strip_prefix(route)
                        .is_some_and(|rest| rest.starts_with('/'));
                if under && best.is_none_or(|(_, len)| route.len() > len) {
                    *best = Some((item.id, route.len()));
                }
            }
            walk(&node.children, path, best);
        }
    }
    let mut best = None;
    walk(tree, path, &mut best);
    best.map(|(id, _)| id)
}

fn nav_node(node: &menu::Node, active: Option<&str>) -> NavNode {
    let item = node.item.item;
    // Bilimbi ShellComponents uses the contributed icon name unchanged.
    let icon = item.icon.unwrap_or("square-3-stack-3d").to_owned();
    NavNode {
        id: item.id.to_owned(),
        label: item.label.to_owned(),
        icon,
        route: item.route.map(str::to_owned),
        active: active == Some(item.id),
        children: node
            .children
            .iter()
            .map(|child| nav_node(child, active))
            .collect(),
    }
}

/// Whether a normalized pin URL names a page this deployment serves; a route
/// pattern's `{param}` segments match any one segment. A pin to a page that is
/// gone stays stored but is not drawn.
fn is_served(routes: &[&str], url: &str) -> bool {
    let path = url.split('?').next().unwrap_or("");
    let actual: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
    routes.iter().any(|route| {
        let pattern: Vec<_> = route.split('/').filter(|s| !s.is_empty()).collect();
        pattern.len() == actual.len()
            && pattern
                .iter()
                .zip(&actual)
                .all(|(p, a)| p.starts_with('{') || p == a)
    })
}

/// The session behind a write of the account's own preferences: same Origin
/// and CSRF checks as a page write.
async fn writer(app: &App, headers: &HeaderMap) -> std::result::Result<(String, Scope), HttpError> {
    origin(app, headers)?;
    let credential = token(headers)?;
    let scope = app.security.guard(&credential).await?;
    if headers.get("x-csrf-token").and_then(|h| h.to_str().ok()) != Some(scope.csrf.as_str()) {
        return Err(Error::Forbidden.into());
    }
    Ok((credential, scope))
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// A refused value is a 422 with a stable code; every other failure keeps the
/// host's own mapping.
fn refusal(error: PreferenceError, code: &str) -> std::result::Result<Response, HttpError> {
    match error {
        PreferenceError::Invalid => Ok(json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": code}),
        )),
        // Bilimbi's answer to a borrowed session: the shell keeps the account's
        // own choices and says why nothing changed.
        PreferenceError::Impersonating => Ok(json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "impersonating"}),
        )),
        PreferenceError::Other(error) => Err(error.into()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    kind: String,
    value: String,
}

/// `POST /preferences`: the shell's theme and time-display choosers. The
/// account can only change its own; replies with the saved presentation.
pub async fn save(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, HttpError> {
    let (credential, scope) = writer(&app, &headers).await?;
    let preference: Preference = serde_json::from_slice(&body).map_err(|_| Error::Invalid)?;
    if !["theme", "timezone", "locale"].contains(&preference.kind.as_str()) {
        return Err(Error::Invalid.into());
    }
    if let Err(error) = app
        .user
        .save_preference(
            &credential,
            &scope.csrf,
            &preference.kind,
            &preference.value,
        )
        .await
    {
        return refusal(error, "invalid_preference");
    }
    let saved = app.user.presentation(&scope).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"theme": saved.theme, "mode": saved.mode.as_str(), "timezone": saved.timezone, "locale": saved.locale}),
    ))
}

/// The account's pins for a client that must refresh: the shell renders the
/// same list into `data-pins`.
pub async fn pins(
    State(app): State<App>,
    headers: HeaderMap,
) -> std::result::Result<Response, HttpError> {
    let scope = app.security.guard(&token(&headers)?).await?;
    let client = app.db.get().await.map_err(Error::from)?;
    let served = served_routes(&app);
    let pins: Vec<_> = bilibi_user::list_pins(&**client, &scope)
        .await?
        .into_iter()
        .filter(|pin| is_served(&served, &pin.url))
        .collect();
    Ok(json_response(StatusCode::OK, json!({"pins": pins})))
}

fn served_routes(app: &App) -> Vec<&'static str> {
    app.routes
        .iter()
        .filter(|bound| bound.route.method == Method::Get)
        .map(|bound| bound.route.path)
        .collect()
}

#[derive(Deserialize)]
struct Toggle {
    label: String,
    url: String,
    #[serde(default)]
    icon: Option<String>,
}

/// `POST /api/pins/toggle`: pins the page, or unpins it when it is pinned.
pub async fn toggle_pin(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, HttpError> {
    let (credential, scope) = writer(&app, &headers).await?;
    let Ok(toggle) = serde_json::from_slice::<Toggle>(&body) else {
        return Ok(json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "invalid_parameters"}),
        ));
    };
    match app
        .user
        .toggle_pin(
            &credential,
            &scope.csrf,
            &toggle.label,
            &toggle.url,
            toggle.icon.as_deref(),
        )
        .await
    {
        Ok((pinned, pins)) => {
            let served = served_routes(&app);
            let pins: Vec<_> = pins
                .into_iter()
                .filter(|pin| is_served(&served, &pin.url))
                .collect();
            Ok(json_response(
                StatusCode::OK,
                json!({"pinned": pinned, "pins": pins}),
            ))
        }
        Err(error) => refusal(error, "invalid_pin"),
    }
}

/// A pin id from the client: a positive integer or its decimal string, bare or
/// as `{"id": ..}`. Any other entry rejects the whole request, because a
/// partial reorder would leave the sidebar disagreeing with the server.
fn pin_id(entry: &serde_json::Value) -> Option<i64> {
    match entry {
        serde_json::Value::Object(map) => {
            map.get("id").filter(|id| !id.is_object()).and_then(pin_id)
        }
        serde_json::Value::Number(number) => number.as_i64().filter(|id| *id > 0),
        serde_json::Value::String(text) => text.parse::<i64>().ok().filter(|id| *id > 0),
        _ => None,
    }
}

/// `POST /api/pins/reorder`: `{"pins": [{"id": 3}, {"id": 1}]}` in the order
/// the sidebar now shows them.
pub async fn reorder_pins(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, HttpError> {
    let (credential, scope) = writer(&app, &headers).await?;
    let ids = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| match value.get("pins") {
            Some(serde_json::Value::Array(entries)) => {
                entries.iter().map(pin_id).collect::<Option<Vec<_>>>()
            }
            _ => None,
        });
    let Some(ids) = ids else {
        return Ok(json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "invalid_parameters"}),
        ));
    };
    match app.user.reorder_pins(&credential, &scope.csrf, &ids).await {
        Ok(pins) => {
            let served = served_routes(&app);
            let pins: Vec<_> = pins
                .into_iter()
                .filter(|pin| is_served(&served, &pin.url))
                .collect();
            Ok(json_response(StatusCode::OK, json!({"pins": pins})))
        }
        Err(error) => refusal(error, "invalid_parameters"),
    }
}

/// Once an installation locale is stored it stands; until then it is inferred
/// from the operator's primary company address on every resolve.
static LOCALE_ADOPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bilimbi persists the inferred installation locale the first time a
/// signed-in request resolves it from the operator's address facts. Here the
/// write needs operator proof, so the first operator page that finds facts
/// and no stored locale adopts them (`Locales::adopt_inferred`), attributed
/// to that operator. Display path: any refusal or failure leaves the locale
/// inferred and is tried again on a later page.
async fn adopt_installation_locale(app: &App, scope: &Scope) {
    use std::sync::atomic::Ordering;
    if LOCALE_ADOPTED.load(Ordering::Relaxed) {
        return;
    }
    let adopt = async {
        let locales = app
            .services
            .get::<bilibi_base::locale::Locales>("base/locale.locales")?;
        let persisted = {
            let client = app.db.get().await?;
            locales
                .resolve_global(&**client, scope, None)
                .await?
                .persisted
        };
        if persisted {
            return Ok::<bool, Error>(true);
        }
        // Address takes a connection of its own: hold none across it.
        let Some(facts) = app
            .services
            .get::<bilibi_core::address::AddressService>(bilibi_core::address::SERVICE)?
            .platform_operator_bootstrap()
            .await?
        else {
            return Ok(false);
        };
        let mut client = app.db.get().await?;
        let mut tx = app
            .services
            .get::<bilibi_database::MutationBoundary>("base/database.mutations")?
            .begin(&mut client, scope.mutation_context("locale.adopt"))
            .await?;
        let adopted = locales.adopt_inferred(&mut tx, scope, &facts).await?;
        tx.commit().await?;
        Ok(adopted.is_some())
    };
    if let Ok(true) = adopt.await {
        LOCALE_ADOPTED.store(true, Ordering::Relaxed);
    }
}
