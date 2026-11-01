//! Plans, with same-plan record references.
//! Single-use reviewed writes. Storage retains only SHA-256 digests. Owners
//! validate/lock reference facts and write through the outer audit transaction.
use crate::{Error, Handler, Registry, Result, CONTENT_NOTICE};
use bilibi_database::{Db, MutationBoundary};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::Settings;
use bilibi_system::Instance;
use bilibi_tenancy::{reaches_work_company, Authentication, Channel, ProofFuture, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{future::Future, sync::Arc};
use tokio_postgres::Transaction;

const WORKPLACE_PROOF_BOUNDS: &str = "SET LOCAL lock_timeout = '150ms'";

/// Wire input is bounded before handlers run. Ordering and exact JSON types are
/// part of the contract; unknown keys and non-object inputs are refused.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedOperation {
    pub key: String,
    pub input: Value,
}

/// Trusted credential owner output, never deserialize from a request. Binding
/// identifies the OAuth connection (not a rotating access token), or a browser
/// session. Include impersonation identity/session in this opaque binding when
/// supported. The sealed Scope supplies account and party facets separately.
pub struct PlanIdentity {
    pub scope: Scope,
    pub credential_binding: String,
}
/// E5's credential adapter must check revocation/expiry and reprove live account,
/// company and person eligibility on this transaction, holding the relevant
/// credential/eligibility locks until commit. No cached request Scope suffices.
pub trait PlanAuthority: Send + Sync {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity>;
}

