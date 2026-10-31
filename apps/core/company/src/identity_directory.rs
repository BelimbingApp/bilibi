//! Company identity half of the shared party directory (Bilimbi CompanyDirectory).
use bilibi_database::{Error, Result};
use bilibi_party_ids::{OrganisationId, PartyId};
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
        party: PartyId::Organisation(
            OrganisationId::new(value["id"].as_i64().ok_or(Error::Internal)?)
                .map_err(|_| Error::Internal)?,
        ),
        code: text("code")?,
        name: text("name")?,
        status: text("status")?,
    })
}
const FACTS:&str="json_build_object('id',id,'code',code,'name',COALESCE(NULLIF(btrim(legal_name),''),name),'status',status)";
impl PartyIdentityProvider for Provider {
    fn by_id<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: i64,
    ) -> ProofFuture<'a, Option<PartyIdentity>> {
        Box::pin(async move {
            client.query_opt(&format!("SELECT {FACTS} FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL"),&[&scope.tenant(),&id]).await?.map(|row|identity(&row.get::<_,Value>(0))).transpose()
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
            let row=client.query_opt(&format!("SELECT COALESCE(json_agg({FACTS} ORDER BY code,id),'[]'::json) FROM companies WHERE tenant_id=$1 AND code=ANY($2::text[]) AND deleted_at IS NULL"),&[&scope.tenant(),&codes]).await?.ok_or(Error::Internal)?;
            let Value::Array(values) = row.get::<_, Value>(0) else {
                return Err(Error::Internal);
            };
            values.iter().map(identity).collect()
        })
    }
}
