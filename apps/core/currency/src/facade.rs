//! The `Currencies` facade: what forms read (`enabled`, `base`), what an
//! owner's write checks (`check`), the one conversion (`rate`, `convert`) and
//! the operator's curation (enable, base currency, dated rates). Reads need
//! only the sealed scope, since the caller authorises its own operation;
//! every write rechecks its capability inside the write's transaction. A
//! conversion always names the rate it used, its date and how it was found.
//! No Bilimbi counterpart; Belimbing has no shared currency list either.
use crate::{BASE_KEY, MANAGE, MAX_AGE_KEY, RATES_MANAGE, VIEW};
use bilibi_authz::authorize_key;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use bilibi_ui::{Choice, Combobox};
use jiff::civil::Date;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::{GenericClient, Row};

/// The most places a stored rate keeps (`numeric(30,12)`).
pub const RATE_PLACES: u32 = 12;
/// Rates stay below this, so a conversion cannot overflow a `Decimal`.
const RATE_CEILING: i64 = 1_000_000_000_000;
/// Amounts a conversion accepts stay below this magnitude.
const AMOUNT_CEILING: i64 = 1_000_000_000_000_000;
pub const NOTE_MAX: usize = 200;

/// One ISO 4217 currency as the tenant sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Currency {
    pub code: String,
    pub numeric_code: String,
    pub name: String,
    /// Decimal places of the smallest unit (JPY 0, MYR 2, KWD 3).
    pub minor_units: u32,
    /// ISO has retired the code. Records holding it still read; it cannot be
    /// enabled or chosen for something new.
    pub withdrawn: bool,
    /// The tenant has switched it on.
    pub enabled: bool,
}
impl Currency {
    fn from_row(row: &Row) -> Self {
        Self {
            code: row.get("code"),
            numeric_code: row.get("numeric_code"),
            name: row.get("name"),
            minor_units: row.get::<_, i16>("minor_units") as u32,
            withdrawn: row.get("withdrawn"),
            enabled: row.get("enabled"),
        }
    }
    /// `MYR — Malaysian ringgit`, the label every currency control shows.
    pub fn label(&self) -> String {
        format!("{} — {}", self.code, self.name)
    }
    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "numeric_code": self.numeric_code,
            "name": self.name,
            "minor_units": self.minor_units,
            "withdrawn": self.withdrawn,
            "enabled": self.enabled,
        })
    }
}
const CURRENCY_COLUMNS: &str =
    "c.code,c.numeric_code,c.name,c.minor_units,c.withdrawn,(e.code IS NOT NULL) AS enabled";
const CURRENCY_FROM: &str =
    "FROM currencies c LEFT JOIN currency_tenant_enabled e ON e.tenant_id=$1 AND e.code=c.code";

/// One dated rate: 1 `from` = `rate` `to` from `effective_on` until a later row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rate {
    pub id: i64,
    pub from: String,
    pub to: String,
    pub effective_on: Date,
    pub rate: Decimal,
    pub source_note: Option<String>,
    pub version: i64,
}
const RATE_COLUMNS: &str =
    "id,from_code,to_code,effective_on::text AS effective_on,rate,source_note,version";
impl Rate {
    fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            id: row.get("id"),
            from: row.get("from_code"),
            to: row.get("to_code"),
            effective_on: row
                .get::<_, String>("effective_on")
                .parse()
                .map_err(|_| Error::Internal)?,
            rate: row.get::<_, Decimal>("rate").normalize(),
            source_note: row.get("source_note"),
            version: row.get("version"),
        })
    }
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "from": self.from,
            "to": self.to,
            "effective_on": self.effective_on.to_string(),
            "rate": self.rate.to_string(),
            "source_note": self.source_note,
            "version": self.version,
        })
    }
}

/// How a rate was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// Both sides are the same currency; no rate is involved.
    Same,
    /// A rate entered for exactly this pair.
    Direct,
    /// One over the rate entered for the opposite pair.
    Inverse,
    /// Two rates, through the base currency; the older date is reported.
    ViaBase,
}
impl Path {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Direct => "direct",
            Self::Inverse => "inverse",
            Self::ViaBase => "via_base",
        }
    }
}

