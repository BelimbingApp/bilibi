//! Approval routing extends Bilimbi Workflow human actions; policies are
//! bounded company data, never editable process graphs or executable scripts.
use bilibi_database::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields, rename_all = "snake_case")]
pub enum ApprovalFact {
    Boolean { value: bool },
    Code { value: String },
    Identifier { value: i64 },
    Decimal { value: String },
    Money { currency: String, value: String },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalFactKind {
    Boolean,
    Code,
    Identifier,
    Decimal,
    Money,
}
impl ApprovalFact {
    pub(crate) fn kind(&self) -> ApprovalFactKind {
        match self {
            Self::Boolean { .. } => ApprovalFactKind::Boolean,
            Self::Code { .. } => ApprovalFactKind::Code,
            Self::Identifier { .. } => ApprovalFactKind::Identifier,
            Self::Decimal { .. } => ApprovalFactKind::Decimal,
            Self::Money { .. } => ApprovalFactKind::Money,
        }
    }
    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Boolean { .. } => true,
            Self::Code { value } => label(value),
            Self::Identifier { value } => *value > 0,
            Self::Decimal { value } => decimal(value).is_ok(),
            Self::Money { currency, value } => {
                currency.len() == 3
                    && currency.bytes().all(|c| c.is_ascii_uppercase())
                    && decimal(value).is_ok()
            }
        }
    }
}

/// Exact decimal comparison without floating point, implicit exchange rates,
/// rounding, or machine-integer overflow. At most 38 digits and 18 decimals.
fn decimal(value: &str) -> Result<(bool, String, String)> {
    let (negative, raw) = value
        .strip_prefix('-')
        .map_or((false, value), |v| (true, v));
    let parts: Vec<_> = raw.split('.').collect();
    if parts.is_empty()
        || parts.len() > 2
        || parts[0].is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()))
        || parts.iter().map(|p| p.len()).sum::<usize>() > 38
        || parts.get(1).is_some_and(|p| p.len() > 18)
    {
        return Err(Error::Invalid);
    }
    let whole = parts[0].trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole }.to_owned();
    let fraction = parts.get(1).unwrap_or(&"").trim_end_matches('0').to_owned();
    Ok((
        negative && (whole != "0" || !fraction.is_empty()),
        whole,
        fraction,
    ))
}
fn compare(a: &str, b: &str) -> Result<std::cmp::Ordering> {
    let (an, aw, mut af) = decimal(a)?;
    let (bn, bw, mut bf) = decimal(b)?;
    let n = af.len().max(bf.len());
    af.extend(std::iter::repeat_n('0', n - af.len()));
    bf.extend(std::iter::repeat_n('0', n - bf.len()));
    let magnitude = aw
        .len()
        .cmp(&bw.len())
        .then_with(|| aw.cmp(&bw))
        .then_with(|| af.cmp(&bf));
    Ok(match (an, bn) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (true, true) => magnitude.reverse(),
        _ => magnitude,
    })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields, rename_all = "snake_case")]
