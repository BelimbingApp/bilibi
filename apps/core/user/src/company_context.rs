//! The company a browser session works in (`Scope::current_company`): the shell's
//! company switcher posts a choice here, and this write is the one place a
//! session's `company_id` is set. The choice is a saved account preference
//! for an ordinary session and a view-only session fact while impersonating,
//! so an operator viewing as a user may make it too; the reach it is checked
//! against is the viewed account's.
//!
//! A choice is accepted only for a company where the account has a desk now (Core
//! Company's `reachable`: live, unarchived, with an explicit desk), and Tenancy proves the same fact again on
//! every later authentication, so the stored choice never outlives its
//! reach. The switch commits with a `user.company.switched` action, so the
//! trail says which company was current from when; a refused switch leaves a
//! non-retained `user.company.switch_refused` action naming the requested
//! company. The reach check itself writes no authorization decision. An empty
//! choice keeps the scope's current valid desk.
use crate::UserService;
use bilibi_audit::{record_action, Attribution, RequestInfo};
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
use serde_json::json;

/// Why a switch did not happen.
#[derive(Debug, thiserror::Error)]
pub enum ChoiceRefusal {
    /// Not a live company of the tenant the account reaches; the shell never
    /// offers one, so this is a stale or forged form.
    #[error("That company is not available to you.")]
    Unavailable,
    #[error(transparent)]
    Other(#[from] Error),
}

/// Where a switch left the session: the company it works in from now on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Switched {
    /// The chosen company, or `None` when no desk is available.
    pub company: Option<i64>,
}

impl UserService {
    /// Makes `company` the session's current company (`None` keeps the
    /// current valid desk). The session is
    /// the one the credential names and the CSRF proof the form carried.
    pub async fn choose_company(
        &self,
        token: &str,
        csrf: &str,
        company: Option<i64>,
        request: &RequestInfo,
    ) -> std::result::Result<Switched, ChoiceRefusal> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        let company = company.or(scope.current_company().map(OrganisationId::get));
        if let Some(id) = company {
            let reached = match OrganisationId::new(id) {
                Ok(company) => {
                    bilibi_tenancy::reaches_work_company(&**client, &scope, company).await?
                }
                Err(_) => false,
            };
            if !reached {
                record_action(
                    &**client,
                    &Attribution::from_scope(&scope),
                    Some(request),
                    "user.company.switch_refused",
                    json!({
                        "semantic": true,
                        "source": "CompanyContext",
                        "summary": "Refused a switch to a company the account does not reach",
                        "surface": "shell.company",
                        "subject": {"name": "company", "id": id},
                        "context": {"requested": id},
                        "result": "refused",
                    }),
                    false,
                )
                .await?;
                return Err(ChoiceRefusal::Unavailable);
            }
        }
        let before = scope.current_company().map(OrganisationId::get);
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.company.switch"))
            .await?;
        if let Some(id) = company {
            match bilibi_company::require_writable(tx.sql(), &scope, id, true).await {
                Ok(_) => {}
                Err(bilibi_company::WriteError::Access(error)) => return Err(error.into()),
                Err(_) => return Err(ChoiceRefusal::Unavailable),
            }
            let company = OrganisationId::new(id).map_err(|_| Error::Invalid)?;
            if !bilibi_tenancy::reaches_work_company(tx.sql(), &scope, company).await? {
                return Err(ChoiceRefusal::Unavailable);
            }
        }
        let to = company;
        let tenant = scope.tenant();
        let user = scope.user();
        // Company -> User -> Session, as account mutations revoke sessions
        // after locking their user. A view's choice is never a saved preference.
        if scope.impersonator().is_none() {
            tx.mutate(move|sql|Box::pin(async move {
                let old:Option<i64>=sql.query_opt("SELECT last_desk_id FROM users WHERE tenant_id=$1 AND id=$2 AND kind='staff' FOR UPDATE",&[&tenant,&user]).await?.ok_or(Error::Unauthorized)?.get(0);
                sql.execute("UPDATE users SET last_desk_id=$3 WHERE tenant_id=$1 AND id=$2",&[&tenant,&user,&to]).await?;
                let changes=if old==to {Vec::new()}else{vec![bilibi_database::Change{kind:"user.desk_choice".into(),id:user,old:json!({"company_id":old}),new:json!({"company_id":to})}]};
                Ok(((),changes))
            })).await?;
        }
        match self
            .sessions
            .choose_company(tx.sql(), &scope, company)
            .await
        {
            Ok(()) => {}
            Err(Error::Conflict) => return Err(Error::Unauthorized.into()),
            Err(error) => return Err(error.into()),
        }
        record_action(
            tx.sql(),
            &Attribution::from_scope(&scope),
            Some(request),
            "user.company.switched",
            json!({
                "semantic": true,
                "source": "CompanyContext",
                "summary": "Switched the current company",
                "surface": "shell.company",
                "subject": {"name": "company", "id": to, "label": to.map(|id| format!("Company#{id}"))},
                "context": {"from": before, "to": to},
                "result": "succeeded",
            }),
            false,
        )
        .await?;
        tx.commit().await?;
        Ok(Switched { company })
    }
}
