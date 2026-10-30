//! Work calendars (issue 135, ADR 13): one answer, for every module, to "is
//! this business time?" and "when is N business hours from now?". Core master
//! data beside Company and Address, so Domains share one calendar. No Bilimbi
//! counterpart; the day types follow Belimbing's attendance calendar. Read
//! `README.md` before use.
//!
//! Calendars decide business time, never who works: shifts and rosters stay
//! in Domains. Day and arithmetic reads need only the sealed scope (the caller
//! authorizes its own operation); administration needs the module's
//! capabilities and is reached through the `core.work_calendar.*` contracts.
//! Installation-wide holiday sets (issue 210) are the platform operator's:
//! every tenant includes them, and the `publish` job tells each one of a
//! change.
mod agent_operations;
mod contributions;
mod model;
mod publish;
mod resolve;
mod store;
mod timetable;
mod web;

pub use model::{
    Calendar, CalendarEntry, Choice, Day, DayType, Exception, FieldError, Holiday, HolidaySet,
    HolidaySetEntry, Listing, Period, Reason, Rows, Rule, Substitute, Week, WeekDay, WriteError,
    WriteResult, MAX_CHOICES, MAX_PAGE_SIZE, MAX_ROWS,
};
pub use publish::{PUBLISH, PUBLISHER};
pub use timetable::{TimeError, Timetable, MAX_WINDOW_DAYS, SEARCH_DAYS};

use bilibi_authz::{
    authorize_key, CapabilityDefinition, RoleDefinition, StandingGrant, SystemPrincipalDefinition,
    VocabularyContribution, STANDING_GRANTS, VOCABULARY,
};
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_events::{EventType, Events, EVENTS};
use bilibi_geonames::Geonames;
use bilibi_module_registry::{Contribution, Service};
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{
    execution::{HandlerBinding, HandlerService, HANDLERS},
    Queue, Worker, WORKERS,
};
use bilibi_settings::{
    Definition, Editor, ScopeKind, Setting, Settings, Target, Validator, ValueType, DEFINITIONS,
};
use bilibi_tenancy::{Authentication, Scope};
use jiff::civil::Date;
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub const SERVICE: &str = "core/work_calendar.calendars";
pub const VIEW: &str = "admin.work-calendar.view";
pub const LIST: &str = "admin.work-calendar.list";
pub const MANAGE: &str = "admin.work-calendar.manage";
/// Keeping installation-wide holiday sets: a platform capability, held in the
/// platform operator's tenant beside [`MANAGE`].
pub const INSTALLATION_MANAGE: &str = "admin.work-calendar.holiday-sets.manage";
/// The tenant's or a company's default calendar code (null: none).
pub const DEFAULT: &str = "work_calendar.default";
/// Recorded once per calendar whose dates a write changed, with payload
/// `{"calendar": code, "from": date|null, "to": date|null}` (null: open-ended).
/// A module with open items (SLA due times) recomputes them itself.
pub const CHANGED: &str = "core/work_calendar.changed";

/// A calendar a record may be assigned now, locked against retirement until
/// the caller's transaction ends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assignable {
    pub code: String,
    pub name: String,
}

