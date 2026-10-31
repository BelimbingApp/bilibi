//! Address references follow the detail page's tenant, removal and view
//! rules. Mirrors Bilimbi `Core.Address` reads; Base Records has no counterpart.
use bilibi_authz::authorize_proof;
use bilibi_database::Error;
use bilibi_records::{Access, RecordKind, RecordOwner};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};

struct AddressRecords;
static RECORDS: AddressRecords = AddressRecords;

impl RecordOwner for AddressRecords {
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: &'a str,
    ) -> ProofFuture<'a, Access> {
        Box::pin(async move {
            let Ok(id) = id.parse::<i64>() else {
                return Ok(Access::NotFound);
            };
            match authorize_proof(client, scope, crate::contributions::VIEW).await {
                Ok(()) => {}
                Err(Error::Forbidden) => return Ok(Access::Forbidden),
                Err(error) => return Err(error),
            }
            Ok(if client.query_opt(
                "SELECT id FROM addresses WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
                &[&scope.tenant(), &id],
            ).await?.is_some() { Access::Readable } else { Access::NotFound })
        })
    }
}

bilibi_records::declare!(
    "core/address",
    RecordKind::new("address", "addresses", &RECORDS).page("/addresses/{id}")
);