/// The rate a conversion used, ready for the caller to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub rate: Decimal,
    /// The effective date of the rate (the older one, via the base currency).
    pub rate_date: Date,
    pub path: Path,
    /// The `currency_rates` rows used: none, one, or two via the base.
    pub rate_ids: Vec<i64>,
}
impl Quote {
    pub fn to_json(&self) -> Value {
        json!({
            "rate": self.rate.to_string(),
            "rate_date": self.rate_date.to_string(),
            "path": self.path.as_str(),
            "rate_ids": self.rate_ids,
        })
    }
}

/// An amount in the target currency, with what produced it. The caller stores
/// what it needs; a later rate change rewrites nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversion {
    pub amount: Decimal,
    pub from: String,
    pub to: String,
    pub quote: Quote,
}
impl Conversion {
    pub fn to_json(&self) -> Value {
        json!({
            "amount": self.amount.to_string(),
            "from": self.from,
            "to": self.to,
            "rate": self.quote.rate.to_string(),
            "rate_date": self.quote.rate_date.to_string(),
            "path": self.quote.path.as_str(),
            "rate_ids": self.quote.rate_ids,
        })
    }
}

/// What a conversion needs. Rounds to the target's minor units, half away
/// from zero, unless the caller asks for the unrounded value.
#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub amount: Decimal,
    pub from: &'a str,
    pub to: &'a str,
    pub on: Date,
    pub round: bool,
}
impl<'a> Request<'a> {
    pub fn new(amount: Decimal, from: &'a str, to: &'a str, on: Date) -> Self {
        Self {
            amount,
            from,
            to,
            on,
            round: true,
        }
    }
    pub fn unrounded(mut self) -> Self {
        self.round = false;
        self
    }
}

/// Why a lookup or conversion refused. Never an assumed rate of 1 and never a
/// silent old rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    UnknownCurrency(String),
    /// The amount has more decimal places than its currency allows.
    TooManyPlaces {
        currency: String,
        minor_units: u32,
    },
    OutOfRange,
    /// No usable rate on or before the date (none entered, or too old).
    NoRate {
        from: String,
        to: String,
        on: Date,
    },
    /// A conversion to the base currency before one is set.
    NoBase,
}
impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Self::UnknownCurrency(code) => format!("{code} is not a currency code"),
            Self::TooManyPlaces {
                currency,
                minor_units,
            } => format!(
                "The amount has more decimal places than {currency} allows ({minor_units})"
            ),
            Self::OutOfRange => "The amount is too large to convert".into(),
            Self::NoRate { from, to, on } => format!(
                "No exchange rate from {from} to {to} on or before {on} is recent enough. Add one on the Exchange rates page"
            ),
            Self::NoBase => {
                "No base currency is set. Choose one on the Currencies page".into()
            }
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownCurrency(_) => "unknown_currency",
            Self::TooManyPlaces { .. } => "too_many_places",
            Self::OutOfRange => "out_of_range",
            Self::NoRate { .. } => "no_rate",
            Self::NoBase => "no_base",
        }
    }
}

/// Why `check` refuses a code for a new assignment. A record that keeps the
/// code it already holds does not need the check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unusable {
    Unknown,
    NotEnabled,
    Withdrawn,
}
impl Unusable {
    pub fn message(self) -> &'static str {
        match self {
            Self::Unknown => "is not a currency code",
            Self::NotEnabled => "is not enabled; enable it on the Currencies page",
            Self::Withdrawn => "has been withdrawn from ISO 4217",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}
impl FieldError {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}
/// A write's answer: the saved value, or what the operator must fix.
pub type Checked<T> = std::result::Result<T, Vec<FieldError>>;

/// A rate to enter. `version` is the version read when the rate for this
/// pair and date already exists; leave it out to add a new one.
#[derive(Clone, Debug)]
pub struct RateInput {
    pub from: String,
    pub to: String,
    pub effective_on: Date,
    pub rate: Decimal,
    pub source_note: Option<String>,
    pub version: Option<i64>,
}

/// The rates page's filter.
#[derive(Clone, Debug, Default)]
pub struct RateQuery {
    /// A code on either side.
    pub code: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// The registry service `core/currency.currencies`.
pub struct Currencies {
    pub(crate) settings: Arc<Settings>,
}

fn code_shaped(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_uppercase())
}
/// `myr ` -> `MYR`; anything else is not a code.
pub fn normalize(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_uppercase();
    code_shaped(&code).then_some(code)
}
fn civil(date: Date) -> String {
    date.to_string()
}

impl Currencies {
    // ------------------------------------------------------------ reading

