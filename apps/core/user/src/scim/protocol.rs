//! RFC 7643/7644 subset for company provisioning; no Bilimbi equivalent.
use super::*;

pub(super) const USER: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
pub(super) const GROUP: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
pub(super) const LIST: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
const PATCH: &str = "urn:ietf:params:scim:api:messages:2.0:PatchOp";
pub(super) fn attr<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}
fn text(v: &Value, key: &str) -> String {
    attr(v, key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}
fn bounded(s: &str, max: usize) -> bool {
    s.len() <= max && !s.contains('\0')
}
pub(super) fn normalize(v: &Value, group: bool) -> ScimResult<Value> {
    if !v.is_object() {
        return Err(ScimError::invalid());
    }
    let schema = if group { GROUP } else { USER };
    if !attr(v, "schemas")
        .and_then(Value::as_array)
        .is_some_and(|a| a.contains(&json!(schema)))
    {
        return Err(ScimError::bad(
            "invalidValue",
            "Include the resource's core schema.",
        ));
    }
    let mut out = json!({"schemas":[schema]});
    if let Some(id) = attr(v, "id") {
        out["id"] = id.clone();
    }
    if let Some(external) = attr(v, "externalId") {
        let external = external.as_str().ok_or_else(ScimError::invalid)?;
        if !bounded(external, 255) {
            return Err(ScimError::invalid());
        }
        if !external.is_empty() {
            out["externalId"] = json!(external);
        }
    }
    if group {
        let name = text(v, "displayName");
        if name.trim().is_empty() || !bounded(&name, 512) {
            return Err(ScimError::invalid());
        }
        out["displayName"] = json!(name);
        let members = attr(v, "members").cloned().unwrap_or(json!([]));
        let members = members.as_array().ok_or_else(ScimError::invalid)?;
        if members.len() > 10000 {
            return Err(ScimError::invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        for member in members {
            let id = text(member, "value");
            if id.is_empty() || !bounded(&id, 64) {
                return Err(ScimError::invalid());
            }
            if attr(member, "type")
                .and_then(Value::as_str)
                .is_some_and(|t| !t.eq_ignore_ascii_case("User"))
            {
                return Err(ScimError::bad(
                    "invalidValue",
                    "Only direct User members are supported.",
                ));
            }
            ids.insert(id);
        }
        out["members"] = json!(ids
            .into_iter()
            .map(|id| json!({"value":id}))
            .collect::<Vec<_>>());
    } else {
        let username = crate::accounts::validate_email(&text(v, "userName"))
            .map_err(|_| ScimError::bad("invalidValue", "userName must be an email address."))?;
        let display = text(v, "displayName");
        let name = attr(v, "name").cloned().unwrap_or(json!({}));
        let formatted = text(&name, "formatted");
        let given = text(&name, "givenName");
        let family = text(&name, "familyName");
        let full = if !display.is_empty() {
            display.clone()
        } else if !formatted.is_empty() {
            formatted.clone()
        } else {
            format!("{given} {family}").trim().to_owned()
        };
        let full = if full.is_empty() {
            username.clone()
        } else {
            full
        };
        if crate::accounts::validate_name(&full).is_some()
            || [&given, &family, &formatted]
                .iter()
                .any(|s| !bounded(s, 160))
        {
            return Err(ScimError::invalid());
        }
        out["userName"] = json!(username);
        out["displayName"] = json!(full);
        out["name"] = json!({"formatted":formatted,"givenName":given,"familyName":family});
        out["active"] = match attr(v, "active") {
            None => json!(true),
            Some(Value::Bool(b)) => json!(b),
            Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => json!(true),
            Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => json!(false),
            _ => return Err(ScimError::invalid()),
        };
        // Bilibi has one personal mailbox. Preserve the supported work/primary entry.
        let emails = attr(v, "emails").cloned().unwrap_or(json!([]));
        let emails = emails.as_array().ok_or_else(ScimError::invalid)?;
        if emails.len() > 10 {
            return Err(ScimError::invalid());
        }
        let selected = emails
            .iter()
            .find(|e| attr(e, "primary") == Some(&json!(true)))
            .or_else(|| {
                emails
                    .iter()
                    .find(|e| text(e, "type").eq_ignore_ascii_case("work"))
            })
            .or_else(|| emails.first());
        if let Some(email) = selected {
            let email = crate::accounts::validate_email(&text(email, "value"))
                .map_err(|_| ScimError::invalid())?;
            out["emails"] = json!([{"value":email,"type":"work","primary":true}]);
        } else {
            out["emails"] = json!([]);
        }
    }
    Ok(out)
}
pub(super) fn mailbox(doc: &Value) -> String {
    doc["emails"][0]["value"]
        .as_str()
        .or_else(|| doc["userName"].as_str())
        .unwrap_or("")
        .to_owned()
}
fn canonical(path: &str, group: bool) -> Option<&'static str> {
    match path.to_ascii_lowercase().as_str() {
        "externalid" => Some("externalId"),
        "displayname" => Some("displayName"),
        "username" if !group => Some("userName"),
        "active" if !group => Some("active"),
        "name" if !group => Some("name"),
        "emails" if !group => Some("emails"),
        "members" if group => Some("members"),
        _ => None,
    }
}
pub(super) fn quoted_eq(s: &str) -> ScimResult<(String, String)> {
    let s = s.trim();
    let mut depth = 0_i32;
    let split = s
        .char_indices()
        .find_map(|(i, ch)| {
            match ch {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (depth == 0 && ch.is_whitespace()).then_some(i)
        })
        .ok_or_else(ScimError::filter)?;
    let (field, rest) = s.split_at(split);
    let rest = rest.trim_start();
    let (op, value) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(ScimError::filter)?;
    if !op.eq_ignore_ascii_case("eq") {
        return Err(ScimError::filter());
    }
    let value: Value = serde_json::from_str(value.trim()).map_err(|_| ScimError::filter())?;
    match value {
        Value::String(s) => Ok((field.to_ascii_lowercase(), s)),
        Value::Bool(b) => Ok((field.to_ascii_lowercase(), b.to_string())),
        _ => Err(ScimError::filter()),
    }
}
pub(super) fn patch(mut doc: Value, body: &Value, group: bool) -> ScimResult<Value> {
    if !attr(body, "schemas")
        .and_then(Value::as_array)
        .is_some_and(|a| a.contains(&json!(PATCH)))
    {
        return Err(ScimError::invalid());
    }
    let operations = attr(body, "Operations")
        .and_then(Value::as_array)
        .ok_or_else(ScimError::invalid)?;
    if operations.is_empty() || operations.len() > 100 {
        return Err(ScimError::invalid());
    }
    for operation in operations {
        let op = text(operation, "op").to_ascii_lowercase();
        if !matches!(op.as_str(), "add" | "replace" | "remove") {
            return Err(ScimError::invalid());
        }
        let path = text(operation, "path");
        let value = attr(operation, "value").cloned().unwrap_or(Value::Null);
        if path.is_empty() {
            if op == "remove" {
                return Err(ScimError::bad(
                    "noTarget",
                    "A remove operation needs a path.",
                ));
            }
            for (key, v) in value.as_object().ok_or_else(ScimError::invalid)? {
                apply(&mut doc, &op, key, v.clone(), group)?;
            }
        } else {
            apply(&mut doc, &op, &path, value, group)?;
        }
    }
    normalize(&doc, group)
}
fn apply(doc: &mut Value, op: &str, path: &str, value: Value, group: bool) -> ScimResult<()> {
    let lower = path.to_ascii_lowercase();
    if lower == "id" || lower == "schemas" || lower.starts_with("meta") || lower == "groups" {
        return Err(ScimError::bad("mutability", "This attribute is read-only."));
    }
    if group && lower.starts_with("members[") {
        let end = path.rfind(']').ok_or_else(ScimError::invalid)?;
        if end != path.len() - 1 || op != "remove" {
            return Err(ScimError::bad(
                "invalidPath",
                "Only filtered member removal is supported.",
            ));
        }
        let (field, id) = quoted_eq(&path[8..end])?;
        if field != "value" {
            return Err(ScimError::invalid());
        }
        doc["members"]
            .as_array_mut()
            .ok_or_else(ScimError::invalid)?
            .retain(|v| v["value"] != id);
        return Ok(());
    }
    if !group && lower.starts_with("name.") {
        let key = match &lower[5..] {
            "givenname" => "givenName",
            "familyname" => "familyName",
            "formatted" => "formatted",
            _ => return Err(ScimError::bad("invalidPath", "Unsupported name attribute.")),
        };
        doc["name"][key] = if op == "remove" { json!("") } else { value };
        return Ok(());
    }
    if !group
        && (lower == "emails.value"
            || lower == "emails[type eq \"work\"].value"
            || lower == "emails[primary eq true].value")
    {
        doc["emails"] = if op == "remove" {
            json!([])
        } else {
            json!([{"value":value,"type":"work","primary":true}])
        };
        return Ok(());
    }
    let key = canonical(path, group)
        .ok_or_else(|| ScimError::bad("invalidPath", "Unsupported attribute path."))?;
    if key == "members" {
        let values = value
            .as_array()
            .cloned()
            .or_else(|| value.is_object().then(|| vec![value.clone()]));
        match op {
            "replace" => doc[key] = json!(values.ok_or_else(ScimError::invalid)?),
            "add" => doc[key]
                .as_array_mut()
                .ok_or_else(ScimError::invalid)?
                .extend(values.ok_or_else(ScimError::invalid)?),
            "remove" => {
                if value.is_null() {
                    doc[key] = json!([]);
                } else {
                    let ids = values.ok_or_else(ScimError::invalid)?;
                    doc[key]
                        .as_array_mut()
                        .ok_or_else(ScimError::invalid)?
                        .retain(|m| !ids.iter().any(|v| attr(v, "value") == m.get("value")));
                }
            }
            _ => return Err(ScimError::invalid()),
        }
    } else if op == "remove" {
        if matches!(key, "active" | "userName" | "displayName") {
            return Err(ScimError::bad(
                "mutability",
                "A required account field cannot be removed.",
            ));
        }
        doc.as_object_mut()
            .ok_or_else(ScimError::invalid)?
            .remove(key);
    } else {
        doc[key] = value;
    }
    Ok(())
}
pub(super) fn project(
    mut doc: Value,
    query: &std::collections::HashMap<String, String>,
) -> ScimResult<Value> {
    if query.contains_key("attributes") && query.contains_key("excludedAttributes") {
        return Err(ScimError::invalid());
    }
    for (param, include) in [("attributes", true), ("excludedAttributes", false)] {
        if let Some(fields) = query.get(param) {
            let fields = fields
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect::<Vec<_>>();
            if fields.iter().any(|s| s.is_empty() || s.contains('.')) {
                return Err(ScimError::bad(
                    "invalidValue",
                    "Select whole resource attributes.",
                ));
            }
            doc.as_object_mut()
                .ok_or_else(ScimError::invalid)?
                .retain(|key, _| {
                    matches!(key.as_str(), "id" | "schemas")
                        || fields.contains(&key.to_ascii_lowercase()) == include
                });
        }
    }
    Ok(doc)
}
pub(super) fn discovery(resource: &str, id: Option<&str>) -> ScimResult<Option<Value>> {
    let types = vec![
        json!({"schemas":["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],"id":"User","name":"User","endpoint":"/Users","schema":USER}),
        json!({"schemas":["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],"id":"Group","name":"Group","endpoint":"/Groups","schema":GROUP}),
    ];
    let attribute = |name: &str, kind: &str, multi: bool, required: bool, mutable: &str| json!({"name":name,"type":kind,"multiValued":multi,"required":required,"caseExact":false,"mutability":mutable,"returned":"default","uniqueness":"none"});
    let mut name = attribute("name", "complex", false, false, "readWrite");
    name["subAttributes"] = json!([
        attribute("formatted", "string", false, false, "readWrite"),
        attribute("givenName", "string", false, false, "readWrite"),
        attribute("familyName", "string", false, false, "readWrite")
    ]);
    let mut emails = attribute("emails", "complex", true, false, "readWrite");
    emails["subAttributes"] = json!([
        attribute("value", "string", false, true, "readWrite"),
        attribute("type", "string", false, false, "readWrite"),
        attribute("primary", "boolean", false, false, "readWrite")
    ]);
    let mut members = attribute("members", "complex", true, false, "readWrite");
    members["subAttributes"] = json!([
        attribute("value", "string", false, true, "immutable"),
        attribute("display", "string", false, false, "readOnly"),
        attribute("$ref", "reference", false, false, "immutable")
    ]);
    let schemas = vec![
        json!({"schemas":["urn:ietf:params:scim:schemas:core:2.0:Schema"],"id":USER,"name":"User","attributes":[attribute("userName","string",false,true,"readWrite"),attribute("displayName","string",false,false,"readWrite"),attribute("active","boolean",false,false,"readWrite"),name,emails]}),
        json!({"schemas":["urn:ietf:params:scim:schemas:core:2.0:Schema"],"id":GROUP,"name":"Group","attributes":[attribute("displayName","string",false,true,"readWrite"),members]}),
    ];
    let values = match resource {
        "ServiceProviderConfig" if id.is_none() => {
            return Ok(Some(
                json!({"schemas":["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],"patch":{"supported":true},"bulk":{"supported":false,"maxOperations":0,"maxPayloadSize":0},"filter":{"supported":true,"maxResults":200},"changePassword":{"supported":false},"sort":{"supported":false},"etag":{"supported":false},"authenticationSchemes":[{"type":"oauthbearertoken","name":"Company bearer token","description":"Company-scoped provisioning credential","primary":true}]}),
            ))
        }
        "ResourceTypes" => types,
        "Schemas" => schemas,
        _ => return Ok(None),
    };
    if let Some(id) = id {
        return values
            .into_iter()
            .find(|v| v["id"] == id)
            .map(Some)
            .ok_or_else(ScimError::missing);
    }
    Ok(Some(
        json!({"schemas":[LIST],"totalResults":values.len(),"startIndex":1,"itemsPerPage":values.len(),"Resources":values}),
    ))
}
