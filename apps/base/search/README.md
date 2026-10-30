# Search

One search over the records of every mounted module, trimmed to what the person may open. The search box in the top bar, the `/search` page and agents all search through the operation `base.search.records` (ADR 01), so they find the same records. Bilimbi has no cross-module record search, so this module mirrors none. The decision is [ADR 06](../../../docs/architecture/decisions/06-query-time-record-search.md).

## Make a table searchable

One contribution and one migration in the module that owns the Grid table. The host, Menu and templates do not change.

1. **Migration.** Add a stored generated `tsvector` column built only from fields operators cannot restrict (the key, the label, `.protected()` fields), with the `simple` configuration, and a GIN index:

   ```sql
   ALTER TABLE companies ADD COLUMN search_vector tsvector GENERATED ALWAYS AS (
       setweight(to_tsvector('simple', name), 'A')
       || setweight(to_tsvector('simple', coalesce(legal_name, '')), 'B')) STORED;
   CREATE INDEX companies_search ON companies USING gin (search_vector);
   ```

2. **Grid.** Select the column in the table's Grid `Source` and declare it as a hidden field: `Field::new("search_vector", "Search terms", FieldType::String).hidden()`.
3. **Contribution.** Depend on `base/search` (Cargo and descriptor) and submit one `Searchable`:

   ```rust
   static SEARCH: bilibi_search::Searchable =
       bilibi_search::Searchable::new("companies", "name", "search_vector", "/companies/{id}")
           .subtitle(&["code", "legal_name"])
           .state("status", &["archived"])
           .updated("updated_at");
   bilibi_search::contribute!("core/company", "company.search", &SEARCH);
   ```

| Part | Rule |
| --- | --- |
| `table` | A Grid table the contributing module owns; its id is the kind's id, its label the kind's name |
| `title` | Shown and protected (the label field usually) |
| `subtitle` | Up to two fields; one the reader's role restricts is left out, one it masks shows only its partial value |
| `vector` | The hidden Grid field holding the `tsvector` |
| `prefix` | Up to four text fields that also match when they start with the query (a personal email or phone). A restrictable one takes part only for a reader no held role restricts or masks, as the Contacts search does |
| `link` | A GET page route of the record, with `{id}` for the key. A hit is offered only to a reader that route admits |
| `state` | A protected field and the values that mean "no longer current". Such records stay findable, ranked after current ones and marked |
| `updated` | A date or time the results page shows |
| `.off()` | Not searched until an operator turns the kind on |

Discovery refuses a malformed declaration and a second one for the same table. Boot refuses an unknown table, a table of another module, a field the table does not declare or of the wrong kind, and a link that is no page route or is public.

## What a search does

- **Who may search.** Capability `base.search.records.list`, which Authz's built-in roles hold (Core Administrator holds everything). A tenant grants it to its own roles. The search box shows records only to a person holding it; it searches pages for everyone.
- **What is searched.** A fresh Grid catalog for the scope keeps the tables the reader may read; a kind also needs its record page to admit the reader, the operator's settings to have it on and the person not to have hidden it. A direct deny or a revoked role takes a kind away on the next keystroke. There is no cache of grants.
- **One statement.** Each kind matches and ranks inside its owner's Grid source with the scope's tenant and user bound, so tenant, soft delete and company filters apply with the match. The branches merge by rank, then group by kind (see Ranking). The read is read-only and repeatable-read, with Grid's statement and lock timeouts and an estimated-cost check before it runs.
- **Matching.** The first 200 characters and 8 words count. Words are runs of letters and digits, lower-cased and bound; what a person typed never becomes `tsquery` syntax. Each word matches the start of a word (`acm` finds Acme) and every word must match; `"a phrase"` matches in order and `-word` leaves records with it out. `%`, quotes and `&|!:*` are literal and never error.
- **Ranking.** `ts_rank_cd` (the title weighs most, as the owner's `setweight` says) times the kind's boost, then newer first by the table's time field; records that are no longer current come after current ones. The search box and results pages keep each kind's hits together, the kinds in the order of their best hit; on a results page a kind's group continues on the next page.
- **Fresh.** The vector is a generated column of the owner's row, so a record is findable as soon as its write commits. There is no index to keep in step.
- **Bounded.** A query that matches too many records to rank within the bounds returns no hits and asks for another word. Results pages count up to 500 matches per kind ("500+").
- **Private.** Query text is not stored or logged, and audit records no search event. Each search logs two capability decisions: the contract's, then the owner's own.

## Callers

| Caller | Where | What it shows |
| --- | --- | --- |
| Search box | Shell embed `shell.search.records`, rendered into the top bar's search dialog between Pages and Account (`bilibi_ui::SearchRecords`); calls `base.search.records` with `mode: suggest` | The best few records of each kind once the query is long enough; Enter or a click opens one; "All results" opens `/search` |
| Results page | `/search?q=…&kind=…` (`FlexTable` on `ListState`); calls `base.search.records` | Every hit, grouped by kind as the search box groups them, with a kind filter counted per kind; the address is shareable |
| Own settings | `/settings/search`, the Search tab of My Settings | A tick per kind the person may search, to keep it in or out of their results |
| Agents | Operation `base.search.records`, guide `base.search` | The same page of hits: kind, id, title, detail, state, link, updated, and the counts; `mode: suggest` gives the search box's few of each kind. Read a hit through its kind's own operations |

## Settings

| Key | Scope | Default | Meaning |
| --- | --- | --- | --- |
| `search.kinds` | Global, tenant, company | `{}` | Kinds on or off over each kind's default: `{"companies": false}` |
| `search.boost` | Global, tenant, company | `{}` | A kind's ranking multiplier, 0.1 to 10: `{"persons": 2}` |
| `search.min_chars` | Global, tenant, company | 2 | Characters before the search box looks for records |
| `search.suggestions` | Global, tenant, company | 5 | Records of each kind the search box shows |
| `search.hidden_kinds` | User | `[]` | Kinds the person keeps out of their own results (their Search tab) |

The operator edits the first four on System Settings, for every tenant (Global), for its own tenant (This tenant) or for one company; the person edits the last on My Settings. A search reads them for the person's company, falling back to the tenant's when the company directory refuses it.

## Not in this phase

Typo tolerance and matching inside a word (`pg_trgm`), scripts written without spaces, stemming per language, synonyms, highlighted snippets, facets beyond kind, saved searches and alerts, usage reports, semantic search and any external engine. Advanced list filters belong to `ListState`.

## Runtime checks

No unit tests. On a disposable database with the walkthrough fixture, signed in at the exact `APP_ORIGIN`:

| Case | Expected |
| --- | --- |
| Type `acm` in the top bar's search | Companies whose name, code or legal name has a word starting with "acm", under Companies; matching pages above |
| A contact's email start, as a reader whose role restricts email | No contact by email; with the restriction lifted, the contact |
| The same, as a reader whose role masks email | No contact by email; by name, the contact with its email partial (`p•••@example.test`), as `base/privacy`'s `failures-walkthrough` shows |
| Revoke a kind's list capability, type again | The kind's group and count are gone without signing in again |
| Rename a record, search the new name | Found at once |
| `/search` for a word several kinds match | Each kind's hits together, best first; the kinds in the order of their best hit, as in the search box |
| Type a query, change it and press Enter before the answer comes | The earlier query's records are gone at once; Enter opens nothing of them |
| `%`, `'"`, `&|!:*`, 1,000 characters | Results or none, never an error |
| Turn a kind off (`search.kinds`), hide another (Search tab) | Both leave the box, `/search` and `base.search.records` |
| `EXPLAIN` of the statement | The GIN index of each kind is used |
