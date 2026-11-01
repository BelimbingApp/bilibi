//! Step-up and audit edge for showing one stored encrypted value. The password prompt is replaced with
//! Base Step-up. The capability must be granted by name; the reveal is a
//! step-up operation on the installation floor, and every attempt is
//! recorded before plaintext can leave. No audit row, no value.
use crate::{form::Plan, SECRET_VIEW};
use bilibi_audit::{record_action, Attribution};
use bilibi_authz::{decide, explicitly_granted};
use bilibi_database::{Error, Result};
use bilibi_settings::{Settings, Target};
use bilibi_step_up::StepUp;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

/// Showing a stored secret asks for the holder's second factor on every
/// tenant: it replaces the password the page used to ask for.
pub const STEP_UP: &str = "base.settings_editor.secret.reveal";
bilibi_step_up::declare!(
    "base/settings_editor",
    bilibi_step_up::Operation::new(STEP_UP, "Show a stored secret setting").floor()
);

pub enum Refusal {
    /// The step-up check refused (`Error::StepUpRequired`): the host sends
    /// the browser to confirm and back.
    StepUp(Error),
    AuditUnavailable,
    /// Not granted, not a visible stored secret, or the key is unavailable.
    Refused,
}

/// Presentation only: whether to draw "Show stored value". Reads without a
/// decision-log row; `reveal` decides again, logged, before acting. An
/// impersonated session never sees it: step-up never accepts
/// an operator for the borrowed account.
pub async fn available(
    client: &impl GenericClient,
    plan: &Plan,
    settings: &Settings,
    scope: &Scope,
) -> Result<bool> {
    Ok(settings.encryption_available()
        && scope.impersonator().is_none()
        && plan.capabilities.contains(SECRET_VIEW)
        && explicitly_granted(client, scope, SECRET_VIEW).await?)
}

/// Records a refused attempt that never reached the step-up check, such as a
/// forged request for a field the page did not offer.
pub async fn refuse(client: &(impl GenericClient + Sync), scope: &Scope, key: &str) -> Result<()> {
    audit(client, scope, key, false).await
}

pub struct Reveal<'a> {
    pub settings: &'a Settings,
    pub step_up: &'a StepUp,
}
impl Reveal<'_> {
    /// The plaintext of `key`'s stored global value, as text for the input.
    pub async fn reveal(
        &self,
        client: &(impl GenericClient + Sync),
        plan: &Plan,
        scope: &Scope,
        key: &str,
    ) -> std::result::Result<String, Refusal> {
        let result = self.attempt(client, plan, scope, key).await;
        match audit(client, scope, key, result.is_ok()).await {
            Ok(()) => result,
            Err(_) => Err(Refusal::AuditUnavailable),
        }
    }
    async fn attempt(
        &self,
        client: &(impl GenericClient + Sync),
        plan: &Plan,
        scope: &Scope,
        key: &str,
    ) -> std::result::Result<String, Refusal> {
        let granted = scope.impersonator().is_none()
            && decide(client, scope, SECRET_VIEW)
                .await
                .is_ok_and(|decision| decision.allowed)
            && explicitly_granted(client, scope, SECRET_VIEW)
                .await
                .unwrap_or(false);
        let stored = plan
            .field(key)
            .is_some_and(|field| field.encrypted() && field.stored_secret);
        if !granted || !stored || !self.settings.encryption_available() {
            return Err(Refusal::Refused);
        }
        match self
            .step_up
            .require(scope, STEP_UP, &format!("setting:{key}"))
            .await
        {
            Ok(()) => {}
            Err(error @ Error::StepUpRequired { .. }) => return Err(Refusal::StepUp(error)),
            Err(_) => return Err(Refusal::Refused),
        }
        match self.settings.get(client, scope, Target::Global, key).await {
            Ok(resolved) => Ok(match resolved.value {
                Value::String(text) => text,
                Value::Null => return Err(Refusal::Refused),
                other => other.to_string(),
            }),
            Err(_) => Err(Refusal::Refused),
        }
    }
}

/// One retained action per attempt, never with the value.
async fn audit(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    key: &str,
    succeeded: bool,
) -> Result<()> {
    let payload = json!({
        "setting_key": key.chars().take(255).collect::<String>(),
        "scope_type": "global",
        "scope_id": Value::Null,
        "result": if succeeded { "succeeded" } else { "refused" },
    });
    record_action(
        client,
        &Attribution::from_scope(scope),
        None,
        "settings.secret.reveal",
        payload,
        true,
    )
    .await
    .map(|_| ())
    .map_err(|_| Error::Internal)
}
