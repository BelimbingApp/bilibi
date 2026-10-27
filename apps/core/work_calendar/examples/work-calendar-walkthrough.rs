//! Manual walkthrough of work calendars (issue 135) against an explicitly
//! supplied disposable DB. No test harness or assertions; it prints outcomes
//! to inspect. Writes go through the real reviewed-plan service and the
//! arithmetic through `core.work_calendar.business_time`, as an agent calls
//! them.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_database::{pool, Error, MutationBoundary};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{ProofFuture, Scope};
use bilibi_work_calendar::{WorkCalendars, SERVICE};
use serde_json::{json, Value};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Failure = Box<dyn std::error::Error>;
struct Browser<'a> {
    app: &'a bilibi_web::App,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            tx.query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND expires_at>clock_timestamp() FOR UPDATE",
                &[&self.token],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
            Ok(PlanIdentity {
                scope: self.app.security.sessions.check(tx, self.token).await?,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}

struct Agent<'a> {
    plans: &'a Plans,
    registry: &'a Registry,
    browser: Browser<'a>,
    scope: Scope,
    db: &'a bilibi_database::Db,
}
impl Agent<'_> {
    /// Preview, then commit, one reviewed write; prints the review summary.
    async fn write(&self, key: &str, input: Value) -> Result<Value, String> {
        let ops = [PlannedOperation {
            key: key.into(),
            input,
        }];
        let plan = self
            .plans
            .preview(&self.browser, &ops)
            .await
            .map_err(|error| error.to_string())?;
        if plan["valid"] != json!(true) {
            return Err(plan["results"][0]["error"].to_string());
        }
        let code = plan["plan_code"].as_str().ok_or("no plan code")?;
        self.plans
            .commit(&self.scope, &self.browser, code, &ops)
            .await
            .map(|result| result["results"][0]["result"]["result"].clone())
            .map_err(|error| error.to_string())
    }
    async fn read(&self, key: &str, input: Value) -> Result<Value, String> {
        let client = self.db.get().await.map_err(|error| error.to_string())?;
        self.registry
            .call(&**client, &self.scope, key, &input)
            .await
            .map(|out| out["result"].clone())
            .map_err(|error| error.to_string())
    }
    async fn version(&self, key: &str, code: &str) -> i64 {
        self.read(key, json!({"code": code}))
            .await
            .ok()
            .and_then(|value| value["version"].as_i64())
            .unwrap_or(0)
    }
    async fn ask(&self, label: &str, input: Value) {
        match self.read("core.work_calendar.business_time", input).await {
            Ok(answer) if answer["beyond"] == json!(true) => println!("{label}: Beyond"),
            Ok(answer) => println!("{label}: {}", answer["result"]),
            Err(error) => println!("{label}: refused — {error}"),
        }
    }
}
fn show(label: &str, result: Result<Value, String>) {
    match result {
        Ok(value) => println!("{label}: ok {value}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
fn week(working: &[&str], saturday: Value, sunday: Value) -> Value {
    let day = json!({"type": "working", "periods": working});
    json!({"monday": day, "tuesday": day, "wednesday": day, "thursday": day, "friday": day, "saturday": saturday, "sunday": sunday})
}
fn print_days(days: &Value) {
    for day in days["days"].as_array().into_iter().flatten() {
        let reason = &day["reason"];
        let why = match reason["kind"].as_str() {
            Some("week") => "week".to_owned(),
            Some("substitute") => format!(
                "substitute for {} on {}",
                reason["name"].as_str().unwrap_or(""),
                reason["holiday"].as_str().unwrap_or("")
            ),
            _ => format!(
                "{} {}",
                reason["kind"].as_str().unwrap_or(""),
                reason["name"].as_str().unwrap_or("")
            ),
        };
        println!(
            "  {} {:<9} {:<7} (week: {:<7}) {:<24} {}",
            day["date"].as_str().unwrap_or(""),
            jiff::civil::Date::strptime("%Y-%m-%d", day["date"].as_str().unwrap_or("2000-01-01"))
                .map(|d| d.strftime("%A").to_string())
                .unwrap_or_default(),
            day["type"].as_str().unwrap_or(""),
            day["weekly"].as_str().unwrap_or(""),
            day["periods"]
                .as_array()
                .map(|p| p
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default(),
            why,
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_WORK_CALENDAR_DEMO").as_deref() != Ok("disposable") {
        return Err(
            "set BILIBI_WORK_CALENDAR_DEMO=disposable and a disposable DATABASE_URL".into(),
        );
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_web::migrate(&db).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let calendars = app.services.get::<WorkCalendars>(SERVICE)?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let mut agents = Vec::new();
    for email in [
        "ai@agent.my",
        "viewer@example.test",
        "manager@other.example.test",
    ] {
        let login = app.user.login(email, password).await?;
        agents.push((login.token.clone(), app.security.guard(&login.token).await?));
    }
    let agent = |index: usize| Agent {
        plans: &plans,
        registry: &registry,
        browser: Browser {
            app: &app,
            token: &agents[index].0,
        },
        scope: agents[index].1.clone(),
        db: &db,
    };
    let (admin, viewer, other) = (agent(0), agent(1), agent(2));

    println!("== a calendar: Monday-Friday with a lunch break, Saturday off, Sunday rest");
    let office = week(
        &["09:00-13:00", "14:00-18:00"],
        json!({"type": "off"}),
        json!({"type": "rest"}),
    );
    show("create office", admin.write("core.work_calendar.create", json!({"code": "office", "name": "Office", "timezone": "Asia/Kuala_Lumpur", "substitute": "rest", "effective_from": "2026-01-01", "week": office})).await);
    show("a week whose hours run into the next day", admin.write("core.work_calendar.create", json!({"code": "overnight", "name": "Overnight", "timezone": "UTC", "week": week(&["01:30-03:00", "22:00-02:00"], json!({"type": "rest"}), json!({"type": "rest"}))})).await);
    show(
        "a duplicate code",
        admin
            .write(
                "core.work_calendar.create",
                json!({"code": "office", "name": "Again", "week": office}),
            )
            .await,
    );

    println!("== holiday sets: a national and a state set");
    show(
        "a country before Geonames is imported",
        admin
            .write(
                "core.work_calendar.holiday_set.create",
                json!({"code": "my-national", "name": "Malaysia national", "country_iso": "MY"}),
            )
            .await,
    );
    // Demonstration only: the rows a Geonames import would write.
    let client = db.get().await?;
    client.batch_execute("INSERT INTO geonames_countries(iso,iso3,iso_numeric,country,continent) VALUES('MY','MYS','458','Malaysia','AS'); INSERT INTO geonames_admin1(code,name) VALUES('MY.14','Kuala Lumpur'),('MY.01','Johor')").await?;
    show(
        "national set",
        admin
            .write(
                "core.work_calendar.holiday_set.create",
                json!({"code": "my-national", "name": "Malaysia national", "country_iso": "MY"}),
            )
            .await,
    );
    show("state set", admin.write("core.work_calendar.holiday_set.create", json!({"code": "my-kl", "name": "Kuala Lumpur", "country_iso": "MY", "admin1_code": "MY.14"})).await);
    show(
        "a division of another country",
        admin
            .write(
                "core.work_calendar.holiday_set.create",
                json!({"code": "bad", "name": "Bad", "country_iso": "SG", "admin1_code": "MY.14"}),
            )
            .await,
    );
    let version = admin
        .version("core.work_calendar.holiday_set.get", "my-national")
        .await;
    show("a year of national holidays in one call", admin.write("core.work_calendar.holiday_set.holidays", json!({"code": "my-national", "version": version, "add": [
        {"date": "2026-11-08", "name": "Deepavali"}, {"date": "2026-12-25", "name": "Christmas Day"},
        {"date": "2027-01-01", "name": "New Year's Day"}, {"date": "2027-02-06", "name": "Chinese New Year"},
        {"date": "2027-02-07", "name": "Chinese New Year (second day)"}
    ]})).await);
    let version = admin
        .version("core.work_calendar.holiday_set.get", "my-kl")
        .await;
    show("state holiday", admin.write("core.work_calendar.holiday_set.holidays", json!({"code": "my-kl", "version": version, "add": [{"date": "2027-02-01", "name": "Federal Territory Day"}]})).await);
    for set in ["my-national", "my-kl"] {
        let version = admin.version("core.work_calendar.get", "office").await;
        show(
            &format!("include {set}"),
            admin
                .write(
                    "core.work_calendar.include",
                    json!({"code": "office", "version": version, "holiday_set": set}),
                )
                .await,
        );
    }

    println!("== exceptions: a shutdown, a special working Saturday, a half day");
    for (label, input) in [
        (
            "year-end shutdown",
            json!({"starts_on": "2026-12-28", "ends_on": "2026-12-31", "name": "Year-end shutdown", "type": "off"}),
        ),
        (
            "special working Saturday",
            json!({"starts_on": "2026-11-14", "name": "Stock take", "type": "working", "periods": ["09:00-13:00"]}),
        ),
        (
            "Christmas Eve half day",
            json!({"starts_on": "2026-12-24", "name": "Christmas Eve", "type": "working", "periods": ["09:00-13:00"]}),
        ),
        (
            "an overlapping range",
            json!({"starts_on": "2026-12-30", "ends_on": "2027-01-02", "name": "Overlap", "type": "off"}),
        ),
    ] {
        let version = admin.version("core.work_calendar.get", "office").await;
        let mut input = input;
        input["code"] = json!("office");
        input["version"] = json!(version);
        show(
            label,
            admin.write("core.work_calendar.exception.add", input).await,
        );
    }
    show("a stale version", admin.write("core.work_calendar.exception.add", json!({"code": "office", "version": 1, "starts_on": "2027-03-01", "name": "Stale", "type": "off"})).await);

    println!("== month view: November and December 2026 (exception, then holiday or substitute, then week)");
    match admin
        .read(
            "core.work_calendar.days",
            json!({"code": "office", "from": "2026-11-06", "to": "2026-11-16"}),
        )
        .await
    {
        Ok(days) => print_days(&days),
        Err(error) => println!("days: refused — {error}"),
    }
    match admin
        .read(
            "core.work_calendar.days",
            json!({"code": "office", "from": "2026-12-22", "to": "2027-01-05"}),
        )
        .await
    {
        Ok(days) => print_days(&days),
        Err(error) => println!("days: refused — {error}"),
    }

    println!("== a new rule version changes the weekend only from its date (Saturday half days from 2027-01-04)");
    let version = admin.version("core.work_calendar.get", "office").await;
    show("rule from 2027-01-04", admin.write("core.work_calendar.rule.set", json!({"code": "office", "version": version, "effective_from": "2027-01-04", "week": week(&["09:00-13:00", "14:00-18:00"], json!({"type": "working", "periods": ["09:00-13:00"]}), json!({"type": "rest"}))})).await);
    match admin
        .read(
            "core.work_calendar.days",
            json!({"code": "office", "from": "2027-01-02", "to": "2027-01-09"}),
        )
        .await
    {
        Ok(days) => print_days(&days),
        Err(error) => println!("days: refused — {error}"),
    }

    println!(
        "== substitutes under a Friday-Saturday weekend (Friday rest, Saturday off, rest_and_off)"
    );
    let gulf = json!({"sunday": {"type": "working", "periods": ["08:00-16:00"]}, "monday": {"type": "working", "periods": ["08:00-16:00"]}, "tuesday": {"type": "working", "periods": ["08:00-16:00"]}, "wednesday": {"type": "working", "periods": ["08:00-16:00"]}, "thursday": {"type": "working", "periods": ["08:00-16:00"]}, "friday": {"type": "rest"}, "saturday": {"type": "off"}});
    show("create gulf", admin.write("core.work_calendar.create", json!({"code": "gulf", "name": "Gulf office", "timezone": "Asia/Dubai", "substitute": "rest_and_off", "effective_from": "2026-01-01", "week": gulf})).await);
    show(
        "gulf set",
        admin
            .write(
                "core.work_calendar.holiday_set.create",
                json!({"code": "gulf-national", "name": "Gulf national"}),
            )
            .await,
    );
    let version = admin
        .version("core.work_calendar.holiday_set.get", "gulf-national")
        .await;
    show("Friday and Saturday holidays", admin.write("core.work_calendar.holiday_set.holidays", json!({"code": "gulf-national", "version": version, "add": [{"date": "2026-12-04", "name": "Commemoration Day"}, {"date": "2026-12-05", "name": "National Day"}]})).await);
    let version = admin.version("core.work_calendar.get", "gulf").await;
    show(
        "include",
        admin
            .write(
                "core.work_calendar.include",
                json!({"code": "gulf", "version": version, "holiday_set": "gulf-national"}),
            )
            .await,
    );
    match admin
        .read(
            "core.work_calendar.days",
            json!({"code": "gulf", "from": "2026-12-03", "to": "2026-12-08"}),
        )
        .await
    {
        Ok(days) => print_days(&days),
        Err(error) => println!("days: refused — {error}"),
    }

    println!("== business time on office (Asia/Kuala_Lumpur)");
    let office_time = |op: &str, extra: Value| {
        let mut input = json!({"code": "office", "operation": op});
        for (key, value) in extra.as_object().into_iter().flatten() {
            input[key] = value.clone();
        }
        input
    };
    admin
        .ask(
            "add PT8H from Monday 10:00 across the lunch break",
            office_time(
                "add",
                json!({"at": "2026-11-02T10:00:00+08:00", "duration": "PT8H"}),
            ),
        )
        .await;
    admin
        .ask(
            "add PT4H from 14:00 ends at the 18:00 close",
            office_time(
                "add",
                json!({"at": "2026-11-02T14:00:00+08:00", "duration": "PT4H"}),
            ),
        )
        .await;
    admin
        .ask(
            "add PT0S at a closed time stays put",
            office_time(
                "add",
                json!({"at": "2026-11-07T10:00:00+08:00", "duration": "PT0S"}),
            ),
        )
        .await;
    admin
        .ask(
            "add PT1H from a closed Saturday, past Deepavali and its substitute",
            office_time(
                "add",
                json!({"at": "2026-11-07T10:00:00+08:00", "duration": "PT1H"}),
            ),
        )
        .await;
    admin
        .ask(
            "is_open at 13:30 (lunch)",
            office_time("is_open", json!({"at": "2026-11-02T13:30:00+08:00"})),
        )
        .await;
    admin
        .ask(
            "next_open at 13:30",
            office_time("next_open", json!({"at": "2026-11-02T13:30:00+08:00"})),
        )
        .await;
    admin
        .ask(
            "next_open while open",
            office_time("next_open", json!({"at": "2026-11-02T09:30:00+08:00"})),
        )
        .await;
    admin
        .ask(
            "elapsed from the half day to after New Year",
            office_time(
                "elapsed",
                json!({"at": "2026-12-24T09:00:00+08:00", "until": "2027-01-04T09:00:00+08:00"}),
            ),
        )
        .await;
    admin
        .ask(
            "elapsed with until before at",
            office_time(
                "elapsed",
                json!({"at": "2026-11-03T09:00:00+08:00", "until": "2026-11-02T09:00:00+08:00"}),
            ),
        )
        .await;
    admin
        .ask(
            "add_working_days(2026-12-23, 2)",
            office_time("add_working_days", json!({"date": "2026-12-23", "days": 2})),
        )
        .await;
    admin
        .ask(
            "add_working_days(Saturday 2026-11-07, 0)",
            office_time("add_working_days", json!({"date": "2026-11-07", "days": 0})),
        )
        .await;

    println!("== daylight saving and a period past midnight (Europe/London, every day 01:30-03:00 and 22:00-00:30)");
    let night = json!({"type": "working", "periods": ["01:30-03:00", "22:00-00:30"]});
    show("create night", admin.write("core.work_calendar.create", json!({"code": "night", "name": "Night desk", "timezone": "Europe/London", "effective_from": "2026-01-01", "week": {"monday": night, "tuesday": night, "wednesday": night, "thursday": night, "friday": night, "saturday": night, "sunday": night}})).await);
    let night_time = |op: &str, extra: Value| {
        let mut input = json!({"code": "night", "operation": op});
        for (key, value) in extra.as_object().into_iter().flatten() {
            input[key] = value.clone();
        }
        input
    };
    admin.ask("elapsed across the spring-forward night (3h30: 2h30 + 01:30 in the gap opening at 02:00 BST)", night_time("elapsed", json!({"at": "2027-03-27T21:00:00Z", "until": "2027-03-28T04:00:00Z"}))).await;
    admin
        .ask(
            "a 01:30 start inside the gap opens at the first valid instant (01:00Z = 02:00 BST)",
            night_time("next_open", json!({"at": "2027-03-28T00:40:00Z"})),
        )
        .await;
    admin
        .ask(
            "elapsed across the fall-back night (5h: 2h30 + 01:30 BST to 03:00 GMT)",
            night_time(
                "elapsed",
                json!({"at": "2026-10-24T20:00:00Z", "until": "2026-10-25T04:00:00Z"}),
            ),
        )
        .await;
    admin
        .ask(
            "a 01:30 start inside the fold uses the earlier instant (00:30Z = 01:30 BST)",
            night_time("next_open", json!({"at": "2026-10-24T23:40:00Z"})),
        )
        .await;
    admin
        .ask(
            "the 22:00-00:30 period is open past midnight",
            night_time("is_open", json!({"at": "2026-11-03T00:15:00Z"})),
        )
        .await;
    let closed = json!({"type": "rest"});
    show("create closed (no working days)", admin.write("core.work_calendar.create", json!({"code": "closed", "name": "Closed", "timezone": "UTC", "week": {"monday": closed, "tuesday": closed, "wednesday": closed, "thursday": closed, "friday": closed, "saturday": closed, "sunday": closed}})).await);
    admin
        .ask(
            "next_open on a calendar with no working days",
            json!({"code": "closed", "operation": "next_open", "at": "2026-11-02T09:00:00Z"}),
        )
        .await;
    admin.ask("add on a calendar with no working days", json!({"code": "closed", "operation": "add", "at": "2026-11-02T09:00:00Z", "duration": "PT1H"})).await;

    println!("== the company default");
    let settings = app.services.get::<Settings>("base/settings.store")?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let company = OrganisationId::new(1)?;
    println!(
        "default for company 1 before: {:?}",
        calendars.default_for(&admin.scope, company).await?
    );
    let mut connection = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut connection,
            admin
                .scope
                .mutation_context("work_calendar.walkthrough.default"),
        )
        .await?;
    settings
        .put(
            &mut tx,
            &admin.scope,
            Target::Company(company),
            "work_calendar.default",
            json!("office"),
        )
        .await?;
    tx.commit().await?;
    println!(
        "default for company 1 after setting office: {:?}",
        calendars.default_for(&admin.scope, company).await?
    );
    let mut tx = boundary
        .begin(
            &mut connection,
            admin
                .scope
                .mutation_context("work_calendar.walkthrough.default"),
        )
        .await?;
    settings
        .delete(
            &mut tx,
            &admin.scope,
            Target::Company(company),
            "work_calendar.default",
        )
        .await?;
    tx.commit().await?;
    println!(
        "default for company 1 after clearing: {:?}",
        calendars.default_for(&admin.scope, company).await?
    );

    println!(
        "== retire gulf: it still computes, leaves the picker and is refused for a new assignment"
    );
    let version = admin.version("core.work_calendar.get", "gulf").await;
    show(
        "retire",
        admin
            .write(
                "core.work_calendar.update",
                json!({"code": "gulf", "version": version, "status": "retired"}),
            )
            .await,
    );
    println!(
        "choices: {:?}",
        calendars
            .choices(&admin.scope)
            .await?
            .iter()
            .map(|c| c.code.as_str())
            .collect::<Vec<_>>()
    );
    let tx = boundary
        .begin(
            &mut connection,
            admin
                .scope
                .mutation_context("work_calendar.walkthrough.assign"),
        )
        .await?;
    println!(
        "check gulf: {:?}",
        calendars
            .check(tx.sql(), &admin.scope, "gulf")
            .await
            .map(|c| c.code)
    );
    println!(
        "check office: {:?}",
        calendars
            .check(tx.sql(), &admin.scope, "office")
            .await
            .map(|c| c.code)
    );
    tx.rollback().await?;
    admin.ask("retired gulf still computes", json!({"code": "gulf", "operation": "add_working_days", "date": "2026-12-03", "days": 1})).await;

    println!("== changed events (no stored result elsewhere is rewritten)");
    for row in client.query("SELECT subject,payload::text FROM base_events WHERE name='core/work_calendar.changed' ORDER BY id", &[]).await? {
        println!("  {} {}", row.get::<_, String>(0), row.get::<_, String>(1));
    }

    println!("== tenants and capabilities");
    show(
        "tenant 2 lists calendars (it holds no capability)",
        other.read("core.work_calendar.list", json!({})).await,
    );
    // Demonstration only: tenant 2's manager is given the list capability.
    client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(2,5,'admin.work-calendar.list',true) ON CONFLICT DO NOTHING", &[]).await?;
    show(
        "tenant 2 lists calendars with the capability",
        other.read("core.work_calendar.list", json!({})).await,
    );
    println!(
        "tenant 2 loads tenant 1's office: {:?}",
        calendars
            .days(
                &other.scope,
                "office",
                "2026-11-02".parse()?,
                "2026-11-02".parse()?
            )
            .await
            .map(|days| days.len())
    );
    show(
        "viewer reads the list",
        viewer.read("core.work_calendar.list", json!({})).await,
    );
    show(
        "viewer forges a write",
        viewer
            .write(
                "core.work_calendar.create",
                json!({"code": "forged", "name": "Forged", "week": office}),
            )
            .await,
    );

    println!("== audit");
    for row in client.query("SELECT auditable_type,event,count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'core/work_calendar.%' GROUP BY 1,2 ORDER BY 1,2", &[]).await? {
        println!("  {} {} x{}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, i64>(2));
    }
    Ok(())
}
