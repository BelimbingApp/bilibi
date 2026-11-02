//! Dry-run reviews and writes, with same-request record references.
//! Owners validate/lock reference facts and write through the outer audit transaction.
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
        Ok(ReviewSettings {
            max_operations,
            requests_per_minute,
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
    ) -> Result<(Value, bool)> {
        let mut results = Vec::new();
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
        }
        let results = json!(results);
        Ok((results, valid))
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
    /// Check writes without changing business records. The caller shows this
    /// review and obtains consent before invoking commit; this service cannot
    /// attest that consent.
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
        self.shape(&tx, operations).await?;
        let plan_scope = self
            .workplace_proof(&tx, self.plan_scope(&tx, &identity.scope, operations))
            .await?;
        let (results, valid) = self.previews(&tx, &plan_scope, operations).await?;
        let instance = Instance::resolve(&self.settings, &tx).await?;
        let response = json!({"valid":valid,"instance":instance.agent_json(),"work_company_id":plan_scope.current_company().map(OrganisationId::get),"operations":operations,"results":results,"meta":{"content_notice":CONTENT_NOTICE}});
        tx.commit().await.map_err(bilibi_database::Error::from)?;
        Ok(response)
    }
    /// Recheck and write the supplied operations in one transaction. Authority,
    /// schemas, capabilities and owner previews are all fresh for this write.
    pub async fn commit(
        &self,
        scope: &Scope,
        authority: &dyn PlanAuthority,
        operations: &[PlannedOperation],
    ) -> Result<Value> {
        let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("base.agent_api.write"))
            .await?;
        let identity = self
            .workplace_proof(tx.sql(), async {
                authority.verify(tx.sql()).await.map_err(Error::from)
            })
            .await?;
        if scope_binding(scope) != scope_binding(&identity.scope) {
            return Err(bilibi_database::Error::Unauthorized.into());
        }
        let binding = binding(&identity)?;
        self.shape(tx.sql(), operations).await?;
        let work_scope = self
            .workplace_proof(tx.sql(), self.plan_scope(tx.sql(), scope, operations))
            .await?;
        let (results, valid) = self.previews(tx.sql(), &work_scope, operations).await?;
        if !valid {
            return Err(Error::PlanRejected(results));
        }
        let mut committed = Vec::new();
        for (index, op) in operations.iter().enumerate() {
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
        // Reprove the credential and workplace after owner execution, including
        // eligibility changes made by operations in this write.
        let final_identity = self
            .workplace_proof(tx.sql(), async {
                authority.verify(tx.sql()).await.map_err(Error::from)
            })
            .await?;
        let final_scope = if let Some(company) = work_scope.current_company() {
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
            return Err(bilibi_database::Error::Unauthorized.into());
        }
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
            return Err(bilibi_database::Error::Unauthorized.into());
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
            return Err(bilibi_database::Error::Unauthorized.into());
        }
        tx.commit().await?;
        Ok(result.envelope(&op.key))
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
