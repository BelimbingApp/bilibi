//! Starting values into tenants, by code: a missing code is inserted, an
//! existing one is left exactly as the operator keeps it (renamed, moved,
//! retired). The declared default becomes the default only when it is
//! inserted and the list has none. Each list is reconciled under its list
//! lock and within the 500-value cap: a run that would pass the cap is
//! refused whole. Tenant provisioning calls [`provision`] and records the
//! returned changes on its own transaction; the production seed runs
//! [`Seeds`] over every tenant. Nothing runs on boot.
use crate::{
    declaration::{PickList, Reach, LISTS, VALUES_MAX},
    values::{change, lock_list, PickValue, COLUMNS},
};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{seeds::SeedFuture, Change, Error, Result};
use bilibi_module_registry::{Contribution, RegistryError};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use tokio_postgres::GenericClient;

/// The linked declarations; an unmounted module declares none.
pub(crate) fn declared() -> Result<Vec<&'static PickList>> {
    inventory::iter::<Contribution>
        .into_iter()
        .filter(|entry| entry.consumer == LISTS)
        .map(|entry| Ok(entry.get::<PickList>()?))
        .collect()
}

/// Inserts each missing starting code of every list into `tenant` and
/// returns one `Change` per inserted value, for the caller to record.
async fn reconcile_tenant(
    client: &impl GenericClient,
    tenant: i64,
    lists: &[&'static PickList],
) -> Result<Vec<Change>> {
    let operator = client
        .query_opt(
            "SELECT 1 FROM platform_operator WHERE singleton AND tenant_id=$1",
            &[&tenant],
        )
        .await?
        .is_some();
    let mut changes = Vec::new();
    for list in lists {
        if list.reach == Reach::Installation && !operator {
            continue;
        }
        lock_list(client, tenant, list.key).await?;
        let present: BTreeSet<String> = client
            .query(
                "SELECT code FROM base_picklist_values WHERE tenant_id=$1 AND list=$2",
                &[&tenant, &list.key],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let missing: Vec<&str> = list
            .values
            .iter()
            .map(|value| value.code)
            .filter(|code| !present.contains(*code))
            .collect();
        if present.len() + missing.len() > VALUES_MAX {
            return Err(Error::Registry(RegistryError::Contribution(format!(
                "pick-list {} in tenant {tenant} holds {} values; adding the starting values {} would pass the {VALUES_MAX}-value limit. Remove custom values from that list (retired ones count), then run again.",
                list.key,
                present.len(),
                missing.join(", ")
            ))));
        }
        for (index, value) in list.values.iter().enumerate() {
            if !missing.contains(&value.code) {
                continue;
            }
            let position = (index as i32 + 1) * 10;
            let default = list.default == Some(value.code);
            let parents: Vec<&str> = value.parents.to_vec();
            let row = client
                .query_one(
                    &format!("INSERT INTO base_picklist_values(tenant_id,list,code,label,hint,position,is_default,parent_codes) \
                     SELECT $1,$2,$3,$4,$5,$6,$7 AND NOT EXISTS(SELECT 1 FROM base_picklist_values WHERE tenant_id=$1 AND list=$2 AND is_default),$8 \
                     RETURNING {COLUMNS}"),
                    &[&tenant, &list.key, &value.code, &value.label, &value.hint, &position, &default, &parents],
                )
                .await?;
            // An empty `old` records a create.
            changes.push(change(json!({}), &PickValue::from_row(&row, list)?));
        }
    }
    Ok(changes)
}

/// Gives a tenant the starting values of every linked list, on the
/// provisioning transaction, and returns one `Change` per inserted value: an
/// audited caller records each on its `MutationTransaction`. Installation-wide
/// lists go to the platform operator only, so call it after the operator is
/// marked.
pub async fn provision(client: &impl GenericClient, tenant: i64) -> Result<Vec<Change>> {
    reconcile_tenant(client, tenant, &declared()?).await
}

/// [`provision`] for a deployment command that runs without a
/// `MutationTransaction` (the `core/company` provisioning helpers): every
/// inserted value is listed in one retained system action, as the seed does.
pub async fn provision_deployment(client: &impl GenericClient, tenant: i64) -> Result<usize> {
    let changes = provision(client, tenant).await?;
    record(client, tenant, "provision", &changes).await?;
    Ok(changes.len())
}

/// One retained system action naming each inserted value.
async fn record(
    client: &impl GenericClient,
    tenant: i64,
    principal: &str,
    changes: &[Change],
) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let mut who = Attribution::system(Some(principal))?;
    who.tenant = Some(tenant);
    let values: Vec<Value> = changes
        .iter()
        .map(|change| json!({"id": change.id, "list": change.new["list"], "code": change.new["code"]}))
        .collect();
    record_action(
        client,
        &who,
        None,
        "picklists.starting_values.reconciled",
        json!({"summary":format!("Added {} starting pick-list value(s)", values.len()),"values":values}),
        true,
    )
    .await?;
    Ok(())
}

/// The service `base/picklists.seeds`, operation `reconcile`: every live
/// tenant. A module that adds lists or values after an installation seeded
/// contributes its own seed version naming this operation.
pub(crate) struct Seeds;
impl bilibi_database::seeds::SeedExecutor for Seeds {
    fn execute<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
        operation: &'a str,
    ) -> SeedFuture<'a> {
        Box::pin(async move {
            if operation != "reconcile" {
                return Err(Error::Invalid);
            }
            let lists = declared()?;
            for row in tx
                .query(
                    "SELECT id FROM tenants WHERE deleted_at IS NULL ORDER BY id",
                    &[],
                )
                .await?
            {
                let tenant = row.get(0);
                let changes = reconcile_tenant(tx, tenant, &lists).await?;
                record(tx, tenant, "picklists-reconcile", &changes).await?;
            }
            Ok(())
        })
    }
}
