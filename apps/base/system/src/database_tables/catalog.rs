//! Read-only PostgreSQL catalog reads for the database-tables pages. Every
//! relation and column a statement names is first resolved through
//! `pg_catalog` in the application's schema and quoted from the catalog's own
//! spelling; request text only ever binds as a parameter. Row reads run in a
//! `READ ONLY` transaction under a statement timeout and never return more
//! than one page or more than [`CELL_CHARS`] characters of any value.
use super::sensitive::Mask;
use bilibi_database::{Error, Result};
use tokio_postgres::{types::ToSql, GenericClient};

/// What a data cell carries at most; a longer value is shown cut.
pub(super) const CELL_CHARS: i32 = 120;
/// Bounds every statement of a row read, counting included.
const STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '5s'";

/// One base or partitioned table of the application schema.
pub(super) struct TableEntry {
    pub name: String,
    pub estimated_rows: i64,
    pub partitioned: bool,
    pub column_count: i64,
}

/// A table the catalog confirmed by name; the only way to name one in SQL.
pub(super) struct Table {
    oid: u32,
    schema: String,
    pub name: String,
}
impl Table {
    fn qualified(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.name))
    }
}

pub(super) struct Column {
    pub number: i16,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub identity: Option<&'static str>,
    pub generated: bool,
    /// A string type: what the Data tab's search reads.
    pub text: bool,
    /// The type has a default btree ordering, so the Data tab may sort by it.
    pub orderable: bool,
    pub boolean: bool,
}
impl Column {
    pub fn auto_increment(&self) -> bool {
        self.identity.is_some()
            || self
                .default
                .as_deref()
                .is_some_and(|default| default.starts_with("nextval("))
    }
}

pub(super) struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub method: String,
    pub primary: bool,
    pub unique: bool,
    pub definition: String,
}

pub(super) struct Constraint {
    pub name: String,
    pub kind: &'static str,
    pub definition: String,
}

/// One column pair of a foreign key. Outgoing: `column` here references
/// `other_table.other_column`. Incoming: `other_table.other_column`
/// references `column` here.
pub(super) struct Reference {
    pub constraint: String,
    pub column: String,
    pub other_table: String,
    pub other_column: String,
    /// The other table is in the application schema, so the pages can link it.
    pub linkable: bool,
    pub on_delete: &'static str,
}

pub(super) async fn tables(client: &impl GenericClient) -> Result<Vec<TableEntry>> {
    Ok(client
        .query(
            "SELECT c.relname::text, greatest(c.reltuples, 0)::bigint, c.relkind = 'p', \
             (SELECT count(*) FROM pg_attribute a WHERE a.attrelid=c.oid AND a.attnum > 0 AND NOT a.attisdropped) \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p') \
             ORDER BY c.relname",
            &[],
        )
        .await?
        .iter()
        .map(|row| TableEntry {
            name: row.get(0),
            estimated_rows: row.get(1),
            partitioned: row.get(2),
            column_count: row.get(3),
        })
        .collect())
}

