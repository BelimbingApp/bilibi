//! Step-up ("sudo mode") as an operation-level, channel-agnostic contract.
//! An owner declares the operations step-up may guard (`declare!`) and calls
//! [`StepUp::require`] from its own facade; tenant policy decides whether the
//! check applies, the channel decides which proof counts (a web session's
//! recent check, a code the agent's human gave for this one action), and a
//! bound [`Provider`] checks the person. This crate names no consumer and no
//! second-factor method. Contract: `README.md`.
mod declaration;
mod gate;
mod policy;

pub use declaration::{Operation, OPERATIONS};
pub use gate::{
    end_agent_windows, Availability, Challenge, Declared, Enforcement, FactorAccount, Lifecycle,
    Method, MethodChoice, Provider, Purpose, Rule, SessionProof, SessionProvider, Spent, StepUp,
    Window, WindowState,
};
pub use policy::{Mode, CAP_RANGE, WINDOW_RANGE};
#[doc(hidden)]
pub use {
    bilibi_module_registry::{Contribution, Provenance},
    inventory,
};

pub const SERVICE: &str = "base/step_up.gate";

/// The machine answer every agent channel gives for `Error::StepUpRequired`,
/// so an agent can ask its human for a code and repeat the call with it.
pub fn refusal(operation: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "error": "step_up_required",
        "operation": operation,
        "reason": reason,
        "message": match reason {
            "invalid_code" => "The code was not accepted (wrong, expired or already used). Ask the account holder for a new current code.",
            "method_required" => "Choose a second-factor method and send step_up_code as method_id:code.",
            "locked" => "The account holder's checks are locked after repeated failures. Try again later.",
            "not_enrolled" => "The account holder has no second factor yet. They must set one up in Bilibi first.",
            "impersonating" => "Step-up cannot be satisfied while impersonating.",
            "unavailable" => "No second factor is installed, so this operation cannot be confirmed.",
            _ => "This operation needs the account holder's 2FA code. Ask them for the current code and repeat the request with step_up_code.",
        },
    })
}
/// Changing step-up policy is itself step-up-guarded, and on the floor.
pub const POLICY_UPDATE: &str = "base.step_up.policy.update";

declare!(
    "base/step_up",
    Operation::new(POLICY_UPDATE, "Change 2FA requirements").floor()
);

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/step_up", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/settings", "base/audit"],
        migrations: &[("base-step-up-001-agent-windows", include_str!("../migrations/001-agent-windows.sql"))],
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |services| Ok(std::sync::Arc::new(StepUp::new(
            services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
            services.get("base/settings.store")?,
            // The linked modules' declarations; an unmounted module declares none.
            &bilibi_module_registry::Catalog::discover(&bilibi_module_registry::discover()?)?,
        )?)) }],
    }
}
