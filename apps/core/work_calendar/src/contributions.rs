//! The administration pages' routes and menu entries, their page writes and
//! the calendars' Grid table (issue 209). No Bilimbi counterpart; the pages
//! follow Core Currency's. The host checks each route's capability; every
//! write asks again inside its contract.
use crate::{store::zone_in_force, web::SERVICE as PAGES, LIST, VIEW};
use bilibi_grid::{Field, FieldType, GridContribution, Source, TableDecl};
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution,
};

pub(crate) const CALENDARS_PATH: &str = "/work-calendars";
pub(crate) const SETS_PATH: &str = "/holiday-sets";

macro_rules! page {
    ($id:literal, $path:literal, $operation:literal, $cap:expr) => {
        inventory::submit! { Contribution::new(ROUTES, $id, crate::provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
            method: Method::Get, path: $path, service: PAGES, operation: $operation,
            access: Access::Capability($cap), framed: true,
        }) }
    };
}
// A calendar or set is created in a dialog on its list, so no static path
// shares the `{code}` segment: any code is a page.
page!(
    "core/work_calendar.calendars",
    "/work-calendars",
    "calendars",
    LIST
);
page!(
    "core/work_calendar.calendar",
    "/work-calendars/{code}",
    "calendar",
    VIEW
);
page!(
    "core/work_calendar.month",
    "/work-calendars/{code}/month",
    "month",
    VIEW
);
page!(
    "core/work_calendar.holiday-sets",
    "/holiday-sets",
    "holiday_sets",
    LIST
);
page!(
    "core/work_calendar.holiday-set",
    "/holiday-sets/{code}",
    "holiday_set",
    VIEW
);

// Every write goes through the contracts agents call. A confirmation's
// review (`request_*`) is the contract's preview.
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, crate::provenance!(), &PageWrites {
    service: PAGES,
    contracts: &[
        ("create_calendar", "core.work_calendar.create"),
        ("save_calendar", "core.work_calendar.update"),
        ("save_substitute", "core.work_calendar.update"),
        ("request_retire", "core.work_calendar.update"),
        ("retire", "core.work_calendar.update"),
        ("reactivate", "core.work_calendar.update"),
        ("set_rule", "core.work_calendar.rule.set"),
        ("include_set", "core.work_calendar.include"),
        ("leave_out", "core.work_calendar.include"),
        ("add_exception", "core.work_calendar.exception.add"),
        ("request_remove_exception", "core.work_calendar.exception.remove"),
        ("remove_exception", "core.work_calendar.exception.remove"),
        ("create_holiday_set", "core.work_calendar.holiday_set.create"),
        ("save_holiday_set", "core.work_calendar.holiday_set.update"),
        ("save_place", "core.work_calendar.holiday_set.update"),
        ("add_holiday", "core.work_calendar.holiday_set.holidays"),
        ("save_holiday", "core.work_calendar.holiday_set.holidays"),
        ("request_remove_holiday", "core.work_calendar.holiday_set.holidays"),
        ("remove_holiday", "core.work_calendar.holiday_set.holidays"),
    ],
    pending: Pending::None,
}) }

bilibi_menu::contribute!(
    "core/work_calendar",
    bilibi_menu::MenuItem::page("admin.work-calendar", "Work Calendars", CALENDARS_PATH)
        .under("admin")
        .icon("calendar-days")
        .requires(bilibi_menu::Requirement::Capability(LIST))
        .order(60)
);
bilibi_menu::contribute!(
    "core/work_calendar",
    bilibi_menu::MenuItem::page(
        "admin.work-calendar.holiday-sets",
        "Holiday Sets",
        SETS_PATH
    )
    .under("admin")
    .icon("calendar-date-range")
    .requires(bilibi_menu::Requirement::Capability(LIST))
    .order(61)
);

// The calendars as a Grid table, keyed by the code other modules store: the
// zone in force today and the included sets as the list shows them. Every
// field is one the list draws itself, so none is open to restriction.
static GRID: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[TableDecl::new("work_calendars", "Work calendars", LIST,
        Source::tenant(concat!("SELECT c.code,c.name,c.status,c.substitute,", zone_in_force!(), " AS timezone,\
            (SELECT string_agg(s.code, ', ' ORDER BY s.code) FROM core_work_calendar_includes i JOIN core_work_calendar_holiday_sets s ON s.tenant_id=i.set_tenant_id AND s.id=i.holiday_set_id WHERE i.tenant_id=c.tenant_id AND i.calendar_id=c.id) AS holiday_sets \
            FROM core_work_calendars c WHERE c.tenant_id=$1")), "code", &[
            Field::new("code", "Code", FieldType::String).protected(),
            Field::new("name", "Name", FieldType::String).protected(),
            Field::choice("status", "Status", &["active", "retired"]).protected(),
            Field::choice("substitute", "Holiday substitution", &["none", "rest", "rest_and_off"]).protected(),
            Field::new("timezone", "Zone", FieldType::String).protected(),
            Field::new("holiday_sets", "Holiday sets", FieldType::String).protected(),
        ]).label_field("name")],
    links: &[],
};
bilibi_grid::contribute!("core/work_calendar", "work_calendar.grid", &GRID);
