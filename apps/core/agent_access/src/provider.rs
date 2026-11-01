//! Provider-token grants. They share the ordinary
//! connection lifetime, revocation, audit and step-up authority, not OAuth issuance.
use super::*;

pub(super) async fn prove_provider(
    users: &bilibi_user::UserService,
    client: &dyn ProofClient,
    scope: &Scope,
    connection: i64,
) -> Result<Option<OrganisationId>> {
    let row = client.query_opt("SELECT provider_public,provider_version,provider_link FROM core_agent_connections WHERE tenant_id=$1 AND user_id=$2 AND id=$3", &[&scope.tenant(),&scope.user(),&connection]).await?.ok_or(Error::Unauthorized)?;
    if let Some(public) = row.get::<_, Option<String>>(0) {
        return users
            .prove_provider_access(client, scope, &public, row.get(1), row.get(2))
            .await
            .map(Some);
    }
    Ok(None)
}

impl AgentAccess {
    /// Validate the provider signature afresh on every HTTP request. A public
    /// connection ID is required, so no untrusted issuer can direct discovery
    /// and no request scans other companies' trust configurations.
    pub async fn authenticate_provider(
        &self,
        public: &str,
        token: &str,
        resource: Option<&str>,
    ) -> std::result::Result<AgentClaims, OAuthError> {
        let admitted = self
            .users
            .authenticate_provider_access(public, token, resource.is_some())
            .await
            .map_err(refused)?;
        let scope = &admitted.scope;
        let (tenant, user) = (scope.tenant(), scope.user());
        let digest = secrets::digest(token);
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("agent_access.provider.admit"),
            )
            .await?;
        // Serialize first use of this credential, including concurrent API
        // calls, without ever storing or auditing the bearer itself.
        tx.sql()
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended(encode($1::bytea,'hex'),0))",
                &[&digest],
            )
            .await?;
        let company = self
            .users
            .prove_provider_access(tx.sql(), scope, public, admitted.version, admitted.link)
            .await
            .map_err(refused)?;
        if company != admitted.company || scope.current_company() != Some(company) {
            return Err(OAuthError::InvalidToken);
        }
        let prior = tx.sql().query_opt("SELECT id,provider_public,provider_version,provider_link,resource FROM core_agent_connections WHERE tenant_id=$1 AND user_id=$2 AND provider_token_hash=$3 FOR UPDATE", &[&tenant,&user,&digest]).await?;
        let id = if let Some(row) = prior {
            if row.get::<_, String>(1) != public
                || row.get::<_, i64>(2) != admitted.version
                || row.get::<_, i64>(3) != admitted.link
                || row.get::<_, Option<String>>(4).as_deref() != resource
            {
                return Err(OAuthError::InvalidToken);
            }
            row.get(0)
        } else {
            let organisation = scope.actor().organisation().map(OrganisationId::get);
            let person = scope.actor().person().map(PersonId::get);
            let public = public.to_owned();
            let resource = resource.map(str::to_owned);
            let (version, link, expires) = (admitted.version, admitted.link, admitted.expires_at);
            tx.mutate(move|sql|Box::pin(async move {
                let id:i64 = sql.query_opt("INSERT INTO core_agent_connections(tenant_id,user_id,organisation_id,person_id,agent_id,client_id,resource,expires_at,provider_public,provider_version,provider_link,provider_token_hash) VALUES($1,$2,$3,$4,'Company identity provider','company-provider',$5,$6,$7,$8,$9,$10) ON CONFLICT(provider_token_hash) DO NOTHING RETURNING id", &[&tenant,&user,&organisation,&person,&resource,&expires,&public,&version,&link,&digest]).await?.ok_or(Error::Unauthorized)?.get(0);
                Ok((id,vec![change("agent_connection",id,json!(null),json!({"provider":public,"expires_at":expires,"resource":resource}))]))
            })).await.map_err(refused)?
        };
        let claims = AgentClaims {
            connection: id,
            tenant,
            user,
            organisation: scope.actor().organisation(),
            person: scope.actor().person(),
            enabled_session: None,
        };
        self.prove(tx.sql(), &claims, true, "", None).await?;
        tx.commit().await?;
        Ok(claims)
    }
}
