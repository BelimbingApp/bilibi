//! The field plan and save semantics behind the editor (Bilimbi
//! `Settings.Form`). The screen names no fields: every contributed definition
//! with editor metadata and the selected target's scope (global, the tenant,
//! or company once a company is picked) is one, so a module that declares a
//! setting gets it on screen with no UI change.
//!
//! Three rules are easy to get wrong. Clearing is not writing an empty value:
//! a blank submission deletes this scope's override so the value inherits
//! again. Inherited is not the same as set here: `overridden` says which, and
//! the screen shows it. An encrypted value never reaches the browser: a stored
//! secret renders the keep-current mask, and a submission still equal to that
//! mask means "unchanged".
use bilibi_authz::effective_capabilities;
use bilibi_database::Result;
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, Settings, Target, ValueType};
use bilibi_tenancy::Scope;
use bilibi_ui::SECRET_MASK;
use serde_json::Value;
use std::collections::BTreeSet;
use tokio_postgres::GenericClient;

/// One rendered field. `value` is null for an encrypted field: storage is
/// read masked, so plaintext never enters the plan.
pub struct Field {
    pub key: &'static str,
    pub module: &'static str,
    pub definition: &'static Definition,
    pub editor: &'static Editor,
    pub value: Value,
    pub overridden: bool,
    pub stored_secret: bool,
    /// Where an inherited value comes from; `None` is the declared default.
    pub source: Option<ScopeKind>,
}
impl Field {
    pub fn dom(&self) -> String {
        dom(self.key)
    }
    pub fn encrypted(&self) -> bool {
        self.definition.encrypted
    }
}
pub fn dom(key: &str) -> String {
    key.replace('.', "-")
}

/// The fields the actor may see, and the capabilities of those withheld. A
/// field without a grant is withheld here, not by the template, so forged
/// submissions for it are ignored too and the page can say what it withheld.
pub struct Plan {
    pub fields: Vec<Field>,
    pub withheld: BTreeSet<&'static str>,
    pub capabilities: BTreeSet<&'static str>,
}
impl Plan {
    pub fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.key == key)
    }
}

/// One fresh read: capabilities without decision-log rows (a hidden field is
/// not an access attempt) and every value in one masked storage snapshot.
pub async fn plan(
    client: &(impl GenericClient + Sync),
    settings: &Settings,
    scope: &Scope,
    target: Target,
) -> Result<Plan> {
    // The company target shows company-scoped definitions only (Bilimbi
    // `company_groups/0`), the tenant target tenant-scoped ones, and the
    // global page global ones.
    let wanted = match target {
        Target::Company(_) => ScopeKind::Company,
        Target::Tenant => ScopeKind::Tenant,
        _ => ScopeKind::Global,
    };
    let capabilities = effective_capabilities(client, scope).await?;
    let mut visible = Vec::new();
    let mut withheld = BTreeSet::new();
    for owned in settings.definitions().all() {
        let Setting::Definition(definition) = owned.setting else {
            continue;
        };
        let Some(editor) = definition.editor.as_ref() else {
            continue;
        };
        if owned.pattern.contains('*') || !definition.scopes.contains(&wanted) {
            continue;
        }
        match editor.capability {
            Some(capability) if !capabilities.contains(capability) => {
                withheld.insert(capability);
            }
            _ => visible.push((owned.pattern, owned.owner, definition, editor)),
        }
    }
    let keys: Vec<&str> = visible.iter().map(|(key, _, _, _)| *key).collect();
    let mut resolved = settings
        .resolve_masked(client, scope, target, &keys)
        .await?;
    let fields = visible
        .into_iter()
        .filter_map(|(key, module, definition, editor)| {
            let resolved = resolved.remove(key)?;
            Some(Field {
                key,
                module,
                definition,
                editor,
                stored_secret: definition.encrypted
                    && (resolved.source.is_some() || present(&resolved.value)),
                value: if definition.encrypted {
                    Value::Null
                } else {
                    resolved.value
                },
                overridden: resolved.overridden,
                source: resolved.source.map(|source| source.kind),
            })
        })
        .collect();
    Ok(Plan {
        fields,
        withheld,
        capabilities,
    })
}

