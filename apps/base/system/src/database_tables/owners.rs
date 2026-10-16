//! Which module owns a table: Bilibi's counterpart of Belimbing's table
//! registry, read from the migrations the mounted modules declare rather than
//! kept in a table. The first migration, in dependency order, whose SQL
//! creates the table owns it. A table no mounted migration creates (one an
//! unmounted Domain left, or one made outside migrations) has no owner.
use bilibi_module_registry::Module;
use std::collections::BTreeMap;

pub(super) struct Owner {
    pub module: &'static str,
    pub version: &'static str,
    pub sql: &'static str,
}

pub(super) struct Owners(BTreeMap<String, Owner>);
impl Owners {
    pub fn new(modules: &[&'static Module]) -> Self {
        let mut owners = BTreeMap::new();
        for module in modules {
            for (version, sql) in module.migrations {
                for table in created_tables(sql) {
                    owners.entry(table).or_insert(Owner {
                        module: module.id,
                        version,
                        sql,
                    });
                }
            }
        }
        Self(owners)
    }
    pub fn get(&self, table: &str) -> Option<&Owner> {
        self.0.get(table)
    }
    /// The migration ledger is the one table Database creates in code.
    pub fn module(&self, table: &str) -> Option<&'static str> {
        self.get(table)
            .map(|owner| owner.module)
            .or((table == "bilibi_migrations").then_some("base/database"))
    }
}

/// Names after `CREATE [UNLOGGED] TABLE [IF NOT EXISTS]`, unquoted and without
/// a schema, lower-cased as PostgreSQL folds them.
fn created_tables(sql: &str) -> Vec<String> {
    let words: Vec<&str> = sql.split_whitespace().collect();
    let mut found = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if !word.eq_ignore_ascii_case("create") {
            continue;
        }
        let mut next = index + 1;
        if words
            .get(next)
            .is_some_and(|word| word.eq_ignore_ascii_case("unlogged"))
        {
            next += 1;
        }
        if !words
            .get(next)
            .is_some_and(|word| word.eq_ignore_ascii_case("table"))
        {
            continue;
        }
        next += 1;
        if words.get(next..next + 3).is_some_and(|clause| {
            clause
                .iter()
                .map(|word| word.to_ascii_lowercase())
                .eq(["if", "not", "exists"])
        }) {
            next += 3;
        }
        let Some(name) = words.get(next) else {
            continue;
        };
        let name = name.split('(').next().unwrap_or_default();
        let name = name.rsplit('.').next().unwrap_or_default();
        let name = if name.starts_with('"') {
            name.trim_matches('"').to_owned()
        } else {
            name.to_ascii_lowercase()
        };
        if !name.is_empty() {
            found.push(name);
        }
    }
    found
}