pub enum ApprovalSelector {
    Capability,
    Role {
        id: i64,
    },
    Person {
        account: i64,
    },
    /// An owner-proved relationship key, resolved again at every decision.
    Relationship {
        key: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalSeat {
    pub key: String,
    pub label: String,
    pub capability: String,
    pub selector: ApprovalSelector,
    pub delegation: bool,
    pub reminder_hours: Option<u32>,
    pub escalation_hours: Option<u32>,
    pub escalation: Option<ApprovalSelector>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    All,
    Any,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalStage {
    pub key: String,
    pub label: String,
    pub mode: ApprovalMode,
    pub seats: Vec<ApprovalSeat>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalComparison {
    Equal,
    Greater,
    AtLeast,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalCondition {
    pub fact: String,
    pub comparison: ApprovalComparison,
    pub value: ApprovalFact,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRule {
    pub when: Vec<ApprovalCondition>,
    pub stages: Vec<ApprovalStage>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalPolicy {
    pub stages: Vec<ApprovalStage>,
    pub rules: Vec<ApprovalRule>,
    pub distinct_stages: bool,
    pub approval_reason: bool,
    /// Only an owner explicitly permitting a separation exception may set it.
    pub allow_preparer: bool,
}
pub(crate) fn label(v: &str) -> bool {
    !v.trim().is_empty() && v.len() <= 255 && !v.contains('\0')
}
impl ApprovalPolicy {
    pub(crate) fn validate(&self, case: &crate::ApprovalCase) -> Result<()> {
        if self.stages.is_empty()
            || self.stages.len() > 32
            || self.rules.len() > 64
            || (case.distinct_stages && !self.distinct_stages)
            || (case.approval_reason && !self.approval_reason)
            || (self.allow_preparer && !case.allow_preparer)
        {
            return Err(Error::Invalid);
        }
        let mut keys = BTreeMap::new();
        for stage in self
            .stages
            .iter()
            .chain(self.rules.iter().flat_map(|r| &r.stages))
        {
            if !crate::definitions::key(&stage.key)
                || !label(&stage.label)
                || stage.seats.is_empty()
                || stage.seats.len() > 32
            {
                return Err(Error::Invalid);
            }
            if let Some(old) = keys.insert(stage.key.clone(), stage) {
                if old != stage {
                    return Err(Error::Invalid);
                }
            }
            let mut seats = BTreeSet::new();
            for seat in &stage.seats {
                if !crate::definitions::key(&seat.key)
                    || !label(&seat.label)
                    || !seats.insert(&seat.key)
                    || !case
                        .decisions
                        .iter()
                        .any(|a| a.capability == seat.capability)
                    || !valid_selector(&seat.selector, case)
                    || seat
                        .escalation
                        .as_ref()
                        .is_some_and(|s| !valid_selector(s, case))
                    || seat.reminder_hours.is_some_and(|h| h == 0 || h > 87600)
                    || seat.escalation_hours.is_some_and(|h| h == 0 || h > 87600)
                    || seat.escalation.is_some() != seat.escalation_hours.is_some()
                    || seat
                        .reminder_hours
                        .zip(seat.escalation_hours)
                        .is_some_and(|(r, e)| r >= e)
                    || (seat.delegation && !case.delegation)
                {
                    return Err(Error::Invalid);
                }
            }
        }
        for rule in &self.rules {
            if rule.when.is_empty() || rule.when.len() > 16 || rule.stages.is_empty() {
                return Err(Error::Invalid);
            }
            for c in &rule.when {
                if !case
                    .facts
                    .iter()
                    .any(|(k, t)| *k == c.fact && *t == c.value.kind())
                    || !c.value.valid()
                    || (!matches!(c.comparison, ApprovalComparison::Equal)
                        && !matches!(
                            c.value,
                            ApprovalFact::Decimal { .. } | ApprovalFact::Money { .. }
                        ))
                {
                    return Err(Error::Invalid);
                }
            }
        }
        // The unconditional base is itself an approval: conditions can add
        // controls, but can never remove the owner's required decision.
        for required in case.required_capabilities {
            if !self.stages.iter().any(|s| {
                (s.mode == ApprovalMode::All
                    && s.seats.iter().any(|seat| seat.capability == *required))
                    || (s.mode == ApprovalMode::Any
                        && s.seats.iter().all(|seat| seat.capability == *required))
            }) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
    pub(crate) fn route(
        &self,
        facts: &BTreeMap<String, ApprovalFact>,
    ) -> Result<Vec<ApprovalStage>> {
        let mut stages = self.stages.clone();
        for (key, actual) in facts {
            if let ApprovalFact::Money { currency, .. } = actual {
                let currencies = self
                    .rules
                    .iter()
                    .flat_map(|r| &r.when)
                    .filter(|c| c.fact == *key)
                    .filter_map(|c| {
                        if let ApprovalFact::Money { currency, .. } = &c.value {
                            Some(currency)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                if !currencies.is_empty() && !currencies.contains(&currency) {
                    return Err(Error::Invalid);
                }
            }
        }
        for rule in &self.rules {
            // Evaluate every condition even after a mismatch: missing facts
            // and mixed currencies are routing errors, never skipped controls.
            let mut matches = true;
            for c in &rule.when {
                let actual = facts.get(&c.fact).ok_or(Error::Invalid)?;
                if !actual.valid() {
                    return Err(Error::Invalid);
                }
                let order = match (actual, &c.value) {
                    (ApprovalFact::Decimal { value: a }, ApprovalFact::Decimal { value: b }) => {
                        compare(a, b)?
                    }
                    (
                        ApprovalFact::Money {
                            currency: a,
                            value: av,
                        },
                        ApprovalFact::Money {
                            currency: b,
                            value: bv,
                        },
                    ) => {
                        if a != b {
                            matches = false;
                            continue;
                        }
                        compare(av, bv)?
                    }
                    (a, b) if a.kind() == b.kind() => {
                        if a == b {
                            std::cmp::Ordering::Equal
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    _ => return Err(Error::Invalid),
                };
                matches &= match c.comparison {
                    ApprovalComparison::Equal => order.is_eq(),
                    ApprovalComparison::Greater => order.is_gt(),
                    ApprovalComparison::AtLeast => order.is_ge(),
                };
            }
            if matches {
                for stage in &rule.stages {
                    if !stages.iter().any(|s| s.key == stage.key) {
                        stages.push(stage.clone());
                    }
                }
            }
        }
        if stages.len() > 32 {
            return Err(Error::Invalid);
        }
        Ok(stages)
    }
    /// Plain explanation shown before a rule is published. Labels are data
    /// and must still pass the renderer's normal escaping.
    pub fn explanation(&self) -> String {
        fn stages(items: &[ApprovalStage]) -> String {
            items
                .iter()
                .map(|s| {
                    format!(
                        "{}: {} {}",
                        s.label,
                        if s.mode == ApprovalMode::All {
                            "all of"
                        } else {
                            "any one of"
                        },
                        s.seats
                            .iter()
                            .map(|s| s.label.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; then ")
        }
        let mut words = format!("Approval needs {}.", stages(&self.stages));
        for rule in &self.rules {
            let conditions = rule
                .when
                .iter()
                .map(|c| {
                    let value = match &c.value {
                        ApprovalFact::Money { currency, value } => format!("{currency} {value}"),
                        ApprovalFact::Decimal { value } | ApprovalFact::Code { value } => {
                            value.clone()
                        }
                        ApprovalFact::Boolean { value } => value.to_string(),
                        ApprovalFact::Identifier { value } => value.to_string(),
                    };
                    format!(
                        "{} {} {}",
                        c.fact.replace('_', " "),
                        match c.comparison {
                            ApprovalComparison::Equal => "is",
                            ApprovalComparison::Greater => "is over",
                            ApprovalComparison::AtLeast => "is at least",
                        },
                        value
                    )
                })
                .collect::<Vec<_>>()
                .join(" and ");
            words.push_str(&format!(
                " When {conditions}, also require {}.",
                stages(&rule.stages)
            ));
        }
        words.push_str(" Any rejection ends the whole request and needs a reason. Waiting never gives approval.");
        words
    }
}
fn valid_selector(selector: &ApprovalSelector, case: &crate::ApprovalCase) -> bool {
    match selector {
        ApprovalSelector::Capability => true,
        ApprovalSelector::Role { id } => *id > 0,
        ApprovalSelector::Person { account } => *account > 0,
        ApprovalSelector::Relationship { key } => case.relationships.contains(&key.as_str()),
    }
}
