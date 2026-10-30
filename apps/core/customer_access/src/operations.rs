//! The customer access operations agents may call
//! (`core.customer_access.*`): thin adapters over the `CustomerAccess` facade
//! with the sealed scope the registry proved. Reads return the portal site and
//! the invitations; every write is a reviewed safe write whose preview shares
//! the facade's checks without writing and whose commit writes through the
//! plan's audited transaction. The staff page calls the same contracts. Same
//! shape as `core/currency`.
use crate::{
    facade::{Checked, CustomerAccess, FieldError, InvitationInput},
    MANAGE, VIEW,
};
use bilibi_agent_api::{
    json::{Data, Output, Page},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

pub const SITE_GET: &str = "core.customer_access.site.get";
pub const SITE_SET: &str = "core.customer_access.site.set";
pub const LIST: &str = "core.customer_access.invitation.list";
pub const INVITE: &str = "core.customer_access.invitation.create";
pub const REVOKE: &str = "core.customer_access.invitation.revoke";

static BOUND: OnceLock<Arc<CustomerAccess>> = OnceLock::new();
pub(crate) fn bind(access: Arc<CustomerAccess>) -> bilibi_module_registry::Result<()> {
    BOUND.set(access).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "duplicate customer access agent binding".into(),
        )
    })
}
fn access() -> bilibi_agent_api::Result<&'static CustomerAccess> {
    BOUND.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}
fn checked<T>(checked: Checked<T>) -> bilibi_agent_api::Result<T> {
    checked.map_err(|errors: Vec<FieldError>| {
        let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for error in errors {
            fields
                .entry(error.field.to_owned())
                .or_default()
                .push(error.message);
        }
        bilibi_agent_api::Error::InvalidInput(fields)
    })
}
fn found(error: Error) -> bilibi_agent_api::Error {
    match error {
        Error::NotFound => bilibi_agent_api::Error::NotFound,
        other => other.into(),
    }
}
fn text(input: &Value, field: &str) -> String {
    input[field].as_str().unwrap_or_default().to_owned()
}
fn invitation_input(input: &Value) -> bilibi_agent_api::Result<InvitationInput> {
    let person = input["person_id"]
        .as_i64()
        .and_then(|id| PersonId::new(id).ok())
        .ok_or(Error::Invalid)?;
    let affiliation = match input["affiliation_id"].as_i64() {
        None => None,
        Some(id) => Some(AffiliationId::new(id).map_err(|_| Error::Invalid)?),
    };
    Ok(InvitationInput {
        person,
        affiliation,
        name: text(input, "name"),
        email: text(input, "email"),
    })
}
fn id(input: &Value) -> bilibi_agent_api::Result<i64> {
    input["id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or(Error::Invalid.into())
}

enum Read {
    Site,
    List,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "core/customer_access"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let access = access()?;
            let client = access.db.get().await.map_err(Error::from)?;
            match self {
                Self::Site => {
                    let site = access.site(&**client, scope).await.map_err(found)?;
                    Ok(Output::Data(Data::Json(json!({
                        "site": site.map(|site| site.to_json()),
                    }))))
                }
                Self::List => {
                    let page = input["page"].as_i64().unwrap_or(1).max(1);
                    let size = input["page_size"].as_i64().unwrap_or(25).clamp(1, 100);
                    let (entries, total) = access
                        .invitations(&**client, scope, page, size)
                        .await
                        .map_err(found)?;
                    Ok(Output::Page(Page {
                        entries: Data::Json(json!(entries
                            .iter()
                            .map(|invitation| invitation.to_json())
                            .collect::<Vec<_>>())),
                        page: page as u64,
                        page_size: size as u64,
                        total: total as u64,
                    }))
                }
            }
        })
    }
}

