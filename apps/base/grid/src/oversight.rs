//! Child-granted fan-out over Grid, not an acting identity. The same SQL engine serves ordinary and oversight reads.
use crate::{
    Agg, Bounds, Catalog, Lenses, Line, PageSpec, Projection, QueryError, Summary, SummarySpec,
};
use bilibi_audit::{record_action, record_oversight_read, Attribution, OversightRead};
use bilibi_database::{Db, Error};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, Settings, ValueType, DEFINITIONS};
use bilibi_tenancy::{ChildGrant, Oversight, OversightScope, Scope};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    task::JoinSet,
    time::{timeout, timeout_at, Instant},
};

pub const OVERSIGHT_QUERIES: &str = "base/grid.oversight";

#[derive(Clone, Debug)]
pub enum OversightSpec {
    Summary(SummarySpec),
    /// Totals of a Rows report must exclude aggregates-only grants too.
    RowsSummary(SummarySpec),
    Rows {
        page: PageSpec,
        columns: Vec<String>,
        /// Per-child pages for a caller composing one globally bounded page.
        /// Missing entries use `page`; IDs never widen the proven directory.
        offsets: BTreeMap<i64, usize>,
    },
}
impl OversightSpec {
    fn rows(&self) -> bool {
        matches!(self, Self::Rows { .. } | Self::RowsSummary(_))
    }
    fn bounded(&self, millis: u32) -> Self {
        let mut spec = self.clone();
        let bounds = match &mut spec {
            Self::Summary(s) | Self::RowsSummary(s) => &mut s.bounds,
            Self::Rows { page, .. } => &mut page.bounds,
        };
        bounds.statement_ms = bounds.statement_ms.min(millis);
        bounds.max_cost = bounds.max_cost.min(crate::MAX_ESTIMATED_COST);
        spec
    }
    fn evidence(&self) -> (Vec<String>, Vec<String>) {
        match self {
            Self::Summary(s) | Self::RowsSummary(s) => (
                s.groupings
                    .iter()
                    .map(|g| g.column.clone())
                    .chain(s.measures.iter().map(|m| m.column.clone()))
                    .collect(),
                s.filters.iter().map(|f| f.column.clone()).collect(),
            ),
            Self::Rows { page, columns, .. } => (
                columns.clone(),
                page.filters.iter().map(|f| f.column.clone()).collect(),
            ),
        }
    }
    fn bounds(&self) -> Bounds {
        match self {
            Self::Summary(s) | Self::RowsSummary(s) => s.bounds,
            Self::Rows { page, .. } => page.bounds,
        }
    }
}
#[derive(Clone, Debug)]
pub enum OversightData {
    Summary(Summary),
    Rows { projection: Projection, total: i64 },
}
#[derive(Debug)]
pub struct ChildResult {
    pub tenant: i64,
    pub name: String,
    pub grant_version: i64,
    /// ok, timed_out, over_cost, revoked, refused. Never database messages.
    pub outcome: &'static str,
    pub data: Option<OversightData>,
}
#[derive(Debug)]
pub struct OversightResult {
    pub children: Vec<ChildResult>,
    /// Summary measures combined over included children, never averages of averages.
    pub total: Option<Line>,
}
impl OversightResult {
    pub fn excluded(&self) -> usize {
        self.children.iter().filter(|c| c.data.is_none()).count()
    }
}

