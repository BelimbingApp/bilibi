//! Where step-up applies is policy, kept in Settings so every change is
//! validated and audited like any setting: the tenant's choice per declared
//! operation and its window, the installation floor and its window limits.
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, ScopeKind, Setting, Validator, ValueType, DEFINITIONS};
use serde_json::{json, Value};

/// Tenant: `{operation key: "off" | "on" | "every_time"}`; a missing key
/// follows the declared default.
pub const POLICY_KEY: &str = "step_up.policy";
/// Installation: `{operation key: true | false}`, overriding a declaration's
/// floor; a floor operation cannot be turned off by a tenant.
pub const FLOOR_KEY: &str = "step_up.floor";
/// Tenant: minutes a check covers after the last step-up action (sliding).
pub const WINDOW_KEY: &str = "step_up.window_minutes";
/// Tenant: hours after the check when it is asked again regardless (cap).
pub const CAP_KEY: &str = "step_up.cap_hours";
/// Installation maxima for the two tenant values.
pub const WINDOW_LIMIT_KEY: &str = "step_up.window_limit_minutes";
pub const CAP_LIMIT_KEY: &str = "step_up.cap_limit_hours";

pub const WINDOW_RANGE: (i64, i64) = (5, 720);
pub const CAP_RANGE: (i64, i64) = (1, 24);
/// How recent a check must be to cover one "ask every time" action.
pub const FRESH_SECONDS: i64 = 300;

/// What a tenant chose for one operation, or what its declaration defaults to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Off,
    /// Covered by a recent check (the session window on the web).
    On,
    /// A check for each action; a web check covers one action.
    EveryTime,
}
impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::EveryTime => "every_time",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "on" => Some(Self::On),
            "every_time" => Some(Self::EveryTime),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::On => "On",
            Self::EveryTime => "Ask every time",
        }
    }
}

const MAX_ENTRIES: usize = 1000;
fn keyed(value: &Value, accepts: fn(&Value) -> bool) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() <= MAX_ENTRIES
            && map
                .iter()
                .all(|(key, value)| !key.is_empty() && key.len() <= 128 && accepts(value))
    })
}
fn policy_accepts(value: &Value) -> bool {
    keyed(value, |mode| mode.as_str().and_then(Mode::parse).is_some())
}
fn floor_accepts(value: &Value) -> bool {
    keyed(value, Value::is_boolean)
}

macro_rules! setting {
    ($key:expr, $definition:expr) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, Provenance { module: "base/step_up", source: file!(), line: line!() }, &Setting::Definition($definition)) }
    };
}
// No generic editor: the step-up policy page edits these, behind step-up.
setting!(
    POLICY_KEY,
    Definition {
        value_type: ValueType::Mixed,
        scopes: &[ScopeKind::Tenant],
        default: || json!({}),
        nullable: false,
        encrypted: false,
        minimum: None,
        maximum: None,
        validator: Some(Validator {
            accepts: policy_accepts,
            message: "must map operation keys to off, on or every_time",
        }),
        editor: None,
    }
);
setting!(
    FLOOR_KEY,
    Definition {
        value_type: ValueType::Mixed,
        scopes: &[ScopeKind::Global],
        default: || json!({}),
        nullable: false,
        encrypted: false,
        minimum: None,
        maximum: None,
        validator: Some(Validator {
            accepts: floor_accepts,
            message: "must map operation keys to true or false",
        }),
        editor: None,
    }
);
macro_rules! bounded {
    ($key:expr, $scope:expr, $default:expr, $range:expr) => {
        setting!(
            $key,
            Definition {
                value_type: ValueType::Integer,
                scopes: &[$scope],
                default: || json!($default),
                nullable: false,
                encrypted: false,
                minimum: Some($range.0 as f64),
                maximum: Some($range.1 as f64),
                validator: None,
                editor: None,
            }
        );
    };
}
bounded!(WINDOW_KEY, ScopeKind::Tenant, 30, WINDOW_RANGE);
bounded!(CAP_KEY, ScopeKind::Tenant, 6, CAP_RANGE);
bounded!(WINDOW_LIMIT_KEY, ScopeKind::Global, 720, WINDOW_RANGE);
bounded!(CAP_LIMIT_KEY, ScopeKind::Global, 24, CAP_RANGE);
