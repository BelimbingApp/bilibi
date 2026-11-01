# Core Currency

The one currency list, the currencies each tenant trades in, a base currency, dated exchange rates and the one conversion every module shares. It is Core master data because a Domain may depend on Core but never on another Domain, so a list kept in Finance could not serve CRM or Factory. Callers use `Currencies` (registry service `core/currency.currencies`); table layout and SQL stay private. Decision: [ADR 08](../../../docs/architecture/decisions/08-currencies-as-core-master-data.md).

## What a module does

| I need | Call |
| --- | --- |
| A currency dropdown | `currencies.combobox(client, scope, id, name, label)`, or `choices` for the options |
| Show a stored code, even a disabled one | `currencies.get(client, scope, code)` (`name`, `minor_units`, `withdrawn`, `enabled`) |
| Check a code for a **new** record | `currencies.check(&tx, scope, code)` on the write's transaction; refuses `Unknown`, `NotEnabled`, `Withdrawn` |
| Convert an amount | `currencies.convert(client, scope, &Request::new(amount, "USD", "MYR", date))` |
| Convert to the base currency | `currencies.convert_to_base(client, scope, amount, "USD", date)` |
| The base currency | `currencies.base(client, scope, company)` |

Never keep a free-text currency code, your own minor-units list or your own rate table. Never store `f64`: amounts are `rust_decimal::Decimal` (re-exported as `bilibi_currency::Decimal`), and JSON carries them as strings.

## The list

- **ISO 4217 ships.** `currencies` is global, with code, numeric code, name, decimal places (`minor_units`, 0 to 4) and a `withdrawn` flag. Production seed `currencies-iso4217-v1` inserts the list by code and never updates a row, so a rerun changes nothing. It leaves out the precious-metal and test codes (XAU, XTS, XXX…). A later seed version adds new codes and marks withdrawn ones.
- **Tenants enable and disable; they never add a code or change decimal places.** A disabled or withdrawn currency stays readable (`get`) for records that hold it, but `check` refuses it for a new assignment. A withdrawn code cannot be enabled. A currency that is the tenant's base, or any company's own base, cannot be disabled; the refusal names who uses it. An archived or deleted company holds no usable base, so it never blocks a disable.

## Base currency

- Setting `currency.base` (tenant, and company to override). `null` until set: `base` answers `None` and `convert_to_base` refuses with `NoBase`. The pages and `core.currency.base.set` are its operator UI; they accept only an enabled currency.
- When it is unset, the Currencies page suggests the primary company's country currency from Geonames. It never saves the suggestion unasked.
- Conversion through the base uses the tenant's base. A company's own base is for reports and forms; `base(.., Some(company))` reads it.

## Rates

- `currency_rates`: one row per tenant, pair and effective date. 1 `from` = `rate` `to` from that date until a later row. `rate` is `numeric(30,12)`, more than zero, at most 12 decimal places. Entered by hand; nothing is fetched. An Extension may later write rates through the same reviewed operation.
- Adding needs both currencies enabled. A rate is changed in place with its `version`, deleted with its `version`; both are audited with old and new values (`currency_rate`). Past conversions are unchanged because callers store what they used.
- Setting `currency.rates.max_age_days` (tenant, default 7, `0` means no limit): a rate older than this, counted back from the conversion date, is not used.

## Conversion

`rate(from, to, on)` and `convert` look for the latest rate on or before the date, in this order: the pair itself (`Direct`), one over the opposite pair (`Inverse`), then two rates through the base currency (`ViaBase`, the older date is reported). Two equal codes answer `Same` with rate 1 and no row. Nothing found, or only a stale rate, is `NoRate`: never an assumed 1 and never a silent old rate.

`Conversion { amount, from, to, quote: Quote { rate, rate_date, path, rate_ids } }`. Store the `quote` with the result; a later rate change rewrites nothing.

- The result is rounded to the target's decimal places, half away from zero, unless the request is `.unrounded()`.
- An amount with more places than its own currency allows is refused (`TooManyPlaces`).
- `Refusal::code()` is the agent-facing name: `unknown_currency`, `too_many_places`, `out_of_range`, `no_rate`, `no_base`; `message()` is a sentence to show.

## Access

| Capability | Gives |
| --- | --- |
| `admin.currency.view` | The pages, `catalog`, `rates` and the read operations |
| `admin.currency.manage` | Enable and disable currencies, choose the base currency, the `currency.rates.max_age_days` editor |
| `admin.currency.rates.manage` | Add, change and delete rates |

All three are tenant capabilities contributed to `tenant_owner`. `enabled`, `get`, `check`, `rate` and `convert` need only the sealed scope, since the calling module authorises its own operation. Every write rechecks its capability inside its transaction, and every statement carries the tenant predicate.

## Pages and agents

- `/currencies`: the base currency and per-company currencies (read first, chosen in place), then the list with an enable or disable action.
- `/currencies/rates`: the rates, edited in place, a dialog to add one, a confirm to delete one, and a conversion check that calls the same `convert` modules use. The date fields start as today in the reader's display zone; in browser-local display mode they start empty, because only the browser knows the day.
- Operations: `core.currency.list`, `.rates.list` and `.convert` (reads), and the reviewed writes `.enabled.set`, `.base.set`, `.rates.set` and `.rates.delete`. The pages call these same contracts (`PageWrites`).

## Runtime check

On a disposable database, run `migrate` and `seed-production` first, then:

```sh
BILIBI_CURRENCY_DEMO=disposable cargo run -p bilibi-currency --example currency-walkthrough --locked
```

It prints a person without the capability refused, enabling, the base and rates, each conversion path, rounding at 0 and 3 places, stale and missing rates, a second tenant keeping its own, and a disabled currency still reading but refused for a new record.
