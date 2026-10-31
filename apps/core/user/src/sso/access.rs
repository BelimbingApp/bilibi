//! Resource-server admission; no Bilimbi counterpart. User owns the provider
//! trust and exact account link; Agent Access owns the durable machine grant.
use super::*;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{AccountKind, ProofClient};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Verified output for Agent Access, never deserialized from a request.
pub struct ProviderAccess {
    pub scope: Scope,
    pub company: OrganisationId,
    pub version: i64,
    pub link: i64,
    pub expires_at: SystemTime,
}

impl UserService {
    /// The public connection selects a trust relationship and its exact
    /// workplace, not an actor. Only the signed issuer/subject and existing
    /// identity link select the account.
    pub async fn authenticate_provider_access(
        &self,
        public: &str,
        token: &str,
        mcp: bool,
    ) -> Result<ProviderAccess> {
        let c = self.sso_connection(public, true).await?;
        let audience = if mcp {
            &c.config.mcp_audience
        } else {
            &c.config.api_audience
        };
        if audience.is_empty() || token.len() > 65536 {
            return Err(Error::Unauthorized);
        }
        let claims = self
            .identities
            .get(&c.config.method)?
            .access_token(&c.config.protocol, token, audience)
            .await?;
        let subject = claims["sub"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 255)
            .ok_or(Error::Unauthorized)?;
        let issuer = claims["iss"].as_str().ok_or(Error::Unauthorized)?;
        let scopes = claims["scope"].as_str().ok_or(Error::Unauthorized)?;
        if !c
            .config
            .access_token_scopes
            .iter()
            .all(|required| scopes.split_whitespace().any(|s| s == required))
        {
            return Err(Error::Unauthorized);
        }
        let expires_at = UNIX_EPOCH
            .checked_add(Duration::from_secs(
                claims["exp"].as_u64().ok_or(Error::Unauthorized)?,
            ))
            .ok_or(Error::Unauthorized)?;
        if expires_at <= SystemTime::now() {
            return Err(Error::Unauthorized);
        }
        let mut client = self.db.get().await?;
        let tx = client.transaction().await?;
        let row = tx.query_opt("SELECT id,user_id FROM user_external_identities WHERE tenant_id=$1 AND company_id=$2 AND provider_id=$3 AND issuer=$4 AND subject=$5", &[&c.tenant,&c.company,&c.id,&issuer,&subject]).await?.ok_or(Error::Unauthorized)?;
        let scope = self
            .authentication
            .authenticate_session(
                &*tx,
                c.tenant,
                row.get(1),
                None,
                Some(c.company),
                String::new(),
            )
            .await?;
        if scope.current_company().map(OrganisationId::get) != Some(c.company) {
            return Err(Error::Unauthorized);
        }
        let link = row.get(0);
        let company = self
            .prove_provider_access(&*tx, &scope, public, c.version, link)
            .await?;
        if company.get() != c.company {
            return Err(Error::Unauthorized);
        }
        tx.commit().await?;
        Ok(ProviderAccess {
            scope,
            company,
            version: c.version,
            link,
            expires_at,
        })
    }

    /// Recheck the exact link and saved trust version on the caller's
    /// transaction. The share locks serialize a commit with disable/unlink.
    /// A configuration save intentionally invalidates earlier grants.
    pub async fn prove_provider_access(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        public: &str,
        version: i64,
        link: i64,
    ) -> Result<OrganisationId> {
        if scope.impersonator().is_some() || scope.actor().kind() != AccountKind::Staff {
            return Err(Error::Unauthorized);
        }
        let row = client.query_opt("SELECT p.company_id FROM user_identity_providers p JOIN user_external_identities i ON i.tenant_id=p.tenant_id AND i.provider_id=p.id AND i.company_id=p.company_id WHERE p.tenant_id=$1 AND p.public_id=$2 AND p.version=$3 AND p.enabled AND i.id=$4 AND i.user_id=$5 FOR SHARE OF p,i", &[&scope.tenant(),&public,&version,&link,&scope.user()]).await?.ok_or(Error::Unauthorized)?;
        let company = OrganisationId::new(row.get(0)).map_err(|_| Error::Unauthorized)?;
        if !bilibi_company::reachable(client, scope, company).await? {
            return Err(Error::Unauthorized);
        }
        Ok(company)
    }
}
