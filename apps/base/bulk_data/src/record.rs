//! The opt-in contribution (issue 133): one entry per record type a module
//! lets people import, export or update in bulk, naming the contracts that do
//! the work. The fields a column can map to are data, not code: the top-level
//! scalar properties of the create contract's input schema; the fields a bulk
//! update may set are scalar properties of the update contract's fields that
//! the owner names. Boot refuses an unknown or foreign contract, a write a job
//! cannot call (one that declares step-up or leaves out the job door), an
//! undeclared or foreign capability, a match field, reference, parent or bulk
//! field that does not exist, and a list contract a job cannot page through.
use bilibi_agent_api::{job::JobContracts, RegisteredOperation};
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError};
use serde_json::Value;
use std::collections::BTreeMap;

pub const RECORD_TYPES: &str = "base/bulk_data.record_types";

/// A record type people may import, contributed by its owner with
/// `Contribution::new(RECORD_TYPES, "<module>.<name>", …)`; that id is the
/// type's key in runs and saved mappings, so keep it stable.
#[derive(Debug)]
pub struct RecordType {
    /// What the records are called, plural: "Contacts".
    pub label: &'static str,
    /// The owner's Grid table: a field restricted there for the person is
    /// not offered for mapping.
    pub table: &'static str,
    /// The record's page with `{id}`, for links from row results.
    pub path: Option<&'static str>,
    pub import: Import,
    /// Taking a list's records out to CSV; `None` when the type offers none.
    pub export: Option<Export>,
    /// Changing one field on many records; `None` when the type offers none.
    pub bulk_update: Option<BulkUpdate>,
}

/// Setting one field on many records at once: the selected records, or every
/// record a list's filters match (resolved once, when the update starts),
/// each through the owner's update contract as the person, so a record
/// changed since then is refused by the owner's version fence.
#[derive(Debug)]
pub struct BulkUpdate {
    /// The owner's capability to update this type in bulk (verb
    /// `bulk-update`), from its own vocabulary; checked again for every
    /// record, beside the update contract's own.
    pub capability: &'static str,
    /// The owner's list read contract, open to the job door, taking integer
    /// `page` and `page_size` and answering records with their id under
    /// [`Import::id`] and their `version`.
    pub list: &'static str,
    /// The owner's read of one record by `get_id`, answering it with its
    /// `version`; hidden fields read as `{"restricted": true}`.
    pub get: &'static str,
    pub get_id: &'static str,
    /// The owner's update contract with its id, version and fields
    /// properties, as in [`Duplicates`]: the record's other fields keep the
    /// values it reads, and a hidden one is left out.
    pub update: &'static str,
    pub update_id: &'static str,
    pub update_version: &'static str,
    pub update_fields: &'static str,
    /// The fields people may set in bulk: choices, dates, flags, numbers or
    /// an owner's id, each a scalar property of the update's fields.
    pub fields: &'static [&'static str],
}

/// Taking a list's filtered records out to CSV, with the columns shown.
#[derive(Debug)]
pub struct Export {
    /// The owner's capability to export this type (verb `export`), from its
    /// own vocabulary; checked again when the export runs.
    pub capability: &'static str,
    /// The owner's list read contract, open to the job door: it takes the
    /// list's filters with integer `page` (from 1) and `page_size`, and
    /// answers one page of records (an array), each with its id under
    /// [`Import::id`].
    pub list: &'static str,
}

#[derive(Debug)]
pub struct Import {
    /// The owner's capability to import this type, from its own vocabulary;
    /// checked again for every row, beside each contract's own.
    pub capability: &'static str,
    /// The owner's write contract creating one record.
    pub create: &'static str,
    /// Where a record's id is in the create result and in a duplicate
    /// candidate.
    pub id: &'static str,
    pub duplicates: Option<Duplicates>,
    pub references: &'static [Reference],
}

/// Matching a row to an existing record, and updating it.
#[derive(Debug)]
pub struct Duplicates {
    /// The owner's read contract answering candidate records (an array).
    pub read: &'static str,
    /// Create fields passed to it under the same names; a row with none of
    /// them matches nothing.
    pub fields: &'static [&'static str],
    /// The owner's update contract.
    pub update: &'static str,
    /// Its record id and version properties, filled from the candidate's
    /// id and `version`.
    pub update_id: &'static str,
    pub update_version: &'static str,
    /// The property holding the record's fields, or `""` when they sit
    /// beside the id and version. A field the file leaves out keeps the
    /// candidate's value.
    pub update_fields: &'static str,
}