/// The table of that exact name in the application schema, or `NotFound`.
pub(super) async fn table(client: &impl GenericClient, name: &str) -> Result<Table> {
    if name.is_empty() || name.len() > 63 {
        return Err(Error::NotFound);
    }
    let row = client
        .query_opt(
            "SELECT c.oid, n.nspname::text, c.relname::text \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p') AND c.relname = $1",
            &[&name],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Ok(Table {
        oid: row.get(0),
        schema: row.get(1),
        name: row.get(2),
    })
}

pub(super) async fn columns(client: &impl GenericClient, table: &Table) -> Result<Vec<Column>> {
    Ok(client
        .query(
            "SELECT a.attnum, a.attname::text, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
              pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated <> '', \
              b.typcategory = 'S', b.oid = 'boolean'::regtype, \
              b.typcategory <> 'A' AND EXISTS (SELECT 1 FROM pg_opclass oc JOIN pg_am am ON am.oid = oc.opcmethod \
                WHERE am.amname = 'btree' AND oc.opcdefault \
                AND (oc.opcintype = b.oid OR (b.typtype = 'e' AND oc.opcintype = 'anyenum'::regtype))) \
             FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid \
             JOIN pg_type b ON b.oid = CASE WHEN t.typtype = 'd' THEN t.typbasetype ELSE t.oid END \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            &[&table.oid],
        )
        .await?
        .iter()
        .map(|row| Column {
            number: row.get(0),
            name: row.get(1),
            data_type: row.get(2),
            nullable: row.get(3),
            default: row.get(4),
            identity: match row.get::<_, String>(5).as_str() {
                "a" => Some("always"),
                "d" => Some("by default"),
                _ => None,
            },
            generated: row.get(6),
            text: row.get(7),
            boolean: row.get(8),
            orderable: row.get(9),
        })
        .collect())
}

pub(super) async fn indexes(client: &impl GenericClient, table: &Table) -> Result<Vec<Index>> {
    Ok(client
        .query(
            "SELECT i.relname::text, \
              ARRAY(SELECT coalesce(a.attname::text, pg_get_indexdef(x.indexrelid, k.n::int, true)) \
                FROM unnest(x.indkey::int2[]) WITH ORDINALITY AS k(attnum, n) \
                LEFT JOIN pg_attribute a ON a.attrelid = x.indrelid AND a.attnum = k.attnum AND k.attnum > 0 \
                ORDER BY k.n), \
              am.amname::text, x.indisprimary, x.indisunique, pg_get_indexdef(x.indexrelid) \
             FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid JOIN pg_am am ON am.oid = i.relam \
             WHERE x.indrelid = $1 ORDER BY x.indisprimary DESC, i.relname",
            &[&table.oid],
        )
        .await?
        .iter()
        .map(|row| Index {
            name: row.get(0),
            columns: row.get(1),
            method: row.get(2),
            primary: row.get(3),
            unique: row.get(4),
            definition: row.get(5),
        })
        .collect())
}

pub(super) async fn constraints(
    client: &impl GenericClient,
    table: &Table,
) -> Result<Vec<Constraint>> {
    Ok(client
        .query(
            "SELECT conname::text, contype::text, pg_get_constraintdef(oid, true) \
             FROM pg_constraint WHERE conrelid = $1 \
             ORDER BY array_position(ARRAY['p','u','f','c','x','n','t'], contype::text), conname",
            &[&table.oid],
        )
        .await?
        .iter()
        .map(|row| Constraint {
            name: row.get(0),
            kind: match row.get::<_, String>(1).as_str() {
                "p" => "Primary key",
                "u" => "Unique",
                "f" => "Foreign key",
                "c" => "Check",
                "x" => "Exclusion",
                "n" => "Not null",
                "t" => "Trigger",
                _ => "Other",
            },
            definition: row.get(2),
        })
        .collect())
}

/// Foreign keys from this table (`outgoing`) and onto it (`incoming`), one
/// entry per column pair.
pub(super) async fn references(
    client: &impl GenericClient,
    table: &Table,
    outgoing: bool,
) -> Result<Vec<Reference>> {
    // Outgoing reads conkey here and confkey there; incoming the reverse.
    let sql = if outgoing {
        "SELECT con.conname::text, a.attname::text, oc.relname::text, oa.attname::text, \
          on_.nspname = current_schema(), con.confdeltype::text \
         FROM pg_constraint con \
         CROSS JOIN LATERAL unnest(con.conkey, con.confkey) WITH ORDINALITY AS k(here, there, n) \
         JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.here \
         JOIN pg_class oc ON oc.oid = con.confrelid JOIN pg_namespace on_ ON on_.oid = oc.relnamespace \
         JOIN pg_attribute oa ON oa.attrelid = con.confrelid AND oa.attnum = k.there \
         WHERE con.contype = 'f' AND con.conrelid = $1 AND con.conparentid = 0 \
         ORDER BY con.conname, k.n"
    } else {
        "SELECT con.conname::text, a.attname::text, oc.relname::text, oa.attname::text, \
          on_.nspname = current_schema(), con.confdeltype::text \
         FROM pg_constraint con \
         CROSS JOIN LATERAL unnest(con.confkey, con.conkey) WITH ORDINALITY AS k(here, there, n) \
         JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.here \
         JOIN pg_class oc ON oc.oid = con.conrelid JOIN pg_namespace on_ ON on_.oid = oc.relnamespace \
         JOIN pg_attribute oa ON oa.attrelid = con.conrelid AND oa.attnum = k.there \
         WHERE con.contype = 'f' AND con.confrelid = $1 AND con.conparentid = 0 \
         ORDER BY oc.relname, con.conname, k.n"
    };
    Ok(client
        .query(sql, &[&table.oid])
        .await?
        .iter()
        .map(|row| Reference {
            constraint: row.get(0),
            column: row.get(1),
            other_table: row.get(2),
            other_column: row.get(3),
            linkable: row.get(4),
            on_delete: match row.get::<_, String>(5).as_str() {
                "c" => "cascade",
                "n" => "set null",
                "d" => "set default",
                "r" => "restrict",
                _ => "no action",
            },
        })
        .collect())
}

/// One column as a row read projects it.
pub(super) struct Projected<'a> {
    pub column: &'a Column,
    pub mask: Mask,
}

/// What the Data tab asked for, already bounded by its list state.
pub(super) struct RowQuery<'a> {
    pub search: &'a str,
    /// An exact match on one column's text form (a followed reference).
    pub equals: Option<(&'a Column, &'a str)>,
    pub order: Option<(&'a Column, bool)>,
    /// Ties and unordered reads follow these, so paging is stable.
    pub key: Vec<&'a Column>,
    pub limit: i64,
    pub offset: i64,
}

