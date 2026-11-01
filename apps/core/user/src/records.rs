//! Staff account references follow the account detail page's read rules.
use bilibi_authz::authorize_proof;
use bilibi_database::Error;
use bilibi_records::{Access, RecordKind, RecordOwner};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};

struct UserRecords;
static RECORDS: UserRecords = UserRecords;

impl RecordOwner for UserRecords {
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
                "SELECT id FROM users WHERE tenant_id=$1 AND id=$2 AND kind='staff' AND deleted_at IS NULL",
                &[&scope.tenant(), &id],
            ).await?.is_some() { Access::Readable } else { Access::NotFound })
        })
    }
}

bilibi_records::declare!(
    "core/user",
    RecordKind::new("user", "users", &RECORDS).page("/users/{id}")
);
