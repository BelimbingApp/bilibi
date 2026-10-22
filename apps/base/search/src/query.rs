//! The one bounded, read-only statement behind every search. Each searched
//! kind is a branch that matches and ranks inside its owner's Grid source, so
//! the tenant predicate, soft delete and company filters apply in the same
//! statement as the match; the branches are merged by rank. Only catalog
//! identifiers and boot-checked declaration values enter the SQL; what a person
//! typed is bound, as a `tsquery` built from its letters and digits alone.
use crate::model::Kind;
use bilibi_grid::Table;

/// The longest query read, in characters; the rest is ignored.
pub const MAX_TEXT: usize = 200;
/// The most words a query matches on; later words are ignored.
pub const MAX_WORDS: usize = 8;
/// The most hits one kind ranks for a results page; its count stops there.
pub const MAX_KIND_HITS: i64 = 500;
/// Above this planner estimate a search is not run: too many records match
/// to rank within the bounds, and the person is asked for another word.
pub const MAX_ESTIMATED_COST: f64 = 100_000.0;
/// Every search: bounded locally, on the UTC calendar, as Grid's reads are.
pub const BOUNDS: &str = "SET LOCAL statement_timeout = '2000ms'; SET LOCAL lock_timeout = '500ms'; SET LOCAL TimeZone = 'UTC'";

/// What a person typed, made safe to search with.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Terms {
    /// The text as searched: trimmed, at most [`MAX_TEXT`] characters.
    pub text: String,
    /// The `tsquery` for `to_tsquery('simple', …)`: each plain word as a
    /// prefix, a quoted phrase in order, a `-word` excluded, all required.
    /// Every lexeme is letters and digits only. `None` when no word asks for
    /// a match (nothing typed, or only exclusions).
    pub query: Option<String>,
    /// A `LIKE` pattern for the prefix fields when the text is a plain
    /// lookup (no phrase, no exclusion): the whole text, lower-cased.
    pub prefix: Option<String>,
}

/// Reads a query. Words are runs of letters and digits, lower-cased, so
/// `%`, quotes, `&|!:*` and the like never reach the `tsquery` as syntax: a
/// hostile query matches literally or not at all, and never errors.
pub fn terms(text: &str) -> Terms {
    let text: String = text.trim().chars().take(MAX_TEXT).collect();
    let text = text.trim_end().to_owned();
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let mut plain = true;
    let mut budget = MAX_WORDS;
    let mut rest = text.as_str();
    while budget > 0 {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let negated = rest.starts_with('-');
        if negated {
            rest = &rest[1..];
            plain = false;
        }
        let (part, quoted) = match rest.strip_prefix('"') {
            Some(inner) => {
                let end = inner.find('"').unwrap_or(inner.len());
                rest = inner.get(end + 1..).unwrap_or_default();
                plain = false;
                (&inner[..end], true)
            }
            None => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let part = &rest[..end];
                rest = &rest[end..];
                (part, false)
            }
        };
        let words: Vec<String> = words(part).into_iter().take(budget).collect();
        if words.is_empty() {
            continue;
        }
        budget -= words.len();
        if negated {
            negative.push(format!("!{}", phrase(&words)));
        } else if quoted {
            positive.push(phrase(&words));
        } else {
            positive.extend(words.iter().map(|word| format!("'{word}':*")));
        }
    }
    let query = (!positive.is_empty()).then(|| {
        positive
            .into_iter()
            .chain(negative)
            .collect::<Vec<_>>()
            .join(" & ")
    });
    let prefix = (plain && query.is_some()).then(|| format!("{}%", like(&text.to_lowercase())));
    Terms {
        text,
        query,
        prefix,
    }
}

fn words(part: &str) -> Vec<String> {
    part.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Words that must stand next to each other, in order; one word is exact.
fn phrase(words: &[String]) -> String {
    match words {
        [word] => format!("'{word}'"),
        _ => format!(
            "({})",
            words
                .iter()
                .map(|word| format!("'{word}'"))
                .collect::<Vec<_>>()
                .join(" <-> ")
        ),
    }
}

/// `text` as a literal `LIKE` prefix: the escape character, `%` and `_`
/// match themselves.
fn like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// One kind as this reader searches it: its declaration, its Grid table as the
/// reader's catalog keeps it (restricted fields removed) and its boost.
pub struct Planned<'a> {
    pub kind: &'static Kind,
    pub table: &'a Table,
    pub boost: f64,
}