pub struct Plans {
    db: Db,
    boundary: Arc<MutationBoundary>,
    registry: Arc<Registry>,
    settings: Arc<Settings>,
    authentication: Arc<Authentication>,
}
pub(crate) struct ReviewSettings {
    pub max_operations: u64,
    pub requests_per_minute: u64,
    pub expiry_seconds: u64,
}
impl Plans {
    pub fn new(
        db: Db,
        boundary: Arc<MutationBoundary>,
        registry: Arc<Registry>,
        settings: Arc<Settings>,
        authentication: Arc<Authentication>,
    ) -> Self {
        Self {
            db,
            boundary,
            registry,
            settings,
            authentication,
        }
    }
    async fn plan_scope(
        &self,
        tx: &Transaction<'_>,
        scope: &Scope,
        operations: &[PlannedOperation],
    ) -> Result<Scope> {
        if scope.agent().is_none() {
            return Ok(scope.clone());
        }
        let mut explicit = None;
        for company in operations
            .iter()
            .filter_map(|op| op.input.get("company_id").and_then(Value::as_i64))
            .filter_map(|id| OrganisationId::new(id).ok())
        {
            if reaches_work_company(tx, scope, company).await? {
                if explicit.is_some_and(|selected| selected != company) {
                    return Err(Error::InvalidInput(std::collections::BTreeMap::from([(
                        "operations".into(),
                        vec!["must use one workplace per plan".into()],
                    )])));
                }
                explicit = Some(company);
            }
        }
        let company = explicit
            .or_else(|| scope.current_company())
            .ok_or(bilibi_database::Error::Forbidden)?;
        self.authentication
            .reprove_work_company(tx, scope, company)
            .await
            .map_err(Error::from)
    }
    async fn workplace_proof<T>(
        &self,
        tx: &Transaction<'_>,
        proof: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        tx.batch_execute(WORKPLACE_PROOF_BOUNDS)
            .await
            .map_err(bilibi_database::Error::from)?;
        match proof.await {
            Ok(value) => {
                tx.batch_execute("SET LOCAL lock_timeout = DEFAULT")
                    .await
                    .map_err(bilibi_database::Error::from)?;
                Ok(value)
            }
            Err(Error::Backend(error)) if error.is_retryable_lock_contention() => {
                Err(bilibi_database::Error::Conflict.into())
            }
            Err(error) => Err(error),
        }
    }
    /// The installation every plan and commit names (`instance` in both
    /// answers), so the person's review says where the change lands.
    pub async fn instance(&self) -> bilibi_database::Result<Instance> {
        let client = self.db.get().await?;
        Instance::resolve(&self.settings, &**client).await
    }
    pub(crate) async fn review_settings(&self) -> Result<ReviewSettings> {
        let client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let max_operations = self
            .settings
            .get_global(&**client, "agent_api.plan_max_operations")
            .await?
            .as_u64()
            .ok_or(bilibi_database::Error::Invalid)?;
        let requests_per_minute = self
            .settings
            .get_global(&**client, crate::mcp::REQUESTS_PER_MINUTE)
            .await?
            .as_u64()
            .ok_or(bilibi_database::Error::Invalid)?;
        let expiry_seconds = self
            .settings
            .get_global(&**client, "agent_api.plan_expiry_seconds")
            .await?
            .as_u64()
            .ok_or(bilibi_database::Error::Invalid)?;
        Ok(ReviewSettings {
            max_operations,
            requests_per_minute,
            expiry_seconds,
        })
    }
    async fn shape(&self, tx: &Transaction<'_>, operations: &[PlannedOperation]) -> Result<String> {
        let max = self
            .settings
            .get_global(tx, "agent_api.plan_max_operations")
            .await?
            .as_u64()
            .ok_or(bilibi_database::Error::Invalid)?;
        if operations.is_empty()
            || operations.len() as u64 > max
            || operations
                .iter()
                .any(|op| op.key.len() > 255 || !op.input.is_object())
        {
            return Err(Error::InvalidInput(std::collections::BTreeMap::from([(
                "operations".into(),
                vec!["must be a bounded non-empty list of key/input objects".into()],
            )])));
        }
        let encoded = canonical(
            &serde_json::to_value(operations).map_err(|_| bilibi_database::Error::Invalid)?,
        )
        .to_string();
        if encoded.len() > 1_048_576 {
            return Err(Error::InvalidInput(std::collections::BTreeMap::from([(
                "operations".into(),
                vec!["must fit within 1 MiB".into()],
            )])));
        }
        Ok(encoded)
    }
    async fn previews(
        &self,
        tx: &Transaction<'_>,
        scope: &Scope,
        operations: &[PlannedOperation],
    ) -> Result<(Value, String, bool)> {
        let mut results = Vec::new();
        let mut definitions = Vec::new();
        let mut valid = true;
        let reservations = crate::PreviewReservations::default();
        for (index, op) in operations.iter().enumerate() {
            let result = async {
                let references =
                    crate::plan_references::collect(&self.registry, operations, index)?;
                self.preview_one(tx, scope, op, &references, &reservations)
                    .await
            }
            .await;
            match result {
                Ok(preview) => {
                    results.push(json!({"index":index,"key":op.key,"valid":true,"preview":preview}))
                }
                // Infrastructure errors must abort, including failed decision recording.
                Err(Error::Backend(error))
                    if !matches!(
                        error,
                        bilibi_database::Error::Forbidden
                            | bilibi_database::Error::Conflict
                            | bilibi_database::Error::Invalid
                            | bilibi_database::Error::NotFound
                    ) =>
                {
                    return Err(Error::Backend(error))
                }
                Err(error) => {
                    valid = false;
                    let reason = match &error {
                        Error::InvalidInput(fields) => {
                            json!({"code":"invalid_input","fields":fields})
                        }
                        Error::Backend(bilibi_database::Error::Conflict) => {
                            json!({"code":"rejected","reason":"record_changed"})
                        }
                        Error::Backend(bilibi_database::Error::Forbidden) => {
                            json!({"code":"missing_capability"})
                        }
                        Error::Backend(_) => json!({"code":"rejected"}),
                        _ => json!({"code":error.to_string()}),
                    };
                    results.push(json!({"index":index,"key":op.key,"valid":false,"error":reason}));
                }
            }
            if let Some(registered) = self.registry.operations.get(op.key.as_str()) {
                definitions.push(json!([
                    registered.definition.key,
                    registered.definition.capability,
                    registered.input,
                    registered.output,
                    crate::plan_references::creation(&registered.definition.handler)
                ]));
            }
        }
        let results = json!(results);
        let digest =
            canonical(&json!(["bilibi-agent-review-v1", definitions, results])).to_string();
        Ok((results, digest, valid))
    }
    async fn preview_one(
        &self,
        tx: &Transaction<'_>,
        scope: &Scope,
        op: &PlannedOperation,
        references: &crate::PlanReferences,
        reservations: &crate::PreviewReservations,
    ) -> Result<Value> {
        let registered = self
            .registry
            .operations
            .get(op.key.as_str())
            .ok_or(Error::UnknownOperation)?;
        if !registered.definition.channels.allows(scope.channel()) {
            return Err(Error::ChannelNotAllowed);
        }
        if references.is_empty() {
            crate::schema::validate(&registered.input, &op.input).map_err(Error::InvalidInput)?;
        } else {
            crate::schema::validate_plan(
                &registered.input,
                &op.input,
                &references.paths().cloned().collect(),
            )
            .map_err(Error::InvalidInput)?;
        }
        crate::authorize_target(tx, scope, registered.definition.capability, &op.input).await?;
        match registered.definition.handler {
            Handler::SafeWrite(owner) if references.is_empty() => Ok(owner
                .preview(&crate::TransactionRead(tx, reservations), scope, &op.input)
                .await?
                .envelope(&op.key)),
            Handler::SafeWrite(owner) => {
                let preview = owner
                    .preview_dependent(
                        &crate::TransactionRead(tx, reservations),
                        scope,
                        &op.input,
                        references,
                    )
                    .await?
                    .envelope(&op.key);
                Ok(
                    json!({"result":{"owner_preview":preview["result"],"references":references,"notice":"New record IDs and owner eligibility are checked when these steps save together."},"meta":preview["meta"]}),
                )
            }
            _ => Err(Error::WritesNotEnabled),
        }
    }
    /// A code proves the reviewed contract, not human consent. The calling
    /// client obtains consent before invoking commit; this service cannot do so.
    pub async fn preview(
        &self,
        authority: &dyn PlanAuthority,
        operations: &[PlannedOperation],
    ) -> Result<Value> {
        let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let client: &mut tokio_postgres::Client = &mut client;
        let tx = client
            .transaction()
            .await
            .map_err(bilibi_database::Error::from)?;
        let identity = self
            .workplace_proof(&tx, async {
                authority.verify(&tx).await.map_err(Error::from)
            })
            .await?;
        let binding = binding(&identity)?;
        let encoded = self.shape(&tx, operations).await?;
        let plan_scope = self
            .workplace_proof(&tx, self.plan_scope(&tx, &identity.scope, operations))
            .await?;
        let (results, digest, valid) = self.previews(&tx, &plan_scope, operations).await?;
        let instance = Instance::resolve(&self.settings, &tx).await?;
        let mut response = json!({"valid":valid,"instance":instance.agent_json(),"operations":operations,"results":results,"meta":{"content_notice":CONTENT_NOTICE}});
        if valid {
            let seconds = self
                .settings
                .get_global(&tx, "agent_api.plan_expiry_seconds")
                .await?
                .as_i64()
                .ok_or(bilibi_database::Error::Invalid)?;
            // Two UUIDv4 values supply 244 unpredictable bits without a new RNG dependency.
            let code = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            tx.execute("DELETE FROM base_agent_api_plans WHERE tenant_id=$1 AND expires_at<clock_timestamp()-interval '1 day'", &[&identity.scope.tenant()]).await.map_err(bilibi_database::Error::from)?;
            let company = plan_scope.current_company().map(OrganisationId::get);
            let row = tx.query_one("INSERT INTO base_agent_api_plans(code_hash,tenant_id,binding_hash,operations_hash,preview_hash,company_id,expires_at) VALUES(sha256(convert_to($1,'UTF8')),$2,sha256(convert_to($3,'UTF8')),sha256(convert_to($4,'UTF8')),sha256(convert_to($5,'UTF8')),$6,clock_timestamp()+make_interval(secs=>$7::double precision)) RETURNING to_char(expires_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')", &[&code,&identity.scope.tenant(),&binding,&encoded,&digest,&company,&(seconds as f64)]).await.map_err(bilibi_database::Error::from)?;
            response["plan_code"] = json!(code);
            response["expires_at"] = json!(row.get::<_, String>(0));
            response["work_company_id"] = json!(company);
        }
        tx.commit().await.map_err(bilibi_database::Error::from)?;
        Ok(response)
    }
    /// The request Scope chooses audit attribution only. Authority reproves it
    /// inside the transaction; any mismatch rolls back before owner execution.
    pub async fn commit(
        &self,
        scope: &Scope,
        authority: &dyn PlanAuthority,
        code: &str,
        operations: &[PlannedOperation],
    ) -> Result<Value> {
        if code.len() != 64
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(Error::InvalidPlanCode);
        }
        let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("base.agent_api.plan.commit"),
            )
            .await?;
        let identity = self
            .workplace_proof(tx.sql(), async {
                authority.verify(tx.sql()).await.map_err(Error::from)
            })
            .await?;
        if scope_binding(scope) != scope_binding(&identity.scope) {
            return Err(Error::PlanMismatch);
        }
        let binding = binding(&identity)?;
        let encoded = self.shape(tx.sql(), operations).await?;
        // Lock by tenant/code before reading used/expiry. A racing commit waits,
        // then observes consumption. Wall clock is checked after lock acquisition.
        let row = tx.sql().query_opt("SELECT binding_hash=sha256(convert_to($3,'UTF8')) AND operations_hash=sha256(convert_to($4,'UTF8')), used_at IS NOT NULL, preview_hash, company_id FROM base_agent_api_plans WHERE tenant_id=$1 AND code_hash=sha256(convert_to($2,'UTF8')) FOR UPDATE", &[&scope.tenant(),&code,&binding,&encoded]).await.map_err(bilibi_database::Error::from)?.ok_or(Error::InvalidPlanCode)?;
        if !row.get::<_, bool>(0) {
            return Err(Error::PlanMismatch);
        }
        if row.get::<_, bool>(1) {
            return Err(Error::PlanAlreadyUsed);
        }
        self.unexpired(tx.sql(), scope, code).await?;
        let company = row
            .get::<_, Option<i64>>(3)
            .map(OrganisationId::new)
            .transpose()
            .map_err(|_| bilibi_database::Error::Invalid)?;
        let work_scope = if let Some(company) = company {
            self.workplace_proof(tx.sql(), async {
                self.authentication
                    .reprove_work_company(tx.sql(), &identity.scope, company)
                    .await
                    .map_err(Error::from)
            })
            .await?
        } else {
            identity.scope.clone()
        };
        let (results, digest, valid) = self.previews(tx.sql(), &work_scope, operations).await?;
        if !valid {
            return Err(Error::PlanRejected(results));
        }
        let current: Vec<u8> = tx
            .sql()
            .query_one("SELECT sha256(convert_to($1,'UTF8'))", &[&digest])
            .await
            .map_err(bilibi_database::Error::from)?
            .get(0);
        if current != row.get::<_, Vec<u8>>(2) {
            return Err(Error::StalePlan);
        }
        let mut committed = Vec::new();
        let reservations = crate::PreviewReservations::default();
        for (index, op) in operations.iter().enumerate() {
            let references = crate::plan_references::collect(&self.registry, operations, index)?;
            let current = match self
                .preview_one(tx.sql(), &work_scope, op, &references, &reservations)
                .await
            {
                Ok(preview) => preview,
                Err(Error::Backend(bilibi_database::Error::Forbidden)) => {
                    return Err(Error::Backend(bilibi_database::Error::Forbidden));
                }
                Err(Error::Backend(error))
                    if !matches!(
                        error,
                        bilibi_database::Error::Conflict
                            | bilibi_database::Error::Invalid
                            | bilibi_database::Error::NotFound
                    ) =>
                {
                    return Err(Error::Backend(error));
                }
                Err(_) => return Err(Error::StalePlan),
            };
            if canonical(&current) != canonical(&results[index]["preview"]) {
                return Err(Error::StalePlan);
            }
            let registered = self
                .registry
                .operations
                .get(op.key.as_str())
                .ok_or(Error::UnknownOperation)?;
            let Handler::SafeWrite(owner) = registered.definition.handler else {
                return Err(Error::WritesNotEnabled);
            };
            let input = crate::plan_references::resolve(&op.input, &committed)?;
            crate::schema::validate(&registered.input, &input).map_err(Error::InvalidInput)?;
            let result = owner.commit(&mut tx, &work_scope, &input).await?;
            committed.push(json!({"index":index,"key":op.key,"created":owner.creates(),"result":result.envelope(&op.key)}));
        }
        // Reprove expiry/eligibility after owner execution, including changes made
        // by operations in this plan. Slow handlers cannot extend code lifetime.
        let final_identity = self
            .workplace_proof(tx.sql(), async {
                authority.verify(tx.sql()).await.map_err(Error::from)
            })
            .await?;
        let final_scope = if let Some(company) = company {
            self.workplace_proof(tx.sql(), async {
                self.authentication
                    .reprove_work_company(tx.sql(), &final_identity.scope, company)
                    .await
                    .map_err(Error::from)
            })
            .await?
        } else {
            final_identity.scope.clone()
        };
        if binding
            != self::binding(&PlanIdentity {
                scope: final_scope,
                credential_binding: final_identity.credential_binding,
            })?
        {
            return Err(Error::PlanMismatch);
        }
        self.unexpired(tx.sql(), scope, code).await?;
        tx.sql().execute("UPDATE base_agent_api_plans SET used_at=clock_timestamp() WHERE tenant_id=$1 AND code_hash=sha256(convert_to($2,'UTF8'))",&[&scope.tenant(),&code]).await.map_err(bilibi_database::Error::from)?;
        let instance = Instance::resolve(&self.settings, tx.sql()).await?;
        tx.commit().await?;
        Ok(
            json!({"instance":instance.agent_json(),"results":committed,"meta":{"content_notice":CONTENT_NOTICE}}),
        )
    }
    /// One write reviewed and committed in a single transaction, for a caller
    /// whose person has just reviewed the input (a page's form). The owner's
    /// preview runs first and its refusal comes back typed, then the same
    /// checks as `commit` and the owner's commit. No code is issued: the
    /// review and the write share the transaction, and the owner's version
    /// fence recognises a repeat.
    pub async fn submit(
        &self,
        scope: &Scope,
        authority: &dyn PlanAuthority,
        op: &PlannedOperation,
    ) -> Result<Value> {
        let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context(op.key.clone()))
            .await?;
        let identity = self
            .workplace_proof(tx.sql(), async {
                authority.verify(tx.sql()).await.map_err(Error::from)
            })
            .await?;
        if scope_binding(scope) != scope_binding(&identity.scope) {
            return Err(Error::PlanMismatch);
        }
        let binding = binding(&identity)?;
        self.shape(tx.sql(), std::slice::from_ref(op)).await?;
        let reservations = crate::PreviewReservations::default();
        self.preview_one(
            tx.sql(),
            scope,
            op,
            &crate::PlanReferences::default(),
            &reservations,
        )
        .await?;
        let registered = self
            .registry
            .operations
            .get(op.key.as_str())
            .ok_or(Error::UnknownOperation)?;
        let Handler::SafeWrite(owner) = registered.definition.handler else {
            return Err(Error::WritesNotEnabled);
        };
        let result = owner.commit(&mut tx, scope, &op.input).await?;
        let final_identity = self
            .workplace_proof(tx.sql(), async {
                authority.verify(tx.sql()).await.map_err(Error::from)
            })
            .await?;
        if binding != self::binding(&final_identity)? {
            return Err(Error::PlanMismatch);
        }
        tx.commit().await?;
        Ok(result.envelope(&op.key))
    }
    async fn unexpired(&self, tx: &Transaction<'_>, scope: &Scope, code: &str) -> Result<()> {
        let row = tx.query_one("SELECT expires_at>clock_timestamp() FROM base_agent_api_plans WHERE tenant_id=$1 AND code_hash=sha256(convert_to($2,'UTF8'))",&[&scope.tenant(),&code]).await.map_err(bilibi_database::Error::from)?;
        if row.get(0) {
            Ok(())
        } else {
            Err(Error::PlanExpired)
        }
    }
}
fn scope_binding(scope: &Scope) -> Value {
    let company = match scope.channel() {
        Channel::Web => scope.current_company(),
        Channel::Api | Channel::Mcp | Channel::Job => None,
    };
    json!([
        scope.tenant(),
        "user",
        scope.user(),
        scope.actor().person(),
        company,
        scope.actor().organisation(),
        scope.actor().affiliation()
    ])
}
fn binding(identity: &PlanIdentity) -> Result<String> {
    if identity.credential_binding.is_empty() || identity.credential_binding.len() > 1024 {
        return Err(bilibi_database::Error::Unauthorized.into());
    }
    Ok(json!([
        "bilibi-agent-identity-v1",
        scope_binding(&identity.scope),
        identity.credential_binding
    ])
    .to_string())
}

