//! Retiring a person is a Base Step-up operation, off by default: a tenant
//! that wants the holder's code first turns it on in its policy. Both entry
//! points (the facade and the agent write) ask through `retire`.
use bilibi_database::{Error, Result};
use bilibi_party_ids::PersonId;
use bilibi_step_up::{declare, Operation, StepUp};
use bilibi_tenancy::Scope;
use std::sync::{Arc, OnceLock};

pub const RETIRE: &str = "core.person.retire";
declare!(
    "core/person",
    Operation::new(RETIRE, "Retire a person").off_by_default()
);

static STEP_UP: OnceLock<Arc<StepUp>> = OnceLock::new();
pub(crate) fn bind(step_up: Arc<StepUp>) -> bilibi_module_registry::Result<()> {
    STEP_UP.set(step_up).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate person step-up binding".into())
    })
}
pub(crate) async fn retire(scope: &Scope, id: PersonId) -> Result<()> {
    STEP_UP
        .get()
        .ok_or(Error::Internal)?
        .require(scope, RETIRE, &format!("person:{id}"))
        .await
}