    /// A currency of the catalog, enabled or not, withdrawn or not: a record
    /// that holds a code can always show it. `None` for a code ISO never had.
    pub async fn get(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: &str,
    ) -> Result<Option<Currency>> {
        let Some(code) = normalize(code) else {
            return Ok(None);
        };
        Ok(client
            .query_opt(
                &format!("SELECT {CURRENCY_COLUMNS} {CURRENCY_FROM} WHERE c.code=$2"),
                &[&scope.tenant(), &code],
            )
            .await?
            .as_ref()
            .map(Currency::from_row))
    }
    /// What a form offers: the currencies the tenant enabled, by code.
    pub async fn enabled(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<Currency>> {
        Ok(client
            .query(
                &format!("SELECT {CURRENCY_COLUMNS} {CURRENCY_FROM} WHERE e.code IS NOT NULL AND NOT c.withdrawn ORDER BY c.code"),
                &[&scope.tenant()],
            )
            .await?
            .iter()
            .map(Currency::from_row)
            .collect())
    }
    /// The options of a currency control, `MYR — Malaysian ringgit`.
    pub async fn choices(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<Choice>> {
        Ok(self
            .enabled(client, scope)
            .await?
            .into_iter()
            .map(|currency| Choice::new(currency.label(), currency.code))
            .collect())
    }
    /// The one currency control every form reuses: a searchable `Combobox` of
    /// the enabled currencies. Put it in a form with `forms::combobox`.
    pub async fn combobox(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: &str,
        name: &str,
        label: &str,
    ) -> Result<Combobox> {
        Ok(Combobox::new(id, name)
            .label(label)
            .options(self.choices(client, scope).await?))
    }
    /// The base currency: the company's own when it has one, else the
    /// tenant's. `None` until one is set; a conversion to the base refuses.
    pub async fn base(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company: Option<OrganisationId>,
    ) -> Result<Option<Currency>> {
        let target = company.map_or(Target::Tenant, Target::Company);
        let resolved = self.settings.get(client, scope, target, BASE_KEY).await?;
        match resolved.value.as_str() {
            Some(code) => self.get(client, scope, code).await,
            None => Ok(None),
        }
    }
    /// The base currency a company chose for itself; `None` when it follows
    /// the tenant's. [`base`](Self::base) answers either way.
    pub async fn own_base(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company: OrganisationId,
    ) -> Result<Option<Currency>> {
        let resolved = self
            .settings
            .get(client, scope, Target::Company(company), BASE_KEY)
            .await?;
        match resolved.value.as_str().filter(|_| resolved.overridden) {
            Some(code) => self.get(client, scope, code).await,
            None => Ok(None),
        }
    }
    /// Whether a new assignment may use `code`, on the owner's transaction.
    /// The enabled row is locked `FOR SHARE`, so a concurrent disable waits
    /// for the owner's commit.
    pub async fn check(
        &self,
        tx: &MutationTransaction<'_>,
        scope: &Scope,
        code: &str,
    ) -> Result<std::result::Result<Currency, Unusable>> {
        let Some(code) = normalize(code) else {
            return Ok(Err(Unusable::Unknown));
        };
        let client = tx.sql();
        client
            .execute(
                "SELECT 1 FROM currency_tenant_enabled WHERE tenant_id=$1 AND code=$2 FOR SHARE",
                &[&scope.tenant(), &code],
            )
            .await?;
        Ok(match self.get(client, scope, &code).await? {
            None => Err(Unusable::Unknown),
            Some(currency) if currency.withdrawn => Err(Unusable::Withdrawn),
            Some(currency) if !currency.enabled => Err(Unusable::NotEnabled),
            Some(currency) => Ok(currency),
        })
    }

    /// Every catalog entry with the tenant's switch, under `view`.
    pub async fn catalog(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<Currency>> {
        authorize_key(client, scope, VIEW).await?;
        Ok(client
            .query(
                &format!("SELECT {CURRENCY_COLUMNS} {CURRENCY_FROM} ORDER BY (e.code IS NULL), c.withdrawn, c.code"),
                &[&scope.tenant()],
            )
            .await?
            .iter()
            .map(Currency::from_row)
            .collect())
    }
    /// The rates, newest first, under `view`; with the number that match.
    pub async fn rates(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        query: &RateQuery,
    ) -> Result<(Vec<Rate>, i64)> {
        authorize_key(client, scope, VIEW).await?;
        let code = query.code.as_deref().and_then(normalize);
        let filter = "tenant_id=$1 AND ($2::text IS NULL OR from_code=$2 OR to_code=$2)";
        let total = client
            .query_one(
                &format!("SELECT count(*) FROM currency_rates WHERE {filter}"),
                &[&scope.tenant(), &code],
            )
            .await?
            .get(0);
        let rows = client
            .query(
                &format!("SELECT {RATE_COLUMNS} FROM currency_rates WHERE {filter} ORDER BY effective_on DESC, from_code, to_code LIMIT $3 OFFSET $4"),
                &[&scope.tenant(), &code, &query.limit.clamp(1, 500), &query.offset.max(0)],
            )
            .await?
            .iter()
            .map(Rate::from_row)
            .collect::<Result<_>>()?;
        Ok((rows, total))
    }

    // --------------------------------------------------------- converting

    async fn max_age(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Option<i32>> {
        let days = self
            .settings
            .get(client, scope, Target::Tenant, MAX_AGE_KEY)
            .await?
            .value
            .as_i64()
            .ok_or(Error::Internal)?;
        Ok((days > 0).then(|| days.min(i32::MAX as i64) as i32))
    }
    /// The latest rate on or before `on` for one ordered pair, if it is recent
    /// enough.
    async fn latest(
        client: &(impl GenericClient + Sync),
        tenant: i64,
        from: &str,
        to: &str,
        on: Date,
        max_age: Option<i32>,
    ) -> Result<Option<Rate>> {
        let on = civil(on);
        client
            .query_opt(
                &format!("SELECT {RATE_COLUMNS} FROM currency_rates WHERE tenant_id=$1 AND from_code=$2 AND to_code=$3 \
                          AND effective_on <= $4::text::date AND ($5::int IS NULL OR effective_on >= $4::text::date - $5::int) \
                          ORDER BY effective_on DESC LIMIT 1"),
                &[&tenant, &from, &to, &on, &max_age],
            )
            .await?
            .as_ref()
            .map(Rate::from_row)
            .transpose()
    }
    /// A pair, direct first, then the inverse of the opposite pair.
    async fn leg(
        client: &(impl GenericClient + Sync),
        tenant: i64,
        from: &str,
        to: &str,
        on: Date,
        max_age: Option<i32>,
    ) -> Result<Option<Quote>> {
        if let Some(rate) = Self::latest(client, tenant, from, to, on, max_age).await? {
            return Ok(Some(Quote {
                rate: rate.rate,
                rate_date: rate.effective_on,
                path: Path::Direct,
                rate_ids: vec![rate.id],
            }));
        }
        Ok(Self::latest(client, tenant, to, from, on, max_age)
            .await?
            .map(|rate| Quote {
                rate: Decimal::ONE / rate.rate,
                rate_date: rate.effective_on,
                path: Path::Inverse,
                rate_ids: vec![rate.id],
            }))
    }
    /// The rate to turn 1 `from` into `to` on `on`: the direct pair, the
    /// inverse of the opposite pair, then two rates through the base currency
    /// (the older date is reported). A rate older than the tenant's
    /// `currency.rates.max_age_days` is not used. No rate is a refusal.
    pub async fn rate(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        from: &str,
        to: &str,
        on: Date,
    ) -> Result<std::result::Result<Quote, Refusal>> {
        let Some(from) = normalize(from) else {
            return Ok(Err(Refusal::UnknownCurrency(from.trim().to_owned())));
        };
        let Some(to) = normalize(to) else {
            return Ok(Err(Refusal::UnknownCurrency(to.trim().to_owned())));
        };
        for code in [&from, &to] {
            if self.get(client, scope, code).await?.is_none() {
                return Ok(Err(Refusal::UnknownCurrency(code.clone())));
            }
        }
        if from == to {
            return Ok(Ok(Quote {
                rate: Decimal::ONE,
                rate_date: on,
                path: Path::Same,
                rate_ids: Vec::new(),
            }));
        }
        let tenant = scope.tenant();
        let max_age = self.max_age(client, scope).await?;
        if let Some(quote) = Self::leg(client, tenant, &from, &to, on, max_age).await? {
            return Ok(Ok(quote));
        }
        if let Some(base) = self.base(client, scope, None).await? {
            if base.code != from && base.code != to {
                if let (Some(first), Some(second)) = (
                    Self::leg(client, tenant, &from, &base.code, on, max_age).await?,
                    Self::leg(client, tenant, &base.code, &to, on, max_age).await?,
                ) {
                    if let Some(rate) = first.rate.checked_mul(second.rate) {
                        let mut rate_ids = first.rate_ids;
                        rate_ids.extend(second.rate_ids);
                        return Ok(Ok(Quote {
                            rate,
                            rate_date: first.rate_date.min(second.rate_date),
                            path: Path::ViaBase,
                            rate_ids,
                        }));
                    }
                }
            }
        }
        Ok(Err(Refusal::NoRate { from, to, on }))
    }
    /// An amount in another currency, with the rate, its date and how it was
    /// found. The amount is refused when it has more places than its own
    /// currency allows.
    pub async fn convert(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        request: &Request<'_>,
    ) -> Result<std::result::Result<Conversion, Refusal>> {
        let quote = match self
            .rate(client, scope, request.from, request.to, request.on)
            .await?
        {
            Ok(quote) => quote,
            Err(refusal) => return Ok(Err(refusal)),
        };
        // `rate` proved both codes exist.
        let source = self
            .get(client, scope, request.from)
            .await?
            .ok_or(Error::Internal)?;
        let target = self
            .get(client, scope, request.to)
            .await?
            .ok_or(Error::Internal)?;
        if request.amount.normalize().scale() > source.minor_units {
            return Ok(Err(Refusal::TooManyPlaces {
                currency: source.code,
                minor_units: source.minor_units,
            }));
        }
        if request.amount.abs() >= Decimal::from(AMOUNT_CEILING) {
            return Ok(Err(Refusal::OutOfRange));
        }
        let Some(mut amount) = request.amount.checked_mul(quote.rate) else {
            return Ok(Err(Refusal::OutOfRange));
        };
        if request.round {
            amount = amount
                .round_dp_with_strategy(target.minor_units, RoundingStrategy::MidpointAwayFromZero);
        }
        Ok(Ok(Conversion {
            amount,
            from: source.code,
            to: target.code,
            quote,
        }))
    }
    /// [`convert`](Self::convert) into the tenant's base currency; refuses
    /// with `NoBase` until one is set.
    pub async fn convert_to_base(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        amount: Decimal,
        from: &str,
        on: Date,
    ) -> Result<std::result::Result<Conversion, Refusal>> {
        let Some(base) = self.base(client, scope, None).await? else {
            return Ok(Err(Refusal::NoBase));
        };
        self.convert(client, scope, &Request::new(amount, from, &base.code, on))
            .await
    }

    // ------------------------------------------------------------ curating

    /// Serializes the base, enable and disable writes of one tenant, so a
    /// disable cannot slip past a base currency being chosen.
    async fn lock(client: &(impl GenericClient + Sync), tenant: i64) -> Result<()> {
        client
            .execute(
                "SELECT pg_advisory_xact_lock(hashtextextended('core-currency-' || $1::text, 0))",
                &[&tenant.to_string()],
            )
            .await?;
        Ok(())
    }

    async fn check_enabled_change(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: &str,
        enable: bool,
        lock: bool,
    ) -> Result<Checked<Currency>> {
        authorize_key(client, scope, MANAGE).await?;
        if lock {
            Self::lock(client, scope.tenant()).await?;
        }
        let Some(currency) = self.get(client, scope, code).await? else {
            return Ok(Err(vec![FieldError::new("code", "is not a currency code")]));
        };
        if enable && currency.withdrawn && !currency.enabled {
            return Ok(Err(vec![FieldError::new(
                "code",
                "has been withdrawn from ISO 4217, so it cannot be enabled",
            )]));
        }
        if !enable {
            let users = self.base_users(client, scope, &currency.code).await?;
            if !users.is_empty() {
                return Ok(Err(vec![FieldError::new(
                    "code",
                    format!(
                        "is the base currency of {}; choose another base currency first",
                        users.join(", ")
                    ),
                )]));
            }
        }
        Ok(Ok(currency))
    }
    /// Who has `code` as a base currency: the tenant, and each company that
    /// chose it for itself.
    async fn base_users(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: &str,
    ) -> Result<Vec<String>> {
        let mut users = Vec::new();
        if self
            .base(client, scope, None)
            .await?
            .is_some_and(|base| base.code == code)
        {
            users.push("the tenant".to_owned());
        }
        for company in bilibi_company::list_tenant_company_ids(client, scope).await? {
            if self
                .own_base(client, scope, company)
                .await?
                .is_some_and(|base| base.code == code)
            {
                users.push(
                    match bilibi_company::detail(client, scope, company.get()).await {
                        Ok((company, _)) => company.display_name(),
                        Err(_) => format!("company {}", company.get()),
                    },
                );
            }
        }
        Ok(users)
    }
    /// The currency as it would stand after the change, without making it.
    pub async fn preview_enabled(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: &str,
        enable: bool,
    ) -> Result<Checked<Currency>> {
        Ok(self
            .check_enabled_change(client, scope, code, enable, false)
            .await?
            .map(|currency| Currency {
                enabled: enable,
                ..currency
            }))
    }
    /// Turns a currency on or off for the tenant, under `admin.currency.manage`.
    /// A disabled currency stays readable on the records that hold it.
    pub async fn set_enabled(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        code: &str,
        enable: bool,
    ) -> Result<Checked<Currency>> {
        let currency = match self
            .check_enabled_change(tx.sql(), scope, code, enable, true)
            .await?
        {
            Ok(currency) => currency,
            Err(errors) => return Ok(Err(errors)),
        };
        if currency.enabled == enable {
            return Ok(Ok(currency));
        }
        let tenant = scope.tenant();
        let saved = Currency {
            enabled: enable,
            ..currency
        };
        let (before, after) = if enable {
            (json!({}), saved.to_json())
        } else {
            (
                Currency {
                    enabled: true,
                    ..saved.clone()
                }
                .to_json(),
                json!({}),
            )
        };
        let code = saved.code.clone();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let id: i64 = if enable {
                    sql.query_one(
                        "INSERT INTO currency_tenant_enabled(tenant_id,code) VALUES($1,$2) \
                         ON CONFLICT (tenant_id,code) DO UPDATE SET code=EXCLUDED.code RETURNING id",
                        &[&tenant, &code],
                    )
                    .await?
                    .get(0)
                } else {
                    sql.query_one(
                        "DELETE FROM currency_tenant_enabled WHERE tenant_id=$1 AND code=$2 RETURNING id",
                        &[&tenant, &code],
                    )
                    .await?
                    .get(0)
                };
                Ok((
                    (),
                    vec![Change {
                        kind: "currency_enabled".into(),
                        id,
                        old: before,
                        new: after,
                    }],
                ))
            })
        })
        .await?;
        Ok(Ok(saved))
    }

