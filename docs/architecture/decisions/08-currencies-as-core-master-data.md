# ADR 08: Currencies as Core master data, and conversion always names its rate

**Document Type:** Architecture Decision Record
**Status:** Accepted; implemented
**Agents:** claude-code/sonnet-5.5-default (author)
**Scope:** Where the currency list, a tenant's base currency and exchange rates live (`core/currency`)
**Last Updated:** 2026-10-28

> Source: [issue 140](https://github.com/BelimbingProject/bilibi/issues/140).

## Context

**Finance, CRM and Factory all need currencies, and a Domain may never depend on another Domain.** Finance kept a free-text three-letter code with places picked by the person. A list kept inside Finance could not be shared, and a conversion without a stated rate cannot be audited.

## Decision

1. **Currencies are Core master data**, in `core/currency`: the ISO 4217 list ships with the Platform (unlike holidays, the standard is stable and authoritative), each tenant enables the codes it trades in, and Domains read them through one facade.
2. **A conversion always names the rate it used**: the rate, its date and how it was found (direct, inverse, or through the base currency). The caller stores that with its result, so a later rate change rewrites nothing.
3. **No rate is a refusal.** Never an assumed rate of 1 and never a silent old rate; a rate older than the tenant's limit is not used.
4. **Operators keep the rates by hand.** There is no bundled feed and no fetcher. An Extension may write rates through the same reviewed operation.

## Rejected

- **A currency list inside each Domain:** no sharing across Domains.
- **Taking the list from Geonames:** it holds one currency per country, no decimal places, and is empty until imported.
- **Tenants adding codes or changing decimal places:** it would break the standard; they enable and disable.
- **Bundled rate feeds:** an outbound dependency and a source of silent staleness.

## Consequences

- Finance, CRM and Factory adopt `Currencies` in their own changes; Finance's book currency moving onto it is a follow-up in that Domain.
- Rate types (daily, month-end, budget), spreads, automatic feeds and installation-wide shared rates are out of scope for now: one rate per pair and date.

The module's [README](../../../apps/core/currency/README.md) is its contract.
