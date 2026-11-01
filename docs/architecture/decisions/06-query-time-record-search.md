# ADR 06: Record search at query time, in the owner's source

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** How Bilibi finds records across mounted modules: the search box, the results page and the agent operation
**Last Updated:** 2026-10-22

> Source: [issue 130](https://github.com/BelimbingApp/bilibi/issues/130). There is no prior art to cite.

## Context

**People and agents need to find a record without knowing which list holds it.** A CRM agent looks up a caller by name, phone or reference while the call is live. Each list already searches its own rows its own way, and the top bar's search finds pages only.

What a search must not do:

- Show a hit, or a count, the person could not open: another tenant's record, a kind they may not read, a soft-deleted row.
- Answer whether a value exists through a field an operator restricted for the person (an existence oracle).
- Lag behind writes, or need a second copy of every record kept in step.

## Decision

**Search runs at query time inside each owner's Grid source, on PostgreSQL's built-in full-text search. There is no copied index.**

| Part | Where | What |
| --- | --- | --- |
| Searchable | The owning module | One `Searchable` contribution naming its Grid table, title, subtitles, record page and state |
| Vector | The owner's migration | A stored generated `tsvector` (`simple` configuration) built only from fields operators cannot restrict, with a GIN index, selected by the owner's Grid source as a hidden field |
| Query | `base/search` | A fresh Grid catalog for the scope; one read-only statement whose branches match and rank inside each kind's source, merged by rank and grouped by kind, under Grid's timeouts and cost check |
| Callers | Top bar, `/search`, agents | One contract, `base.search.records`, over one facade; one capability, `base.search.records.list` |

- **Trimming by construction.** The catalog keeps only the tables the reader may read and drops restricted fields; each branch binds the scope's tenant and user into the owner's source, so its tenant predicate, soft delete and company filters apply in the same statement as the match. A kind is also offered only when its record page admits the reader.
- **No oracle.** The vector holds only fields no operator can restrict. A restrictable field (a personal email) may be a prefix field, matched only for a reader no held role restricts.
- **Zero lag.** The vector is a generated column of the owner's row, so it changes in the same transaction as the record. There is no indexing pipeline, queue or reindex job.
- **Bound input.** Words are letters and digits, bound as parameters; user text never becomes `tsquery` syntax. 200 characters and 8 words count.
- **One page, many kinds.** The search box shows the best few of each kind inside the existing top-bar search dialog, beside pages; Enter opens the highlighted record.

## Why not a separate engine

Bilibi is PostgreSQL-only. An external engine would hold a second copy of every searchable record, which must be kept in step on every write, trimmed again at query time for tenant, grants and restrictions, and installed and backed up beside the database. A GIN-indexed `tsvector` serves the record volumes Bilibi targets with no lag, and every rule that guards a list applies to search unchanged.

## Consequences

- A Domain makes a table searchable with one contribution and one migration; no host, Menu or template change.
- Each searchable table carries a generated column and a GIN index; writes pay for keeping them.
- The GIN index does not include the tenant, so a very common prefix is matched across tenants before the tenant predicate filters it. The cost check refuses a search too broad to rank and asks for another word.
- Typo tolerance, matching inside a word and scripts written without spaces need `pg_trgm` or another extension, which would be the Platform's first; each needs its own decision.
- If an adopter outgrows PostgreSQL search, a later port can let an Extension provide another engine behind the same contribution.

## Status

Built in `apps/base/search` (`README.md` is the contract), with Core Person and Core Company contributing.