    async fn check_base(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: Option<&str>,
        lock: bool,
    ) -> Result<Checked<Option<Currency>>> {
        authorize_key(client, scope, MANAGE).await?;
        if lock {
            Self::lock(client, scope.tenant()).await?;
        }
        let Some(code) = code else {
            return Ok(Ok(None));
        };
        match self.get(client, scope, code).await? {
            Some(currency) if currency.enabled && !currency.withdrawn => Ok(Ok(Some(currency))),
            Some(_) => Ok(Err(vec![FieldError::new(
                "code",
                "must be an enabled currency; enable it first",
            )])),
            None => Ok(Err(vec![FieldError::new("code", "is not a currency code")])),
        }
    }
    /// The base currency a write would leave in place.
    pub async fn preview_base(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: Option<&str>,
    ) -> Result<Checked<Option<Currency>>> {
        self.check_base(client, scope, code, false).await
    }
    /// Sets the tenant's base currency, or a company's own; `None` clears it
    /// (a company then follows the tenant). The currency must be enabled.
    /// Saved through Settings, which audits the change.
    pub async fn set_base(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        code: Option<&str>,
        company: Option<OrganisationId>,
    ) -> Result<Checked<Option<Currency>>> {
        let currency = match self.check_base(tx.sql(), scope, code, true).await? {
            Ok(currency) => currency,
            Err(errors) => return Ok(Err(errors)),
        };
        let target = company.map_or(Target::Tenant, Target::Company);
        match &currency {
            Some(currency) => {
                self.settings
                    .put(tx, scope, target, BASE_KEY, json!(currency.code))
                    .await?
            }
            None => self.settings.delete(tx, scope, target, BASE_KEY).await?,
        }
        Ok(Ok(currency))
    }