fn present(value: &Value) -> bool {
    !(value.is_null() || value.as_str() == Some(""))
}

/// The text a control shows for a non-secret value.
pub fn input_value(field: &Field) -> String {
    match &field.value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        value @ (Value::Array(_) | Value::Object(_))
            if field.definition.value_type == ValueType::Array =>
        {
            value.to_string()
        }
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        value => value.to_string(),
    }
}

pub enum Step {
    Skip,
    Clear,
    Write(Value),
}

/// Decides one submitted field without touching storage. A blank on an
/// inherited field and a value equal to what the field already resolves to are
/// both `Skip`: neither changes anything, and writing an inherited value would
/// pin an override that silently stops following its default.
pub fn decide(field: &Field, submitted: &str) -> std::result::Result<Step, String> {
    let definition = field.definition;
    if field.encrypted() && submitted == SECRET_MASK {
        return Ok(Step::Skip);
    }
    if submitted.trim().is_empty() && definition.value_type != ValueType::Boolean {
        return Ok(if field.overridden {
            Step::Clear
        } else {
            Step::Skip
        });
    }
    let value = cast(submitted, definition)?;
    if !field.encrypted() && value == field.value {
        return Ok(Step::Skip);
    }
    Ok(Step::Write(value))
}

/// Converts browser text to the declared type and validates it, refusing
/// rather than coercing: "abc" is never saved as 0.
pub fn cast(submitted: &str, definition: &Definition) -> std::result::Result<Value, String> {
    let text = submitted.trim();
    let value = match definition.value_type {
        ValueType::Integer => text
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| "must be a whole number")?,
        ValueType::Float => text
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .map(Value::from)
            .ok_or("must be a number")?,
        ValueType::Boolean => match text.to_ascii_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => Value::Bool(true),
            "false" | "0" | "off" | "no" => Value::Bool(false),
            _ => return Err("must be true or false".into()),
        },
        // Structured settings travel as JSON across the browser's text boundary.
        ValueType::Array => match serde_json::from_str::<Value>(text) {
            Ok(value @ (Value::Array(_) | Value::Object(_))) => value,
            _ => return Err("must be a JSON object or array".into()),
        },
        ValueType::String | ValueType::Mixed => Value::String(text.to_owned()),
    };
    if matches!(definition.value_type, ValueType::Integer | ValueType::Float) {
        let number = value.as_f64().unwrap_or_default();
        if definition.minimum.is_some_and(|min| number < min)
            || definition.maximum.is_some_and(|max| number > max)
        {
            return Err("must be within the configured range".into());
        }
    }
    definition.validate(&value).map_err(str::to_owned)?;
    Ok(value)
}

/// "1 setting", "3 overrides"; `None` for nothing, so a message omits it.
pub fn count(n: usize, noun: &str) -> Option<String> {
    match n {
        0 => None,
        1 => Some(format!("1 {noun}")),
        n => Some(format!("{n} {noun}s")),
    }
}

/// Say what happened, not "Settings saved": a save that only cleared
/// overrides otherwise looks like a no-op.
pub fn saved_message(written: usize, cleared: usize) -> String {
    let parts: Vec<String> = [
        (count(written, "setting"), "updated"),
        (count(cleared, "override"), "cleared"),
    ]
    .into_iter()
    .filter_map(|(phrase, verb)| phrase.map(|phrase| format!("{phrase} {verb}")))
    .collect();
    if parts.is_empty() {
        "No changes to save.".into()
    } else {
        format!("{}.", parts.join(", "))
    }
}

/// "Shown", not "every setting": restore reaches only the fields this account
/// may see, and a withheld field may still hold an override.
pub fn restored_message(cleared: usize) -> String {
    match count(cleared, "override") {
        None => "Nothing to restore; every setting shown is already inherited.".into(),
        Some(phrase) => format!("{phrase} cleared. Values now come from what they inherit."),
    }
}
