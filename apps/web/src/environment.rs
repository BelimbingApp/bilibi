//! Which kind of deployment this process serves, from `BILIBI_ENV`
//! (`development`, `staging` or `production`). Unset means production, so a
//! forgotten value never relaxes a guard; any other value refuses to boot.
//! It is a bootstrap fact of the deployment, not an operator setting: the
//! settings store cannot be allowed to switch off the guards it decides.
//! The same value is the instance's role (Dev, Staging, Production) that the
//! banner and the Software page announce: `bilibi_base::system::Role` reads
//! it with the one parser below, so there is no second identity source.
use bilibi_base::system::Role;
use bilibi_database::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    Development,
    Staging,
    Production,
}

impl Environment {
    pub fn from_env() -> Result<Self> {
        match std::env::var("BILIBI_ENV") {
            Err(std::env::VarError::NotPresent) => Ok(Self::Production),
            Ok(value) => Self::parse(&value).ok_or_else(|| {
                eprintln!("BILIBI_ENV must be development, staging or production");
                Error::Invalid
            }),
            Err(_) => Err(Error::Invalid),
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Some(match Role::parse(value)? {
            Role::Dev => Self::Development,
            Role::Staging => Self::Staging,
            Role::Production => Self::Production,
        })
    }
    /// Step-up (`Access::StepUp`) is enforced everywhere but development.
    pub fn requires_step_up(self) -> bool {
        self != Self::Development
    }
    /// The SMS simulator (and its `/dev/sms-inbox`) runs in development and
    /// staging only; production has no SMS until a real driver is added.
    pub fn sms_simulator(self) -> bool {
        self != Self::Production
    }
}