/// The one cross-tenant read facade: bounded, read-only and audited before
/// returning. The bounded process cache is private to each reader and live
/// catalog/version; no rows are persisted in the parent tenant.
#[derive(Clone)]
pub struct OversightQueries {
    db: Db,
    settings: Arc<Settings>,
    tenancy: Arc<Oversight>,
    cache: Arc<Mutex<HashMap<String, (Instant, OversightData)>>>,
}
impl OversightQueries {
    pub fn new(db: Db, settings: Arc<Settings>, tenancy: Arc<Oversight>) -> Self {
        Self {
            db,
            settings,
            tenancy,
            cache: Arc::default(),
        }
    }
    pub async fn prove(&self, scope: &Scope) -> Result<OversightScope, QueryError> {
        let client = self.db.get().await.map_err(Error::from)?;
        Ok(self.tenancy.prove(&**client, scope).await?)
    }
    /// A builder receives a per-child catalog, not a union that might offer a
    /// field another child withheld. Callers can intersect these for one query.
    pub async fn catalog(
        &self,
        scope: &OversightScope,
        child: &ChildGrant,
        rows: bool,
    ) -> Result<Catalog, QueryError> {
        let client = self.db.get().await.map_err(Error::from)?;
        Ok(Catalog::for_oversight(&**client, scope, child, rows).await?)
    }
    /// A bounded intersection for builders; no results or child identities.
    pub async fn builder_catalog(&self, scope: &Scope, rows: bool) -> Result<Catalog, QueryError> {
        timeout(Duration::from_secs(60), async {
            let proof = self.prove(scope).await?;
            if proof.children().len() > self.setting("max_children").await? as usize {
                return Err(QueryError::Limit("too many consenting children"));
            }
            let millis = self.setting("total_timeout_ms").await?;
            timeout(Duration::from_millis(u64::from(millis)), async {
                let client = self.db.get().await.map_err(Error::from)?;
                Ok(Catalog::oversight_builder(&**client, &proof, rows).await?)
            })
            .await
            .map_err(|_| QueryError::TooHeavy("oversight catalog timed out"))?
        })
        .await
        .map_err(|_| QueryError::TooHeavy("oversight policy lookup timed out"))?
    }
    pub async fn read(
        &self,
        scope: &OversightScope,
        root: &str,
        spec: &OversightSpec,
    ) -> Result<OversightResult, QueryError> {
        // Bound even pool admission and settings lookup; an unavailable policy
        // cannot leave an unbounded request or return unaudited data.
        timeout(Duration::from_secs(60), async {
            let total_ms = self.setting("total_timeout_ms").await?;
            timeout(
                Duration::from_millis(u64::from(total_ms)),
                self.read_inner(scope, root, spec),
            )
            .await
            .map_err(|_| QueryError::TooHeavy("oversight total time bound exceeded"))?
        })
        .await
        .map_err(|_| QueryError::TooHeavy("oversight policy lookup timed out"))?
    }
    async fn read_inner(
        &self,
        scope: &OversightScope,
        root: &str,
        spec: &OversightSpec,
    ) -> Result<OversightResult, QueryError> {
        // Re-prove the directory too: reusing an old proof never caches grants.
        let scope = self.prove(scope.parent()).await?;
        let max_children = self.setting("max_children").await? as usize;
        let concurrency = self.setting("concurrency").await? as usize;
        let child_ms = self.setting("child_timeout_ms").await?;
        let total_ms = self.setting("total_timeout_ms").await?;
        let cache_seconds = self.setting("cache_seconds").await?;
        if scope.children().len() > max_children {
            return Err(QueryError::Limit(
                "too many consenting children; narrow the grants",
            ));
        }
        let spec = spec.bounded(child_ms);
        spec.bounds().check()?;
        let deadline = Instant::now() + Duration::from_millis(u64::from(total_ms));
        let mut results = BTreeMap::new();
        let mut pending = scope.children().iter().cloned();
        let mut tasks = JoinSet::new();
        loop {
            while tasks.len() < concurrency {
                let Some(grant) = pending.next() else {
                    break;
                };
                let facade = self.clone();
                let proof = scope.clone();
                let root = root.to_owned();
                let query = spec.clone();
                tasks.spawn(async move {
                    let result = timeout(
                        Duration::from_millis(u64::from(child_ms)),
                        facade.child(&proof, &grant, &root, &query, cache_seconds),
                    )
                    .await;
                    let (outcome, data) = match result {
                        Ok(Ok(data)) => ("ok", Some(data)),
                        Err(_) => ("timed_out", None),
                        Ok(Err(QueryError::Database(Error::Forbidden))) => ("revoked", None),
                        Ok(Err(QueryError::TooHeavy(why))) if why.contains("time") => {
                            ("timed_out", None)
                        }
                        Ok(Err(QueryError::TooHeavy(_))) => ("over_cost", None),
                        Ok(Err(_)) => ("refused", None),
                    };
                    ChildResult {
                        tenant: grant.tenant(),
                        name: grant.name().to_owned(),
                        grant_version: grant.version(),
                        outcome,
                        data,
                    }
                });
            }
            if tasks.is_empty() {
                break;
            }
            match timeout_at(deadline, tasks.join_next()).await {
                Ok(Some(Ok(child))) => {
                    results.insert(child.tenant, child);
                }
                Ok(Some(Err(_))) => return Err(Error::Internal.into()),
                Ok(None) => break,
                Err(_) => {
                    tasks.abort_all();
                    break;
                }
            }
        }
        for grant in scope.children() {
            results
                .entry(grant.tenant())
                .or_insert_with(|| ChildResult {
                    tenant: grant.tenant(),
                    name: grant.name().to_owned(),
                    grant_version: grant.version(),
                    outcome: "timed_out",
                    data: None,
                });
        }
        let mut result = OversightResult {
            children: results.into_values().collect(),
            total: None,
        };
        if let OversightSpec::Summary(spec) | OversightSpec::RowsSummary(spec) = &spec {
            result.total = Some(self.combine(&result.children, spec).await?);
        }
        let client = self.db.get().await.map_err(Error::from)?;
        let mut who = Attribution::from_scope(scope.parent());
        who.trace_id = scope.parent().agent().map(|agent| agent.trace().to_owned());
        record_action(&**client, &who, None, "tenancy.oversight.query", json!({"table":root,"reader_name":scope.parent().user_name,"agent_connection":scope.parent().agent().map(|agent|agent.connection()),"row_count":result.children.iter().filter_map(|c|c.data.as_ref()).map(|d|match d { OversightData::Summary(s)=>s.total.rows, OversightData::Rows {projection,..}=>projection.rows.len() as i64 }).sum::<i64>(),"children":result.children.iter().map(|c| json!({"tenant_id":c.tenant,"outcome":c.outcome})).collect::<Vec<_>>(),"excluded":result.excluded()}), false).await?;
        Ok(result)
    }
    async fn child(
        &self,
        scope: &OversightScope,
        grant: &ChildGrant,
        root: &str,
        spec: &OversightSpec,
        cache_seconds: u32,
    ) -> Result<OversightData, QueryError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let catalog = Catalog::for_oversight(&**client, scope, grant, spec.rows()).await?;
        // Debug here is an internal key, never logged or exposed. It includes
        // field restrictions/masks and all resolved source/link declarations.
        let policy = format!("{catalog:?}");
        let key = format!(
            "{}:{}:{}:{}:{root}:{spec:?}:{policy}",
            scope.parent().tenant(),
            scope.parent().user(),
            grant.tenant(),
            grant.version()
        );
        let cached = if cache_seconds == 0 {
            None
        } else {
            self.cache
                .lock()
                .map_err(|_| Error::Internal)?
                .get(&key)
                .filter(|(at, _)| at.elapsed() < Duration::from_secs(u64::from(cache_seconds)))
                .map(|(_, data)| data.clone())
        };
        let cache_hit = cached.is_some();
        let data = if let Some(data) = cached {
            data
        } else {
            let tx = client
                .build_transaction()
                .read_only(true)
                .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
                .start()
                .await?;
            tx.batch_execute(&spec.bounds().sql()).await?;
            let data = match spec {
                OversightSpec::Summary(spec) | OversightSpec::RowsSummary(spec) => {
                    let summary =
                        crate::summary::summarize_catalog(&tx, &catalog, root, spec).await?;
                    OversightData::Summary(summary)
                }
                OversightSpec::Rows {
                    page,
                    columns,
                    offsets,
                } => {
                    let mut page = page.clone();
                    if let Some(offset) = offsets.get(&grant.tenant()) {
                        page.offset = *offset;
                    }
                    let keys =
                        crate::summary::page_keys_catalog(&tx, &catalog, root, &page).await?;
                    let projection = crate::query::project_catalog(
                        &tx,
                        &catalog,
                        root,
                        &keys.keys,
                        columns,
                        &Lenses::default(),
                        page.bounds,
                    )
                    .await?;
                    OversightData::Rows {
                        projection,
                        total: keys.total,
                    }
                }
            };
            tx.commit().await?;
            data
        };
        // A restriction or owning-capability change while SQL ran invalidates
        // the result too, including a cached result. Never expose stale masks.
        let fresh = Catalog::for_oversight(&**client, scope, grant, spec.rows()).await?;
        if format!("{fresh:?}") != policy {
            return Err(Error::Forbidden.into());
        }
        let count = match &data {
            OversightData::Summary(s) => s.total.rows,
            OversightData::Rows { projection, .. } => projection.rows.len() as i64,
        };
        let (columns, filters) = spec.evidence();
        // The recheck happens after the read and before the durable action.
        // A failed audit or revoke discards this entire child's answer.
        record_oversight_read(
            &**client,
            scope,
            grant,
            OversightRead {
                table: root,
                level: if spec.rows() { "rows" } else { "aggregates" },
                columns: &columns,
                filters: &filters,
                rows: count,
            },
        )
        .await?;
        if cache_seconds > 0 && !cache_hit {
            let mut cache = self.cache.lock().map_err(|_| Error::Internal)?;
            cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(u64::from(cache_seconds)));
            if cache.len() >= 128 {
                cache.clear();
            }
            cache.insert(key, (Instant::now(), data.clone()));
        }
        Ok(data)
    }
    async fn combine(
        &self,
        children: &[ChildResult],
        spec: &SummarySpec,
    ) -> Result<Line, QueryError> {
        let summaries: Vec<&Summary> = children
            .iter()
            .filter_map(|c| match &c.data {
                Some(OversightData::Summary(s)) => Some(s),
                _ => None,
            })
            .collect();
        let mut values = Vec::new();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let tx = client.build_transaction().read_only(true).start().await?;
        tx.batch_execute(&Bounds::default().sql()).await?;
        for (i, measure) in spec.measures.iter().enumerate() {
            let parts: Value = summaries
                .iter()
                .map(|s| match &s.averages[i] {
                    Some((sum, count)) => json!({"value":sum,"count":count}),
                    None => json!({"value":s.combine_values[i],"count":0}),
                })
                .collect();
            let numeric = summaries
                .first()
                .is_none_or(|s| s.measures[i].0.ty.number());
            let expression = match measure.agg {
                Agg::Count => {
                    "coalesce(to_jsonb(sum((v->>'value')::numeric)), '0'::jsonb)".to_owned()
                }
                Agg::Sum => "to_jsonb(sum((v->>'value')::numeric))".into(),
                Agg::Avg => {
                    "to_jsonb(sum((v->>'value')::numeric)/nullif(sum((v->>'count')::bigint),0))"
                        .into()
                }
                Agg::Min | Agg::Max => format!(
                    "to_jsonb({}((v->>'value'){}))",
                    measure.agg.key(),
                    if numeric { "::numeric" } else { "" }
                ),
                _ => {
                    // Ordered values have no cross-child ordering contract.
                    // Keep them in each child's summary, not a fabricated total.
                    values.push(Value::Null);
                    continue;
                }
            };
            let value: Option<Value> = tx
                .query_one(
                    &format!("SELECT {expression} FROM jsonb_array_elements($1::jsonb) v"),
                    &[&parts],
                )
                .await?
                .get(0);
            values.push(value.unwrap_or(Value::Null));
        }
        tx.commit().await?;
        Ok(Line {
            keys: vec![],
            subtotal: false,
            rows: summaries.iter().map(|s| s.total.rows).sum(),
            values,
        })
    }
    async fn setting(&self, suffix: &str) -> Result<u32, QueryError> {
        let client = self.db.get().await.map_err(Error::from)?;
        Ok(self
            .settings
            .get_global(&**client, &format!("tenancy.oversight.{suffix}"))
            .await?
            .as_u64()
            .ok_or(Error::Internal)? as u32)
    }
}