enum Write {
    Site,
    Invite,
    Revoke,
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "core/customer_access"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let access = access()?;
            let client = access.db.get().await.map_err(Error::from)?;
            let client = &**client;
            let value = match self {
                Self::Site => {
                    let (before, after) = checked(
                        access
                            .preview_site(
                                client,
                                scope,
                                &text(input, "address"),
                                &text(input, "title"),
                            )
                            .await
                            .map_err(found)?,
                    )?;
                    json!({
                        "summary": format!("Set the customer portal to {} at /portal/sign-in/{}", after.title, after.address),
                        "before": before.map(|site| site.to_json()),
                        "after": after.to_json(),
                    })
                }
                Self::Invite => checked(
                    access
                        .preview_invitation(client, scope, &invitation_input(input)?)
                        .await
                        .map_err(found)?,
                )?,
                Self::Revoke => {
                    let invitation = checked(
                        access
                            .preview_revoke(client, scope, id(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({
                        "summary": format!("Revoke the invitation to {} <{}>; its link stops working", invitation.name, invitation.email),
                        "before": invitation.to_json(),
                    })
                }
            };
            Ok(value.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let access = access()?;
            let value = match self {
                Self::Site => checked(
                    access
                        .set_site_in(
                            tx,
                            scope,
                            &text(input, "address"),
                            &text(input, "title"),
                            input["version"].as_i64(),
                        )
                        .await
                        .map_err(found)?,
                )?
                .to_json(),
                Self::Invite => checked(
                    access
                        .invite_in(tx, scope, &invitation_input(input)?)
                        .await
                        .map_err(found)?,
                )?
                .to_json(),
                Self::Revoke => checked(
                    access
                        .revoke_in(tx, scope, id(input)?)
                        .await
                        .map_err(found)?,
                )?
                .to_json(),
            };
            Ok(Output::Data(Data::Json(value)))
        })
    }
}

fn empty_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{"page":{"type":"integer","minimum":1,"maximum":1000000},"page_size":{"type":"integer","minimum":1,"maximum":100}}})
}
fn site_schema() -> Value {
    json!({"type":"object","required":["address","title"],"properties":{
        "address":{"type":"string","maxLength":63,"description":"The portal address segment, such as acme: lowercase letters, digits and inner hyphens. Customers sign in at /portal/sign-in/{address}."},
        "title":{"type":"string","maxLength":120,"description":"The name customers see on the portal and in its emails."},
        "version":{"type":"integer","minimum":1,"description":"The version you read; a site changed since is refused."},
    }})
}
fn invite_schema() -> Value {
    json!({"type":"object","required":["person_id","name","email"],"properties":{
        "person_id":{"type":"integer","minimum":1,"description":"The Person the customer signs in as."},
        "affiliation_id":{"type":"integer","minimum":1,"description":"For a business customer, the Person's affiliation (and so the company) it signs in through. Leave it out for a private individual."},
        "name":{"type":"string","maxLength":255,"description":"The account name; the customer may change it when accepting."},
        "email":{"type":"string","maxLength":254,"description":"Where the invitation goes; the customer's sign-in address."},
    }})
}
fn revoke_schema() -> Value {
    json!({"type":"object","required":["id"],"properties":{"id":{"type":"integer","minimum":1}}})
}
fn site_output() -> Value {
    json!({"type":"object","description":"site: address, title, version and sign_in (the portal sign-in path), or null when no site is set."})
}
fn list_output() -> Value {
    json!({"type":"array","items":{"type":"object","description":"id, email, name, person_id, affiliation_id, company_id, status (pending, accepted, revoked or expired) and user_id once accepted."}})
}
fn record_output() -> Value {
    json!({"type":"object","description":"The saved record. An invitation's link is emailed, never returned."})
}

const MODULE: &str = "core/customer_access";
inventory::submit! { Contribution::new(DEFINITIONS, "core/customer_access.agent", Provenance { module: MODULE, source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: SITE_GET, title: "Read the customer portal site", summary: "The portal address and title of this tenant's customer portal, or null when none is set.", keywords: &["portal", "customer", "site", "address"], capability: VIEW, input: empty_schema, output: site_output, handler: Handler::Read(&Read::Site), channels: Channels::Every, step_up: &[] },
        Operation { key: SITE_SET, title: "Set the customer portal site", summary: "Set the address customers sign in at (/portal/sign-in/{address}) and the title they see. Changing the address breaks links already emailed.", keywords: &["portal", "customer", "site", "address", "title"], capability: MANAGE, input: site_schema, output: record_output, handler: Handler::SafeWrite(&Write::Site), channels: Channels::Every, step_up: &[] },
        Operation { key: LIST, title: "List customer invitations", summary: "This tenant's customer portal invitations, newest first, with their status.", keywords: &["portal", "customer", "invitation", "invite"], capability: VIEW, input: list_schema, output: list_output, handler: Handler::Read(&Read::List), channels: Channels::Every, step_up: &[] },
        Operation { key: INVITE, title: "Invite a customer to the portal", summary: "Email a Person a link to set a password and use the customer portal, as a private individual or through one of the Person's affiliations. Inviting the same address again replaces the earlier link.", keywords: &["portal", "customer", "invitation", "invite", "account"], capability: MANAGE, input: invite_schema, output: record_output, handler: Handler::SafeWrite(&Write::Invite), channels: Channels::Every, step_up: &[] },
        Operation { key: REVOKE, title: "Revoke a customer invitation", summary: "Stop a pending invitation's link from working. An account already made from an accepted invitation is not touched.", keywords: &["portal", "customer", "invitation", "revoke", "cancel"], capability: MANAGE, input: revoke_schema, output: record_output, handler: Handler::SafeWrite(&Write::Revoke), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "core.customer_access", title: "Customer portal access", summary: "How a tenant's customers get into its portal: the portal site, invitations and what a customer can reach.", keywords: &["portal", "customer", "invitation", "site"], capability: VIEW,
        body: "A tenant's customers sign in to its portal, never to the staff application. First set the portal site with core.customer_access.site.set: an address such as acme (customers sign in at /portal/sign-in/acme) and the title they see. Read it with core.customer_access.site.get.\n\nA customer account always belongs to a Person. Invite one with core.customer_access.invitation.create: the person_id, the name and email, and for a business customer the affiliation_id of the company it signs in through. The person gets an email with a one-time link to choose a password; the link expires after the tenant's invitation lifetime (7 days by default). Inviting the same address again replaces the earlier link. List invitations and their status with core.customer_access.invitation.list and stop a pending one with core.customer_access.invitation.revoke.\n\nA customer sees only portal pages and only its own records; it cannot open staff pages or connect an agent. Each write is reviewed: plan it, check the preview, then commit with the plan code." }],
}) }
