//! The grant changes Authz lets Base Step-up guard (declared here, called
//! from `grants.rs`). On by default; each tenant decides on its policy page.
use bilibi_step_up::{declare, Operation};

pub const PRINCIPAL_ROLE: &str = "base.authz.principal_role.set";
pub const GRANT_ALL_ROLE: &str = "base.authz.grant_all_role.assign";
pub const PRINCIPAL_CAPABILITY: &str = "base.authz.principal_capability.set";
pub const ROLE_CAPABILITY: &str = "base.authz.role_capability.set";
pub const SYSTEM_PRINCIPAL: &str = "base.authz.system_principal.set";

declare!(
    "base/authz",
    Operation::new(PRINCIPAL_ROLE, "Give or remove a user's role")
);
declare!(
    "base/authz",
    Operation::new(
        GRANT_ALL_ROLE,
        "Give a user a full-access role (such as Core Administrator)"
    )
);
declare!(
    "base/authz",
    Operation::new(
        PRINCIPAL_CAPABILITY,
        "Grant or deny a capability to a user directly"
    )
);
declare!(
    "base/authz",
    Operation::new(ROLE_CAPABILITY, "Change a role's capabilities")
);
declare!(
    "base/authz",
    Operation::new(SYSTEM_PRINCIPAL, "Change a system principal's grants")
);