/// A create field holding another record's id.
#[derive(Debug)]
pub struct Reference {
    pub field: &'static str,
    /// A record type (its key) whose earlier import this field may name by
    /// that run's source key column: parent then child.
    pub parent: Option<&'static str>,
    /// A read contract finding the record by one value; it must answer
    /// exactly one record.
    pub lookup: Option<Lookup>,
}

#[derive(Debug)]
pub struct Lookup {
    pub read: &'static str,
    /// The read's input property the cell's value goes in.
    pub by: &'static str,
    /// What the looked-up records are called, singular: "company".
    pub noun: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Text,
    Date,
    Integer,
    Number,
    Boolean,
}

/// A field a column (or a fixed value) can fill.
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub kind: Kind,
    pub required: bool,
    pub choices: Vec<String>,
    pub reference: Option<&'static Reference>,
}

/// A record type as boot accepted it.
pub(crate) struct Registered {
    pub key: &'static str,
    pub definition: &'static RecordType,
    pub fields: Vec<Field>,
    /// The update contract's field properties.
    pub update_fields: Vec<String>,
    /// The fields a bulk update may set, and every field property of its
    /// update contract (kept from the record when not the one set).
    pub bulk_fields: Vec<Field>,
    pub bulk_update_fields: Vec<String>,
    /// Read contracts that take a `limit`, asked for two so several matches show.
    pub limited: BTreeMap<&'static str, bool>,
}

fn invalid(entry: &Entry, reason: impl std::fmt::Display) -> RegistryError {
    RegistryError::Contribution(format!(
        "import record type {} at {}:{} {reason}",
        entry.id, entry.provenance.source, entry.provenance.line
    ))
}

fn shape(entry: &Entry) -> bilibi_module_registry::Result<()> {
    let record = entry.get::<RecordType>()?;
    let name = entry
        .id
        .strip_prefix(entry.provenance.module)
        .and_then(|rest| rest.strip_prefix('.'));
    let name_valid = name.is_some_and(|name| {
        !name.is_empty()
            && name.len() <= 60
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
    });
    let text = |value: &str, max: usize| !value.trim().is_empty() && value.chars().count() <= max;
    if !name_valid
        || !text(record.label, 80)
        || !text(record.table, 200)
        || record
            .path
            .is_some_and(|path| !path.starts_with('/') || path.matches("{id}").count() != 1)
        || !text(record.import.id, 100)
        || record.import.references.iter().any(|reference| {
            reference.parent.is_none() && reference.lookup.is_none()
                || reference
                    .lookup
                    .as_ref()
                    .is_some_and(|lookup| !text(lookup.noun, 60) || !text(lookup.by, 100))
        })
    {
        return Err(invalid(entry, "is malformed: name it <module>.<name>, give a label, table, id property, a path with one {id}, and a parent or lookup for each reference"));
    }
    Ok(())
}
inventory::submit! { Consumer::new::<RecordType>(RECORD_TYPES, "base/bulk_data", |entries| entries.iter().try_for_each(shape)) }