macro_rules! bound {
    ($key:literal, $label:literal, $default:literal, $min:literal, $max:literal) => {
        inventory::submit! { bilibi_module_registry::Contribution::new(DEFINITIONS, concat!("tenancy.oversight.",$key),
            bilibi_module_registry::Provenance { module:"base/grid", source:file!(), line:line!() }, &Setting::Definition(Definition {
                value_type:ValueType::Integer,scopes:&[ScopeKind::Global],default:||json!($default),nullable:false,encrypted:false,minimum:Some($min as f64),maximum:Some($max as f64),validator:None,
                editor:Some(Editor { group:"Parent oversight",label:$label,help:"Limits each read across consenting child tenants.",capability:None,reveal_duration_ms:15000 }),
            })) }
    };
}
bound!("max_children", "Maximum children per read", 50, 1, 500);
bound!("concurrency", "Concurrent child reads", 4, 1, 16);
bound!(
    "cache_seconds",
    "Reader cache lifetime (seconds; 0 disables)",
    60,
    0,
    3600
);
bound!(
    "child_timeout_ms",
    "Child read timeout (ms)",
    2000,
    100,
    30000
);
bound!(
    "total_timeout_ms",
    "Total read timeout (ms)",
    10000,
    100,
    60000
);

pub(crate) fn service(
    services: &bilibi_module_registry::Services,
) -> bilibi_module_registry::Result<bilibi_module_registry::Value> {
    Ok(Arc::new(OversightQueries::new(
        services.get::<Db>("base/database.pool")?.as_ref().clone(),
        services.get("base/settings.store")?,
        services.get(bilibi_tenancy::OVERSIGHT)?,
    )))
}
