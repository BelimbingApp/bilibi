//! Starting values into tenants, by code: a missing code is inserted, an
//! existing one is left exactly as the operator keeps it (renamed, moved,
//! retired). The declared default becomes the default only when it is
//! inserted and the list has none. Tenant provisioning calls [`provision`];
//! the production seed runs [`Seeds`] over every tenant. Nothing runs on boot.
use crate::declaration::{PickList, Reach, LISTS};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{seeds::SeedFuture, Error, Result};
use bilibi_module_registry::Contribution;
use serde_json::json;
use tokio_postgres::GenericClient;

/// The linked declarations; an unmounted module declares none.
pub(crate) fn declared() -> Result<Vec<&'static PickList>> {
    inventory::iter::<Contribution>
        .into_iter()
        .filter(|entry| entry.consumer == LISTS)
        .map(|entry| Ok(entry.get::<PickList>()?))
        .collect()
}

async fn reconcile_tenant(
    client: &impl GenericClient,
    tenant: i64,
    lists: &[&'static PickList],
) -> Result<u64> {
    let operator = client
        .query_opt(
            "SELECT 1 FROM platform_operator WHERE singleton AND tenant_id=$1",
            &[&tenant],
        )
        .await?
        .is_some();
    let mut inserted = 0;
    for list in lists {
        if list.reach == Reach::Installation && !operator {
            continue;
        }
        for (index, value) in list.values.iter().enumerate() {
            let position = (index as i32 + 1) * 10;
            let default = list.default == Some(value.code);
            let parents: Vec<&str> = value.parents.to_vec();
            inserted += client
                .execute(
                    "INSERT INTO base_picklist_values(tenant_id,list,code,label,hint,position,is_default,parent_codes) \
                     SELECT $1,$2,$3,$4,$5,$6,$7 AND NOT EXISTS(SELECT 1 FROM base_picklist_values WHERE tenant_id=$1 AND list=$2 AND is_default),$8 \
                     ON CONFLICT ON CONSTRAINT base_picklist_values_code_key DO NOTHING",
                    &[&tenant, &list.key, &value.code, &value.label, &value.hint, &position, &default, &parents],
                )
                .await?;
        }
    }
    if inserted > 0 {
        let mut who = Attribution::system(Some("picklists-reconcile"))?;
        who.tenant = Some(tenant);
        record_action(
            client,
            &who,
            None,
            "picklists.starting_values.reconciled",
            json!({"summary":format!("Added {inserted} starting pick-list value(s)"),"inserted":inserted}),
            true,
        )
        .await?;
    }
    Ok(inserted)
}

/// Gives a tenant the starting values of every linked list, on the
/// provisioning transaction. Installation-wide lists go to the platform
/// operator only, so call it after the operator is marked.
pub async fn provision(client: &impl GenericClient, tenant: i64) -> Result<u64> {
    reconcile_tenant(client, tenant, &declared()?).await
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
                reconcile_tenant(tx, row.get(0), &lists).await?;
            }
            Ok(())
        })
    }
}
