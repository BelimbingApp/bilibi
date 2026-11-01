//! Linked-record identity reads.
//! Core owners supply their own facts; Base never queries Core tables.
use crate::{PartyProofs, ProofClient, ProofFuture, Scope};
use bilibi_database::{Error, Result};
use bilibi_party_ids::PartyId;
use std::sync::Arc;

/// One code is unique within its Core kind and tenant. Codes are immutable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyIdentity {
    pub party: PartyId,
    pub code: String,
    pub name: String,
    pub status: String,
}
impl PartyIdentity {
    pub fn record_kind(&self) -> &'static str {
        match self.party {
            PartyId::Organisation(_) => "company",
            PartyId::Person(_) => "person",
        }
    }
    pub fn record_id(&self) -> i64 {
        match self.party {
            PartyId::Organisation(id) => id.get(),
            PartyId::Person(id) => id.get(),
        }
    }
}
/// Maximum input count, including duplicates, to bound query and result size.
pub const PARTY_CODE_LOOKUP_LIMIT: usize = 10_000;

/// Owner binding for identity facts only, with no contacts or legal identifiers.
/// Every query must use the sealed scope's tenant on the handed connection.
/// Archived/retired identities remain readable; removed identities do not.
pub trait PartyIdentityProvider: Send + Sync {
    fn by_id<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: i64,
    ) -> ProofFuture<'a, Option<PartyIdentity>>;
    fn by_codes<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        codes: &'a [String],
    ) -> ProofFuture<'a, Vec<PartyIdentity>>;
}
impl PartyProofs {
    pub fn bind_company_identities(
        &self,
        provider: Arc<dyn PartyIdentityProvider>,
    ) -> bilibi_module_registry::Result<()> {
        self.company_identities.set(provider).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate company identity provider".into(),
            )
        })
    }
    pub fn bind_person_identities(
        &self,
        provider: Arc<dyn PartyIdentityProvider>,
    ) -> bilibi_module_registry::Result<()> {
        self.person_identities.set(provider).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate person identity provider".into(),
            )
        })
    }
    /// Identity for an already authorized linked record. The caller authorizes
    /// its owning record first. No Core view grant is needed for code/name;
    /// this neither exposes contacts nor proves eligibility for a write.
    /// Missing owners, removed records and cross-tenant IDs return NotFound.
    pub async fn party_identity(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        party: PartyId,
    ) -> Result<PartyIdentity> {
        let (provider, id) = match party {
            PartyId::Organisation(id) => (self.company_identities.get(), id.get()),
            PartyId::Person(id) => (self.person_identities.get(), id.get()),
        };
        provider
            .ok_or(Error::NotFound)?
            .by_id(client, scope, id)
            .await?
            .ok_or(Error::NotFound)
    }
    /// Exact stored-code matches in the caller's tenant, for both Core kinds,
    /// in one bounded facade call (at most one query to each installed owner).
    /// A code may match both kinds: retain record_kind and record_id together.
    /// Unknown codes are omitted. The caller proves the linked records before
    /// acting on a result; matching a code grants no access or write authority.
    pub async fn parties_by_codes(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        codes: &[String],
    ) -> Result<Vec<PartyIdentity>> {
        if codes.len() > PARTY_CODE_LOOKUP_LIMIT
            || codes
                .iter()
                .any(|c| c.is_empty() || c.chars().count() > 255 || c.contains('\0'))
        {
            return Err(Error::Invalid);
        }
        let codes: Vec<_> = codes
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if codes.is_empty() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for provider in [&self.company_identities, &self.person_identities] {
            if let Some(provider) = provider.get() {
                result.extend(provider.by_codes(client, scope, &codes).await?);
            }
        }
        result.sort_by(|a, b| {
            (a.record_kind(), &a.code, a.record_id()).cmp(&(
                b.record_kind(),
                &b.code,
                b.record_id(),
            ))
        });
        Ok(result)
    }
}