use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType};
// Operator-editable in the Settings editor; the MCP limit sits
// beside them in `mcp.rs`.
macro_rules! plan_setting {
    ($key:literal,$default:literal,$min:literal,$max:literal,$label:literal,$help:literal) => {
        inventory::submit! { Contribution::new(bilibi_settings::DEFINITIONS,$key,Provenance {module:"base/agent_api",source:file!(),line:line!()}, &Setting::Definition(Definition {
            value_type:ValueType::Integer,scopes:&[ScopeKind::Global],default:||json!($default),nullable:false,encrypted:false,minimum:Some($min),maximum:Some($max),validator:None,
            editor:Some(Editor { group:"Agent API", label:$label, help:$help, capability:None, reveal_duration_ms: 5_000 }),
        })) }
    };
}
plan_setting!(
    "agent_api.plan_expiry_seconds",
    600,
    60.0,
    3600.0,
    "Agent plan lifetime",
    "Seconds a person has to review and approve an agent's planned changes."
);
plan_setting!(
    "agent_api.plan_max_operations",
    20,
    1.0,
    100.0,
    "Operations per agent plan",
    "Most changes an agent can propose in one plan."
);

// Explicitly sort at every depth, even if another crate enables preserve_order.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        scalar => scalar.clone(),
    }
}