/// One read cell: a masked value is never read, only whether it is set.
pub(super) enum Value {
    Null,
    Masked,
    Text { text: String, cut: bool },
}

pub(super) struct Rows {
    pub rows: Vec<Vec<Value>>,
    /// `None` when counting ran past the statement timeout.
    pub total: Option<i64>,
}

pub(super) async fn rows(
    client: &mut deadpool_postgres::Object,
    table: &Table,
    columns: &[Projected<'_>],
    query: &RowQuery<'_>,
) -> Result<Rows> {
    let transaction = client.build_transaction().read_only(true).start().await?;
    transaction.batch_execute(STATEMENT_TIMEOUT).await?;
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
    let mut filters = Vec::new();
    let search = query.search.trim();
    if !search.is_empty() {
        let searched: Vec<_> = columns
            .iter()
            .filter(|projected| projected.column.text && projected.mask == Mask::Clear)
            .map(|projected| format!("{} ILIKE $1", quote(&projected.column.name)))
            .collect();
        params.push(Box::new(format!("%{}%", escape_like(search))));
        filters.push(if searched.is_empty() {
            "false".to_owned()
        } else {
            format!("({})", searched.join(" OR "))
        });
    }
    if let Some((column, value)) = query.equals {
        params.push(Box::new(value.to_owned()));
        filters.push(format!("{}::text = ${}", quote(&column.name), params.len()));
    }
    let filter = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    let bound: Vec<&(dyn ToSql + Sync)> = params
        .iter()
        .map(|param| &**param as &(dyn ToSql + Sync))
        .collect();
    let total = match transaction
        .query_one(
            &format!("SELECT count(*) FROM {}{filter}", table.qualified()),
            &bound,
        )
        .await
    {
        Ok(row) => Some(row.get::<_, i64>(0)),
        // A count past the timeout aborts the transaction; read the page
        // on a fresh one rather than give up the rows.
        Err(error) if error.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED) => {
            None
        }
        Err(error) => return Err(error.into()),
    };
    let transaction = if total.is_none() {
        transaction.rollback().await?;
        let fresh = client.build_transaction().read_only(true).start().await?;
        fresh.batch_execute(STATEMENT_TIMEOUT).await?;
        fresh
    } else {
        transaction
    };
    let mut projection = Vec::new();
    for projected in columns {
        let name = quote(&projected.column.name);
        let value = format!("left({name}::text, {})", CELL_CHARS + 1);
        let (value, masked) = match projected.mask {
            Mask::Clear => (value, "false".to_owned()),
            Mask::Always => ("NULL::text".to_owned(), format!("{name} IS NOT NULL")),
            Mask::WhenTrue(flag) => {
                let flag = quote(flag);
                (
                    format!("CASE WHEN {flag} IS TRUE THEN NULL ELSE {value} END"),
                    format!("{flag} IS TRUE AND {name} IS NOT NULL"),
                )
            }
        };
        projection.push(value);
        projection.push(masked);
    }
    let mut order: Vec<String> = Vec::new();
    if let Some((column, descending)) = query.order {
        order.push(format!(
            "{} {} NULLS LAST",
            quote(&column.name),
            if descending { "DESC" } else { "ASC" }
        ));
    }
    order.extend(
        query
            .key
            .iter()
            .map(|column| format!("{} ASC", quote(&column.name))),
    );
    if query.key.is_empty() {
        order.push("ctid".into());
    }
    let limit = params.len() + 1;
    let sql = format!(
        "SELECT {} FROM {}{filter} ORDER BY {} LIMIT ${limit} OFFSET ${}",
        if projection.is_empty() {
            "1".to_owned()
        } else {
            projection.join(", ")
        },
        table.qualified(),
        order.join(", "),
        limit + 1,
    );
    let mut bound = bound;
    bound.push(&query.limit);
    bound.push(&query.offset);
    let found = transaction.query(&sql, &bound).await?;
    transaction.rollback().await?;
    let rows = found
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|index| {
                    if row.get::<_, bool>(index * 2 + 1) {
                        return Value::Masked;
                    }
                    match row.get::<_, Option<String>>(index * 2) {
                        None => Value::Null,
                        Some(text) => {
                            let cut = text.chars().count() > CELL_CHARS as usize;
                            Value::Text {
                                text: if cut {
                                    text.chars().take(CELL_CHARS as usize).collect()
                                } else {
                                    text
                                },
                                cut,
                            }
                        }
                    }
                })
                .collect()
        })
        .collect();
    Ok(Rows { rows, total })
}

/// A double-quoted identifier, from a name the catalog returned.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