    async fn check_rate(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        input: &RateInput,
        lock: bool,
    ) -> Result<Checked<(Option<Rate>, RateInput)>> {
        authorize_key(client, scope, RATES_MANAGE).await?;
        if lock {
            Self::lock(client, scope.tenant()).await?;
        }
        let mut errors = Vec::new();
        let from = normalize(&input.from);
        let to = normalize(&input.to);
        if from.is_none() {
            errors.push(FieldError::new("from", "is not a currency code"));
        }
        if to.is_none() {
            errors.push(FieldError::new("to", "is not a currency code"));
        }
        if from.is_some() && from == to {
            errors.push(FieldError::new(
                "to",
                "must differ from the currency it converts from",
            ));
        }
        if input.rate <= Decimal::ZERO {
            errors.push(FieldError::new("rate", "must be more than zero"));
        } else if input.rate.normalize().scale() > RATE_PLACES {
            errors.push(FieldError::new(
                "rate",
                format!("can have at most {RATE_PLACES} decimal places"),
            ));
        } else if input.rate >= Decimal::from(RATE_CEILING) {
            errors.push(FieldError::new("rate", "is too large"));
        }
        let note = input
            .source_note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_owned);
        if note
            .as_ref()
            .is_some_and(|note| note.chars().count() > NOTE_MAX)
        {
            errors.push(FieldError::new(
                "source_note",
                format!("can have at most {NOTE_MAX} characters"),
            ));
        }
        let (Some(from), Some(to)) = (from, to) else {
            return Ok(Err(errors));
        };
        let tenant = scope.tenant();
        let on = civil(input.effective_on);
        let existing = client
            .query_opt(
                &format!("SELECT {RATE_COLUMNS} FROM currency_rates WHERE tenant_id=$1 AND from_code=$2 AND to_code=$3 AND effective_on=$4::text::date{}", if lock { " FOR UPDATE" } else { "" }),
                &[&tenant, &from, &to, &on],
            )
            .await?
            .as_ref()
            .map(Rate::from_row)
            .transpose()?;
        match (&existing, input.version) {
            (Some(_), None) => errors.push(FieldError::new(
                "version",
                "is needed: a rate for this pair and date already exists; read it and change it with its version",
            )),
            (Some(rate), Some(version)) if rate.version != version => {
                errors.push(FieldError::new("version", "the rate changed; reload before saving"))
            }
            (None, Some(_)) => errors.push(FieldError::new(
                "version",
                "no rate exists for this pair and date; leave the version out to add one",
            )),
            _ => {}
        }
        if existing.is_none() {
            // A new rate names currencies the tenant trades in.
            for (field, code) in [("from", &from), ("to", &to)] {
                match self.get(client, scope, code).await? {
                    None => errors.push(FieldError::new(field, "is not a currency code")),
                    Some(currency) if currency.withdrawn => {
                        errors.push(FieldError::new(field, "has been withdrawn from ISO 4217"))
                    }
                    Some(currency) if !currency.enabled => errors.push(FieldError::new(
                        field,
                        "is not enabled; enable it on the Currencies page",
                    )),
                    Some(_) => {}
                }
            }
        }
        if !errors.is_empty() {
            return Ok(Err(errors));
        }
        Ok(Ok((
            existing,
            RateInput {
                from,
                to,
                effective_on: input.effective_on,
                rate: input.rate.normalize(),
                source_note: note,
                version: input.version,
            },
        )))
    }
    /// The rate before (if any) and as it would be saved, without saving.
    pub async fn preview_set_rate(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        input: &RateInput,
    ) -> Result<Checked<(Option<Rate>, RateInput)>> {
        self.check_rate(client, scope, input, false).await
    }
    /// Adds a rate, or changes the one for this pair and date with its
    /// version. Audited with the old and new values.
    pub async fn set_rate(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &RateInput,
    ) -> Result<Checked<Rate>> {
        let (before, input) = match self.check_rate(tx.sql(), scope, input, true).await? {
            Ok(checked) => checked,
            Err(errors) => return Ok(Err(errors)),
        };
        let tenant = scope.tenant();
        let saved = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let on = civil(input.effective_on);
                    let row = match &before {
                        Some(before) => sql
                            .query_one(
                                &format!("UPDATE currency_rates SET rate=$3,source_note=$4,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {RATE_COLUMNS}"),
                                &[&tenant, &before.id, &input.rate, &input.source_note],
                            )
                            .await?,
                        None => sql
                            .query_one(
                                &format!("INSERT INTO currency_rates(tenant_id,from_code,to_code,effective_on,rate,source_note) VALUES($1,$2,$3,$4::text::date,$5,$6) RETURNING {RATE_COLUMNS}"),
                                &[&tenant, &input.from, &input.to, &on, &input.rate, &input.source_note],
                            )
                            .await?,
                    };
                    let saved = Rate::from_row(&row)?;
                    let change = Change {
                        kind: "currency_rate".into(),
                        id: saved.id,
                        old: before.as_ref().map_or(json!({}), Rate::to_json),
                        new: saved.to_json(),
                    };
                    Ok((saved, vec![change]))
                })
            })
            .await?;
        Ok(Ok(saved))
    }

    async fn check_delete(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
        version: i64,
        lock: bool,
    ) -> Result<Checked<Rate>> {
        authorize_key(client, scope, RATES_MANAGE).await?;
        let rate = client
            .query_opt(
                &format!(
                    "SELECT {RATE_COLUMNS} FROM currency_rates WHERE tenant_id=$1 AND id=$2{}",
                    if lock { " FOR UPDATE" } else { "" }
                ),
                &[&scope.tenant(), &id],
            )
            .await?
            .as_ref()
            .map(Rate::from_row)
            .transpose()?
            .ok_or(Error::NotFound)?;
        if rate.version != version {
            return Ok(Err(vec![FieldError::new(
                "version",
                "the rate changed; reload before deleting",
            )]));
        }
        Ok(Ok(rate))
    }
    /// The rate a delete would remove.
    pub async fn preview_delete_rate(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
        version: i64,
    ) -> Result<Checked<Rate>> {
        self.check_delete(client, scope, id, version, false).await
    }
    /// Deletes a rate under `admin.currency.rates.manage`, audited. Past
    /// conversions are unchanged: callers stored what they used.
    pub async fn delete_rate(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
    ) -> Result<Checked<Rate>> {
        let rate = match self
            .check_delete(tx.sql(), scope, id, version, true)
            .await?
        {
            Ok(rate) => rate,
            Err(errors) => return Ok(Err(errors)),
        };
        let tenant = scope.tenant();
        let removed = rate.clone();
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "DELETE FROM currency_rates WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &removed.id],
                )
                .await?;
                let change = Change {
                    kind: "currency_rate".into(),
                    id: removed.id,
                    old: removed.to_json(),
                    new: json!({}),
                };
                Ok(((), vec![change]))
            })
        })
        .await?;
        Ok(Ok(rate))
    }
}
