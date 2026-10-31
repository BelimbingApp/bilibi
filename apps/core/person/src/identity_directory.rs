//! Person identity half of the shared party directory, without personal contacts.
//! Extends Bilimbi's CompanyDirectory naming seam to Core Persons.
use bilibi_database::{Error, Result};
use bilibi_party_ids::{PartyId, PersonId};
use bilibi_tenancy::{PartyIdentity, PartyIdentityProvider, ProofClient, ProofFuture, Scope};
use serde_json::Value;
pub(crate) struct Provider;
fn identity(value: &Value) -> Result<PartyIdentity> {
    let text = |key| {
        value[key]
            .as_str()
            .map(str::to_owned)
            .ok_or(Error::Internal)
    };
    Ok(PartyIdentity {
        party: PartyId::Person(
            PersonId::new(value["id"].as_i64().ok_or(Error::Internal)?)
                .map_err(|_| Error::Internal)?,
        ),
        code: text("code")?,
        name: text("name")?,
        status: text("status")?,
    })
}
const FACTS:&str="json_build_object('id',id,'code',code,'name',name,'status',CASE WHEN retired_at IS NULL THEN 'active' ELSE 'retired' END)";
impl PartyIdentityProvider for Provider {
    fn by_id<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: i64,
    ) -> ProofFuture<'a, Option<PartyIdentity>> {
        Box::pin(async move {
            client
                .query_opt(
                    &format!("SELECT {FACTS} FROM persons WHERE tenant_id=$1 AND id=$2"),
                    &[&scope.tenant(), &id],
                )
                .await?
                .map(|row| identity(&row.get::<_, Value>(0)))
                .transpose()
        })
    }
    fn by_codes<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        codes: &'a [String],
    ) -> ProofFuture<'a, Vec<PartyIdentity>> {
        Box::pin(async move {
            let codes = codes.to_vec();
            let row=client.query_opt(&format!("SELECT COALESCE(json_agg({FACTS} ORDER BY code,id),'[]'::json) FROM persons WHERE tenant_id=$1 AND code=ANY($2::text[])"),&[&scope.tenant(),&codes]).await?.ok_or(Error::Internal)?;
            let Value::Array(values) = row.get::<_, Value>(0) else {
                return Err(Error::Internal);
            };
            values.iter().map(identity).collect()
        })
    }
}
