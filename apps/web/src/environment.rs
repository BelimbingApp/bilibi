//! Which kind of deployment this process serves, from `BILIBI_ENV`
//! (`development`, `staging` or `production`). Unset means production, so a
//! forgotten value never relaxes a guard; any other value refuses to boot.
//! It is a bootstrap fact of the deployment, not an operator setting: the
//! settings store cannot be allowed to switch off the guards it decides.
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
        match value.trim() {
            "" | "production" => Some(Self::Production),
            "staging" => Some(Self::Staging),
            "development" => Some(Self::Development),
            _ => None,
        }
    }
    /// Step-up (`Access::StepUp`) is enforced everywhere but development.
    pub fn requires_step_up(self) -> bool {
        self != Self::Development
    }
}
