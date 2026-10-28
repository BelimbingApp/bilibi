//! The `company` record kind (`base/records`): another module may point at a
//! company by `("company", id)` and name, link and check it through Base
//! Records. The read rule is the one the company page and its files follow:
//! `admin.company.view` and a live company of the actor's own tenant, so a
//! soft-deleted company is `NotFound`.
use crate::{contributions::VIEW, files};
use bilibi_database::Error;
use bilibi_records::{Access, RecordKind, RecordOwner};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};

pub struct CompanyRecords;
pub static RECORDS: CompanyRecords = CompanyRecords;

impl RecordOwner for CompanyRecords {
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: &'a str,
    ) -> ProofFuture<'a, Access> {
        Box::pin(async move {
            let Ok(id) = files::company_id(id) else {
                return Ok(Access::NotFound);
            };
            match files::authorize(client, scope, id, VIEW, false).await {
                Ok(()) => Ok(Access::Readable),
                Err(Error::NotFound) => Ok(Access::NotFound),
                Err(Error::Forbidden) => Ok(Access::Forbidden),
                Err(error) => Err(error),
            }
        })
    }
}

bilibi_records::declare!(
    "core/company",
    RecordKind::new("company", "companies", &RECORDS).page("/companies/{id}")
);
