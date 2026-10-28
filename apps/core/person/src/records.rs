//! The `person` record kind (`base/records`): another module may point at a
//! person by `("person", id)` and name, link and check it through Base
//! Records. The read rule is the one the contact page and its files follow:
//! `admin.person.view` and a person of the actor's own tenant. A retired
//! person stays readable, as the Contacts list still shows them.
use crate::{contributions::VIEW, files};
use bilibi_database::Error;
use bilibi_records::{Access, RecordKind, RecordOwner};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};

pub struct PersonRecords;
pub static RECORDS: PersonRecords = PersonRecords;

impl RecordOwner for PersonRecords {
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: &'a str,
    ) -> ProofFuture<'a, Access> {
        Box::pin(async move {
            let Ok(id) = files::person_id(id) else {
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
    "core/person",
    RecordKind::new("person", "persons", &RECORDS).page("/contacts/{id}")
);
