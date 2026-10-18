//! Single-use reviewed writes. Storage retains only SHA-256 digests. Owners
//! validate/lock reference facts and write through the outer audit transaction.
use crate::{Error, Handler, OperationClient, Registry, Result, CONTENT_NOTICE};
use bilibi_database::{Db, MutationBoundary};
use bilibi_settings::Settings;
use bilibi_tenancy::{ProofFuture, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::Transaction;

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
}
impl Plans {
    pub fn new(
        db: Db,
        boundary: Arc<MutationBoundary>,
        registry: Arc<Registry>,
        settings: Arc<Settings>,
    ) -> Self {
        Self {
            db,
            boundary,
            registry,
            settings,
        }
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
        for (index, op) in operations.iter().enumerate() {
            let result = self.preview_one(tx, scope, op).await;
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
                    registered.output
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
    ) -> Result<Value> {
        let registered = self
            .registry
            .operations
            .get(op.key.as_str())
            .ok_or(Error::UnknownOperation)?;
        if !registered.definition.channels.allows(scope.channel()) {
            return Err(Error::ChannelNotAllowed);
        }
        crate::schema::validate(&registered.input, &op.input).map_err(Error::InvalidInput)?;
        bilibi_authz::authorize_key(tx, scope, registered.definition.capability).await?;
        match registered.definition.handler {
            Handler::SafeWrite(owner) => Ok(owner
                .preview(tx as &dyn OperationClient, scope, &op.input)
                .await?
                .envelope(&op.key)),
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
        let identity = authority.verify(&tx).await?;
        let binding = binding(&identity)?;
        let encoded = self.shape(&tx, operations).await?;
        let (results, digest, valid) = self.previews(&tx, &identity.scope, operations).await?;
        let mut response = json!({"valid":valid,"operations":operations,"results":results,"meta":{"content_notice":CONTENT_NOTICE}});
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
            let row = tx.query_one("INSERT INTO base_agent_api_plans(code_hash,tenant_id,binding_hash,operations_hash,preview_hash,expires_at) VALUES(sha256(convert_to($1,'UTF8')),$2,sha256(convert_to($3,'UTF8')),sha256(convert_to($4,'UTF8')),sha256(convert_to($5,'UTF8')),clock_timestamp()+make_interval(secs=>$6::double precision)) RETURNING to_char(expires_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')", &[&code,&identity.scope.tenant(),&binding,&encoded,&digest,&(seconds as f64)]).await.map_err(bilibi_database::Error::from)?;
            response["plan_code"] = json!(code);
            response["expires_at"] = json!(row.get::<_, String>(0));
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
        let identity = authority.verify(tx.sql()).await?;
        if scope_binding(scope) != scope_binding(&identity.scope) {
            return Err(Error::PlanMismatch);
        }
        let binding = binding(&identity)?;
        let encoded = self.shape(tx.sql(), operations).await?;
        // Lock by tenant/code before reading used/expiry. A racing commit waits,
        // then observes consumption. Wall clock is checked after lock acquisition.
        let row = tx.sql().query_opt("SELECT binding_hash=sha256(convert_to($3,'UTF8')) AND operations_hash=sha256(convert_to($4,'UTF8')), used_at IS NOT NULL, preview_hash FROM base_agent_api_plans WHERE tenant_id=$1 AND code_hash=sha256(convert_to($2,'UTF8')) FOR UPDATE", &[&scope.tenant(),&code,&binding,&encoded]).await.map_err(bilibi_database::Error::from)?.ok_or(Error::InvalidPlanCode)?;
        if !row.get::<_, bool>(0) {
            return Err(Error::PlanMismatch);
        }
        if row.get::<_, bool>(1) {
            return Err(Error::PlanAlreadyUsed);
        }
        self.unexpired(tx.sql(), scope, code).await?;
        let (results, digest, valid) = self.previews(tx.sql(), scope, operations).await?;
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
        for (index, op) in operations.iter().enumerate() {
            let registered = self
                .registry
                .operations
                .get(op.key.as_str())
                .ok_or(Error::UnknownOperation)?;
            // Recheck each capability after previous operations as well.
            bilibi_authz::authorize_key(tx.sql(), scope, registered.definition.capability).await?;
            let Handler::SafeWrite(owner) = registered.definition.handler else {
                return Err(Error::WritesNotEnabled);
            };
            let result = owner.commit(&mut tx, scope, &op.input).await?;
            committed.push(json!({"index":index,"key":op.key,"result":result.envelope(&op.key)}));
        }
        // Reprove expiry/eligibility after owner execution, including changes made
        // by operations in this plan. Slow handlers cannot extend code lifetime.
        let final_identity = authority.verify(tx.sql()).await?;
        if binding != self::binding(&final_identity)? {
            return Err(Error::PlanMismatch);
        }
        self.unexpired(tx.sql(), scope, code).await?;
        tx.sql().execute("UPDATE base_agent_api_plans SET used_at=clock_timestamp() WHERE tenant_id=$1 AND code_hash=sha256(convert_to($2,'UTF8'))",&[&scope.tenant(),&code]).await.map_err(bilibi_database::Error::from)?;
        tx.commit().await?;
        Ok(json!({"results":committed,"meta":{"content_notice":CONTENT_NOTICE}}))
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
        let identity = authority.verify(tx.sql()).await?;
        if scope_binding(scope) != scope_binding(&identity.scope) {
            return Err(Error::PlanMismatch);
        }
        let binding = binding(&identity)?;
        self.shape(tx.sql(), std::slice::from_ref(op)).await?;
        self.preview_one(tx.sql(), scope, op).await?;
        let registered = self
            .registry
            .operations
            .get(op.key.as_str())
            .ok_or(Error::UnknownOperation)?;
        let Handler::SafeWrite(owner) = registered.definition.handler else {
            return Err(Error::WritesNotEnabled);
        };
        let result = owner.commit(&mut tx, scope, &op.input).await?;
        if binding != self::binding(&authority.verify(tx.sql()).await?)? {
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
    json!([
        scope.tenant(),
        "user",
        scope.user(),
        scope.actor().person(),
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
// Operator-editable in the Settings editor, as in Bilimbi; the MCP limit sits
// beside them in `mcp.rs`.
macro_rules! plan_setting {
    ($key:literal,$default:literal,$min:literal,$max:literal,$label:literal,$help:literal) => {
        inventory::submit! { Contribution::new(bilibi_settings::DEFINITIONS,$key,Provenance {module:"base/agent_api",source:file!(),line:line!()}, &Setting::Definition(Definition {
            value_type:ValueType::Integer,scopes:&[ScopeKind::Global],default:||json!($default),nullable:false,encrypted:false,minimum:Some($min),maximum:Some($max),validator:None,
            editor:Some(Editor { group:"Agent API", label:$label, help:$help, capability:None, reveal_duration_ms:5_000 }),
        })) }
    };
}
plan_setting!(
    "agent_api.plan_expiry_seconds",
    600,
    60.0,
    3600.0,
    "Agent plan lifetime",
    "Seconds to review and commit an agent write plan."
);
plan_setting!(
    "agent_api.plan_max_operations",
    20,
    1.0,
    100.0,
    "Operations per agent plan",
    "Maximum number of writes in one reviewed plan."
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