pub struct WorkCalendars {
    db: Db,
    authentication: Arc<Authentication>,
    settings: Arc<Settings>,
    geonames: Arc<Geonames>,
    events: Arc<Events>,
    queue: Arc<Queue>,
    boundary: Arc<MutationBoundary>,
}
impl WorkCalendars {
    /// Active calendars for a picker whose code or name contains `search`
    /// (empty: any), by name, at most [`MAX_CHOICES`].
    pub async fn choices(&self, scope: &Scope, search: &str) -> Result<Vec<Choice>> {
        let client = self.db.get().await?;
        store::choices(&**client, scope.tenant(), search).await
    }
    /// Prove `code` may be assigned to a record in the caller's transaction:
    /// an active calendar of the scope's tenant, locked `FOR SHARE` so it
    /// cannot be retired before the caller commits. A retired or unknown
    /// code is `Invalid`.
    pub async fn check(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: &str,
    ) -> Result<Assignable> {
        let row = client
            .query_opt(
                "SELECT code,name FROM core_work_calendars WHERE tenant_id=$1 AND code=$2 AND status='active' FOR SHARE",
                &[&scope.tenant(), &code],
            )
            .await?
            .ok_or(Error::Invalid)?;
        Ok(Assignable {
            code: row.get(0),
            name: row.get(1),
        })
    }
    /// The company's default calendar (`work_calendar.default`, company over
    /// tenant). `None` when unset, retired or missing: the facade never
    /// substitutes clock time; a caller that wants it uses `Timetable::clock`.
    pub async fn default_for(
        &self,
        scope: &Scope,
        company: OrganisationId,
    ) -> Result<Option<String>> {
        let client = self.db.get().await?;
        let value = self
            .settings
            .get(&**client, scope, Target::Company(company), DEFAULT)
            .await?
            .value;
        let Some(code) = value.as_str() else {
            return Ok(None);
        };
        Ok(client
            .query_opt(
                "SELECT code FROM core_work_calendars WHERE tenant_id=$1 AND code=$2 AND status='active'",
                &[&scope.tenant(), &code],
            )
            .await?
            .map(|row| row.get(0)))
    }
    /// One resolved date.
    pub async fn day(&self, scope: &Scope, code: &str, date: Date) -> Result<Day> {
        self.days(scope, code, date, date)
            .await?
            .pop()
            .ok_or(Error::Internal)
    }
    /// Each date from `from` to `to` (at most 731 days), resolved.
    pub async fn days(&self, scope: &Scope, code: &str, from: Date, to: Date) -> Result<Vec<Day>> {
        Ok(self.timetable(scope, code, from, to).await?.days().to_vec())
    }
    /// `Timetable::load` on a pooled connection.
    pub async fn timetable(
        &self,
        scope: &Scope,
        code: &str,
        from: Date,
        to: Date,
    ) -> Result<Timetable> {
        let client = self.db.get().await?;
        Timetable::load(&**client, scope, code, from, to).await
    }

    /// One page of the tenant's calendars by code, with the total. Needs
    /// `admin.work-calendar.list`; a page out of range is `Invalid`.
    pub async fn calendars(&self, scope: &Scope, listing: &Listing) -> Result<Rows<CalendarEntry>> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        authorize_key(&**client, &scope, LIST).await?;
        store::calendars(&**client, scope.tenant(), listing).await
    }
    /// One page of the tenant's holiday sets by code, with the total. Needs
    /// `admin.work-calendar.list`.
    pub async fn holiday_sets(
        &self,
        scope: &Scope,
        listing: &Listing,
    ) -> Result<Rows<HolidaySetEntry>> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        authorize_key(&**client, &scope, LIST).await?;
        store::holiday_sets(&**client, scope.tenant(), listing).await
    }
    /// The names of the holiday sets `codes` names (at most [`MAX_ROWS`]),
    /// by code, for the record page. Needs `admin.work-calendar.list`.
    pub(crate) async fn holiday_set_names(
        &self,
        scope: &Scope,
        codes: &[String],
    ) -> Result<Vec<(String, String)>> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        authorize_key(&**client, &scope, LIST).await?;
        store::holiday_set_names(&**client, scope.tenant(), codes).await
    }
    /// One calendar with its rule versions and holiday sets (the first
    /// [`MAX_ROWS`] of each) and page `page` of its exceptions, each with its
    /// total. Needs `admin.work-calendar.view`.
    pub async fn calendar(&self, scope: &Scope, code: &str, page: i64) -> Result<Calendar> {
        self.calendar_page(scope, code, page, MAX_ROWS).await
    }
    /// [`Self::calendar`] with `page_size` (1 to [`MAX_ROWS`]) exceptions a
    /// page, for the record page.
    pub(crate) async fn calendar_page(
        &self,
        scope: &Scope,
        code: &str,
        page: i64,
        page_size: i64,
    ) -> Result<Calendar> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        authorize_key(&**client, &scope, VIEW).await?;
        store::calendar(&**client, scope.tenant(), code, page, page_size).await
    }
    /// One holiday set with page `page` of its holidays from `from` to `to`
    /// (all dates when omitted) and the first [`MAX_ROWS`] calendars
    /// including it, each with its total. Needs `admin.work-calendar.view`.
    pub async fn holiday_set(
        &self,
        scope: &Scope,
        code: &str,
        from: Option<Date>,
        to: Option<Date>,
        page: i64,
    ) -> Result<HolidaySet> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        authorize_key(&**client, &scope, VIEW).await?;
        store::holiday_set(&**client, scope.tenant(), code, from, to, page).await
    }
}