fn properties(schema: &Value) -> Option<&serde_json::Map<String, Value>> {
    schema.get("properties")?.as_object()
}
fn required(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
/// "work_email" reads "Work email".
fn label(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
/// A contract property as a field a value can fill.
fn field(name: &str, schema: &Value, required: bool) -> Option<Field> {
    Some(Field {
        name: name.to_owned(),
        label: schema
            .get("title")
            .and_then(Value::as_str)
            .map_or_else(|| label(name), str::to_owned),
        kind: kind(schema)?,
        required,
        choices: schema
            .get("enum")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        reference: None,
    })
}
/// Whether a list read takes integer `page` and `page_size`.
fn paged(list: &RegisteredOperation) -> bool {
    properties(&list.input).is_some_and(|p| {
        ["page", "page_size"].iter().all(|name| {
            p.get(*name)
                .and_then(|schema| schema.get("type"))
                .and_then(Value::as_str)
                == Some("integer")
        })
    })
}
/// An update contract's fields: the object under `fields`, or the input
/// itself without its id and version when `fields` is empty.
fn update_holder<'u>(
    update: &'u RegisteredOperation,
    id: &str,
    version: &str,
    fields: &str,
) -> Option<(&'u serde_json::Map<String, Value>, Vec<String>)> {
    let top = properties(&update.input)?;
    if !top.contains_key(id) || !top.contains_key(version) {
        return None;
    }
    let holder = if fields.is_empty() {
        top
    } else {
        properties(top.get(fields)?)?
    };
    let names = holder
        .keys()
        .filter(|name| !fields.is_empty() || (*name != id && *name != version))
        .cloned()
        .collect();
    Some((holder, names))
}
fn kind(schema: &Value) -> Option<Kind> {
    match schema.get("type")?.as_str()? {
        "string" if schema.get("format").and_then(Value::as_str) == Some("date") => {
            Some(Kind::Date)
        }
        "string" => Some(Kind::Text),
        "integer" => Some(Kind::Integer),
        "number" => Some(Kind::Number),
        "boolean" => Some(Kind::Boolean),
        _ => None,
    }
}

/// Checks every contributed type against the contracts and vocabulary.
pub(crate) fn register(
    catalog: &Catalog,
    jobs: &JobContracts,
) -> bilibi_module_registry::Result<BTreeMap<&'static str, Registered>> {
    let vocabulary = bilibi_authz::vocabulary::Vocabulary::from_catalog(catalog)?;
    let entries = catalog.entries::<RecordType>(RECORD_TYPES)?;
    let mut types = BTreeMap::new();
    for entry in entries {
        shape(entry)?;
        let record = entry.get::<RecordType>()?;
        let module = entry.provenance.module;
        let own = |operation: &RegisteredOperation, what: &str| {
            if operation.provenance.module == module {
                Ok(())
            } else {
                Err(invalid(
                    entry,
                    format!(
                        "names {what} {} of another module",
                        operation.definition.key
                    ),
                ))
            }
        };
        let writable = |key: &str| {
            jobs.writable(key).map_err(|error| {
                invalid(entry, format!("names {key}, which a queued job cannot call ({error}): it must be a registered reviewed write, open to the job door, with no step-up"))
            })
        };
        let readable = |key: &str| {
            jobs.readable(key).map_err(|error| {
                invalid(
                    entry,
                    format!(
                        "names {key}, which is not a read contract open to the job door ({error})"
                    ),
                )
            })
        };
        let capability = |key: &str| {
            match vocabulary.capabilities.get(key) {
            Some(owned)
                if owned.provenance.module == module
                    && owned.definition.audience == bilibi_authz::Audience::Tenant =>
            {
                Ok(())
            }
            _ => Err(invalid(
                entry,
                format!("names capability {key}, which {module} does not declare as a tenant capability"),
            )),
        }
        };
        capability(record.import.capability)?;
        if let Some(export) = &record.export {
            capability(export.capability)?;
            let list = readable(export.list)?;
            own(list, "list contract")?;
            if !paged(list) {
                return Err(invalid(
                    entry,
                    format!(
                        "names list contract {}, which does not take integer page and page_size",
                        export.list
                    ),
                ));
            }
        }
        let create = writable(record.import.create)?;
        own(create, "create contract")?;
        let create_properties = properties(&create.input).ok_or_else(|| {
            invalid(
                entry,
                "names a create contract whose input has no properties",
            )
        })?;
        let mandatory = required(&create.input);
        let mut fields: Vec<Field> = create_properties
            .iter()
            .filter_map(|(name, schema)| field(name, schema, mandatory.contains(&name.as_str())))
            .collect();
        // Required fields first, then by name.
        fields.sort_by(|a, b| b.required.cmp(&a.required).then(a.name.cmp(&b.name)));
        if let Some(missing) = mandatory
            .iter()
            .find(|name| !fields.iter().any(|field| field.name == **name))
        {
            return Err(invalid(
                entry,
                format!(
                    "cannot import: required field {missing} is not a scalar a column can fill"
                ),
            ));
        }
        let mut limited = BTreeMap::new();
        let mut take_limit = |operation: &RegisteredOperation| {
            let limit = properties(&operation.input).is_some_and(|p| p.contains_key("limit"));
            limited.insert(operation.definition.key, limit);
        };
        let mut update_fields = Vec::new();
        if let Some(duplicates) = &record.import.duplicates {
            let read = readable(duplicates.read)?;
            own(read, "duplicates read")?;
            take_limit(read);
            let read_properties = properties(&read.input).cloned().unwrap_or_default();
            if duplicates.fields.is_empty()
                || duplicates.fields.iter().any(|name| {
                    !fields.iter().any(|field| field.name == *name)
                        || !read_properties.contains_key(*name)
                })
            {
                return Err(invalid(
                    entry,
                    "matches on a field the create contract or the duplicates read does not take",
                ));
            }
            let update = writable(duplicates.update)?;
            own(update, "update contract")?;
            let update_properties = properties(&update.input).ok_or_else(|| {
                invalid(
                    entry,
                    "names an update contract whose input has no properties",
                )
            })?;
            if update_properties.is_empty() {
                return Err(invalid(
                    entry,
                    "names an update contract whose input has no properties",
                ));
            }
            let Some((_, names)) = update_holder(
                update,
                duplicates.update_id,
                duplicates.update_version,
                duplicates.update_fields,
            ) else {
                return Err(invalid(
                    entry,
                    format!(
                        "names an id, version or fields property ({:?}) {} does not take",
                        duplicates.update_fields, duplicates.update
                    ),
                ));
            };
            update_fields = names;
        }
        let (mut bulk_fields, mut bulk_update_fields) = (Vec::new(), Vec::new());
        if let Some(bulk) = &record.bulk_update {
            capability(bulk.capability)?;
            let list = readable(bulk.list)?;
            own(list, "bulk update list contract")?;
            if !paged(list) {
                return Err(invalid(
                    entry,
                    format!(
                        "names list contract {}, which does not take integer page and page_size",
                        bulk.list
                    ),
                ));
            }
            let get = readable(bulk.get)?;
            own(get, "bulk update read")?;
            if !properties(&get.input).is_some_and(|p| p.contains_key(bulk.get_id)) {
                return Err(invalid(
                    entry,
                    format!(
                        "names read {} by {}, which it does not take",
                        bulk.get, bulk.get_id
                    ),
                ));
            }
            let update = writable(bulk.update)?;
            own(update, "bulk update contract")?;
            let Some((holder, names)) = update_holder(
                update,
                bulk.update_id,
                bulk.update_version,
                bulk.update_fields,
            ) else {
                return Err(invalid(
                    entry,
                    format!(
                        "names an id, version or fields property ({:?}) {} does not take",
                        bulk.update_fields, bulk.update
                    ),
                ));
            };
            let fields_schema = if bulk.update_fields.is_empty() {
                Some(&update.input)
            } else {
                properties(&update.input).and_then(|p| p.get(bulk.update_fields))
            };
            let mandatory = fields_schema.map(required).unwrap_or_default();
            for name in bulk.fields {
                let Some(found) = holder
                    .get(*name)
                    .and_then(|schema| field(name, schema, mandatory.contains(name)))
                else {
                    return Err(invalid(
                        entry,
                        format!(
                            "updates {name} in bulk, which is not a scalar field of {}",
                            bulk.update
                        ),
                    ));
                };
                bulk_fields.push(found);
            }
            if bulk_fields.is_empty() {
                return Err(invalid(entry, "offers a bulk update with no fields"));
            }
            bulk_update_fields = names;
        }
        for reference in record.import.references {
            let Some(field) = fields
                .iter_mut()
                .find(|field| field.name == reference.field)
            else {
                return Err(invalid(
                    entry,
                    format!(
                        "refers through {}, which the create contract does not take",
                        reference.field
                    ),
                ));
            };
            if !matches!(field.kind, Kind::Integer | Kind::Text) {
                return Err(invalid(
                    entry,
                    format!("refers through {}, which is not an id", reference.field),
                ));
            }
            field.reference = Some(reference);
            if let Some(lookup) = &reference.lookup {
                let read = readable(lookup.read)?;
                take_limit(read);
                if !properties(&read.input).is_some_and(|p| p.contains_key(lookup.by)) {
                    return Err(invalid(
                        entry,
                        format!(
                            "looks {} up by {}, which {} does not take",
                            reference.field, lookup.by, lookup.read
                        ),
                    ));
                }
            }
        }
        types.insert(
            entry.id,
            Registered {
                key: entry.id,
                definition: record,
                fields,
                update_fields,
                bulk_fields,
                bulk_update_fields,
                limited,
            },
        );
    }
    for registered in types.values() {
        for reference in registered.definition.import.references {
            if let Some(parent) = reference.parent {
                if !types.contains_key(parent) {
                    return Err(RegistryError::Contribution(format!(
                        "import record type {} names parent {parent}, which is not an import record type",
                        registered.key
                    )));
                }
            }
        }
    }
    Ok(types)
}