// Quote catalog identifiers even though declaration checks keep them to
// lowercase ASCII, as Grid's statements do.
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn column(table: &Table, id: &str) -> Option<String> {
    table
        .field(id)
        .map(|field| format!("r.{}", ident(field.source_key())))
}
fn text_of(table: &Table, id: Option<&str>) -> String {
    id.and_then(|id| column(table, id))
        .map_or_else(|| "NULL::text".to_owned(), |c| format!("({c})::text"))
}
fn time_of(table: &Table, id: Option<&str>) -> String {
    id.and_then(|id| column(table, id)).map_or_else(
        || "NULL::timestamptz".to_owned(),
        |c| format!("({c})::timestamptz"),
    )
}
// Boot-checked declaration values only: lowercase letters, digits, `_`, `-`.
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// One kind's branch: its matches inside the owner's source, best first, at
/// most `limit`. A field the reader's catalog left out (restricted) is never
/// selected or matched: a subtitle shows nothing and a prefix field takes no
/// part, so it cannot answer whether a value exists.
fn branch(planned: &Planned<'_>, limit: i64, prefix: bool) -> String {
    let Planned { kind, table, boost } = planned;
    let decl = kind.decl;
    let key = column(table, table.key()).expect("grid keeps the key field");
    let vector = column(table, decl.vector).expect("checked hidden vector field");
    let query = "to_tsquery('simple', $3::text)";
    let (state, inactive) = match decl
        .state
        .and_then(|state| Some((state, column(table, state.field)?)))
    {
        Some((state, c)) => (
            format!("({c})::text"),
            format!(
                "COALESCE(({c})::text IN ({}), false)",
                state
                    .inactive
                    .iter()
                    .map(|value| literal(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        None => ("NULL::text".to_owned(), "false".to_owned()),
    };
    let mut matched = format!("{vector} @@ {query}");
    if prefix {
        for c in decl.prefix.iter().filter_map(|id| column(table, id)) {
            matched.push_str(&format!(" OR lower(({c})::text) LIKE $4::text"));
        }
    }
    let at = time_of(table, table.time_field());
    // A record never updated was last changed when it was made.
    let updated = format!("COALESCE({}, {at})", time_of(table, decl.updated));
    format!(
        "(SELECT {kind}::text AS kind, ({key})::text AS id, {title} AS title, {sub0} AS sub0, {sub1} AS sub1, {state} AS state, {inactive} AS inactive, (ts_rank_cd({vector}, {query}) * {boost:.4})::float8 AS rank, {at} AS at, {updated} AS updated FROM ({source}) r WHERE {matched} ORDER BY inactive, rank DESC, at DESC NULLS LAST, title, id LIMIT {limit})",
        kind = literal(kind.id),
        title = text_of(table, Some(decl.title)),
        sub0 = text_of(table, decl.subtitle.first().copied()),
        sub1 = text_of(table, decl.subtitle.get(1).copied()),
        source = table.source().sql,
    )
}

// Binds the reader as each owner's source expects ($1 tenant, $2 user), the
// tsquery ($3) and the prefix pattern ($4), which every statement references.
fn hits(planned: &[Planned<'_>], limit: i64, prefix: bool) -> String {
    let branches = planned
        .iter()
        .map(|kind| branch(kind, limit, prefix))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    format!("WITH input AS (SELECT $1::bigint AS tenant, $2::bigint AS usr, $3::text AS words, $4::text AS prefix), hits AS ({branches})")
}

// Equal ranks read newest first, then by name, as a person scans them.
const ORDER: &str = "ORDER BY inactive, rank DESC, at DESC NULLS LAST, title, kind, id";
const COLUMNS: &str = "kind, id, title, sub0, sub1, state, inactive, updated";

/// The search box: the best `per_kind` hits of each kind, merged by rank.
pub fn suggestions(planned: &[Planned<'_>], per_kind: i64, prefix: bool) -> String {
    format!(
        "{} SELECT NULL::jsonb AS counts, {COLUMNS} FROM hits {ORDER}",
        hits(planned, per_kind, prefix)
    )
}

/// A results page: how many records each kind matched (up to
/// [`MAX_KIND_HITS`]) and one page of the merged ranking, of the kind `$5`
/// names or of every kind when it is empty, `$6` rows from `$7`. One row
/// carries the counts even when the page is empty.
pub fn page(planned: &[Planned<'_>], prefix: bool) -> String {
    format!(
        "{}, counted AS (SELECT kind, count(*) AS n FROM hits GROUP BY kind) SELECT (SELECT COALESCE(jsonb_object_agg(kind, n), '{{}}'::jsonb) FROM counted) AS counts, {} FROM (SELECT 1) one LEFT JOIN LATERAL (SELECT * FROM hits WHERE ($5::text = '' OR kind = $5::text) {ORDER} LIMIT $6::bigint OFFSET $7::bigint) page ON true",
        hits(planned, MAX_KIND_HITS, prefix),
        COLUMNS
            .split(", ")
            .map(|name| format!("page.{name}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