macro_rules! provenance {
    () => {
        bilibi_module_registry::Provenance {
            module: "core/work_calendar",
            source: file!(),
            line: line!(),
        }
    };
}
pub(crate) use provenance;
// `tenant_owner` holds the tenant's set, as it holds Company's and Person's;
// installation-wide sets are granted to the platform operator by name.
inventory::submit! { Contribution::new(VOCABULARY, "core/work_calendar", provenance!(), &VocabularyContribution {
    domains: &[],
    verbs: &[],
    capabilities: &[CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(LIST), CapabilityDefinition::tenant(MANAGE), CapabilityDefinition::platform(INSTALLATION_MANAGE), CapabilityDefinition::tenant(PUBLISH)],
    roles: &[RoleDefinition { code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW, LIST, MANAGE] }],
    system_principals: &[SystemPrincipalDefinition { name: PUBLISHER, capabilities: &[PUBLISH] }],
}) }
// Telling a tenant of an installation-wide change is Platform work no tenant
// should have to switch on; a tenant administrator may still deny it, and
// that tenant then hears of no installation-wide change.
inventory::submit! { Contribution::new(STANDING_GRANTS, PUBLISHER, provenance!(), &StandingGrant { principal: PUBLISHER, capabilities: &[PUBLISH] }) }
inventory::submit! { Contribution::new(WORKERS, publish::WORKER, provenance!(), &Worker { max_attempts: publish::MAX_ATTEMPTS, validate: publish::arguments }) }
inventory::submit! { Contribution::new(HANDLERS, publish::WORKER, provenance!(), &HandlerBinding { service: publish::HANDLER, capability: PUBLISH }) }
inventory::submit! { Contribution::new(EVENTS, CHANGED, provenance!(), &EventType { version: 1 }) }
inventory::submit! { Contribution::new(DEFINITIONS, DEFAULT, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::Tenant, ScopeKind::Company], default: || serde_json::Value::Null, nullable: true, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value| value.is_null() || value.as_str().is_some_and(model::valid_code), message: "Name a work calendar by its code" }),
    editor: Some(Editor { group: "Work calendars", label: "Default work calendar", help: "The calendar's code, such as \"office\". A company's own value wins over the tenant's; clear a company's value and it uses the tenant's again. A tenant left empty has none.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/work_calendar", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/agent_api", "base/authz", "base/database", "base/datetime", "base/events", "base/grid", "base/menu", "base/party_ids", "base/queue", "base/settings", "base/tenancy", "base/ui", "core/geonames"],
        migrations: &[
            ("core-work-calendar-001-work-calendar", include_str!("../migrations/001-work-calendar.sql")),
            ("core-work-calendar-002-installation-holiday-sets", include_str!("../migrations/002-installation-holiday-sets.sql")),
        ],
        services: &[
            Service { id: SERVICE, factory: |services| {
                let calendars = Arc::new(WorkCalendars {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    authentication: services.get("base/tenancy.authentication")?,
                    settings: services.get("base/settings.store")?,
                    geonames: services.get(bilibi_geonames::SERVICE)?,
                    events: services.get(bilibi_events::SERVICE)?,
                    queue: services.get("base/queue.store")?,
                    boundary: services.get("base/database.mutations")?,
                });
                agent_operations::bind(calendars.clone())?;
                Ok(calendars)
            }},
            Service { id: publish::HANDLER, factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(publish::PublishJob { calendars: services.get(SERVICE)? }))))
            }},
            Service { id: web::SERVICE, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                calendars: services.get(SERVICE)?,
                settings: services.get("base/settings.store")?,
                geonames: services.get(bilibi_geonames::SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
            }))))},
        ],
    }
}
