//! Company status is a business event, never an editable attribute. All
//! transitions and owned writes serialize on the company row. Lock the tenant
//! before that row here, matching `primary::assign`, so a concurrent primary
//! transfer cannot race an archive/suspension or deadlock it. The tenant lock
//! is NO KEY UPDATE: child inserts may already hold the company lock and need
//! KEY SHARE on their tenant foreign key; those locks must remain compatible.
use crate::{organisation, Company, CompanyService, FieldError, WriteError, UPDATE};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;

pub const REASON_MAX_LENGTH: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Activate,
    Reactivate,
    Suspend,
    Archive,
}
impl Operation {
    pub fn permits(self, status: &str) -> bool {
        match self {
            Self::Activate => status == "pending",
            Self::Reactivate => status == "suspended",
            Self::Suspend => status == "active",
            Self::Archive => matches!(status, "pending" | "active" | "suspended"),
        }
    }
    pub fn destination(self) -> &'static str {
        match self {
            Self::Activate | Self::Reactivate => "active",
            Self::Suspend => "suspended",
            Self::Archive => "archived",
        }
    }
    pub fn event(self) -> &'static str {
        match self {
            Self::Activate => "company.activated",
            Self::Reactivate => "company.reactivated",
            Self::Suspend => "company.suspended",
            Self::Archive => "company.archived",
        }
    }
    fn past_tense(self) -> &'static str {
        match self {
            Self::Activate => "Activated",
            Self::Reactivate => "Reactivated",
            Self::Suspend => "Suspended",
            Self::Archive => "Archived",
        }
    }
}
/// Presentation order is part of the source contract. Archived offers none.
pub fn operations_from(status: &str) -> Vec<Operation> {
    [
        Operation::Activate,
        Operation::Reactivate,
        Operation::Suspend,
        Operation::Archive,
    ]
    .into_iter()
    .filter(|operation| operation.permits(status))
    .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("Cannot change a company from {0} with this operation")]
    InvalidTransition(String),
    #[error("The tenant's primary company cannot be archived or suspended")]
    PrimaryCompany,
    #[error("Your own company cannot be archived or suspended")]
    OwnCompany,
    #[error(transparent)]
    Write(#[from] WriteError),
}
impl From<Error> for LifecycleError {
    fn from(error: Error) -> Self {
        Self::Write(WriteError::Access(error))
    }
}

/// Applies one transition on the caller's audited transaction. Reason is
/// optional text; serde adapters must reject non-text values before calling.
/// The optional version fences a dialog opened before another status change.
/// Both the captured mutation and retained semantic action participate in
/// `mutate`, so catching an audit error can never permit a partial commit.
pub async fn apply(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    operation: Operation,
    reason: Option<&str>,
    expected_version: Option<i64>,
) -> Result<Company, LifecycleError> {
    organisation::authorize_target(tx.sql(), scope, id, UPDATE).await?;
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
    if reason.is_some_and(|reason| reason.chars().count() > REASON_MAX_LENGTH) {
        return Err(WriteError::Invalid(vec![FieldError::new(
            "reason",
            "should be at most 500 character(s)",
        )])
        .into());
    }
    tx.sql()
        .query_one(
            "SELECT id FROM tenants WHERE id=$1 FOR NO KEY UPDATE",
            &[&scope.tenant()],
        )
        .await
        .map_err(Error::from)?;
    let row = tx.sql().query_opt(
        &format!("SELECT {} FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE", organisation::COLUMNS),
        &[&scope.tenant(), &id],
    ).await.map_err(Error::from)?.ok_or(WriteError::NotFound)?;
    let before = Company::from_row(&row, &HashSet::new());
    if expected_version.is_some_and(|version| version != before.version) {
        return Err(WriteError::Stale.into());
    }
    if !operation.permits(&before.status) {
        return Err(LifecycleError::InvalidTransition(before.status));
    }
    if matches!(operation, Operation::Archive | Operation::Suspend) {
        if crate::primary::is_primary(tx.sql(), scope, id).await? {
            return Err(LifecycleError::PrimaryCompany);
        }
        if scope
            .current_company()
            .is_some_and(|company| company.get() == id)
        {
            return Err(LifecycleError::OwnCompany);
        }
    }
    let masked = bilibi_authz::restrictions(tx.sql(), scope).await?;
    let tenant = scope.tenant();
    let reason = reason.map(str::to_owned);
    let who = Attribution::from_context(tx.context())?;
    let updated = tx.mutate(move |sql| Box::pin(async move {
        let row = sql.query_one(
            &format!("UPDATE companies SET status=$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {}", organisation::COLUMNS),
            &[&tenant, &id, &operation.destination()],
        ).await?;
        let updated = Company::from_row(&row, &masked);
        record_action(sql, &who, None, operation.event(), json!({
            "semantic":true, "source":"Company",
            "summary":format!("{} company “{}”", operation.past_tense(), before.name),
            "subject":{"name":"company","id":id,"label":before.name},
            "context":{"from_status":before.status,"to_status":updated.status,"reason":reason},
            "result":"succeeded"
        }), true).await?;
        let change = Change {
            kind:"company".into(), id,
            old:json!({"status":before.status}), new:json!({"status":updated.status}),
        };
        Ok((updated, vec![change]))
    })).await?;
    Ok(updated)
}

impl CompanyService {
    pub async fn transition_company(
        &self,
        scope: &Scope,
        id: i64,
        operation: Operation,
        reason: Option<&str>,
        expected_version: Option<i64>,
    ) -> Result<Company, LifecycleError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("company.lifecycle"))
            .await?;
        let updated = apply(&mut tx, scope, id, operation, reason, expected_version).await?;
        tx.commit().await?;
        Ok(updated)
    }
    pub async fn activate_company(
        &self,
        scope: &Scope,
        id: i64,
        reason: Option<&str>,
    ) -> Result<Company, LifecycleError> {
        self.transition_company(scope, id, Operation::Activate, reason, None)
            .await
    }
    pub async fn reactivate_company(
        &self,
        scope: &Scope,
        id: i64,
        reason: Option<&str>,
    ) -> Result<Company, LifecycleError> {
        self.transition_company(scope, id, Operation::Reactivate, reason, None)
            .await
    }
    pub async fn suspend_company(
        &self,
        scope: &Scope,
        id: i64,
        reason: Option<&str>,
    ) -> Result<Company, LifecycleError> {
        self.transition_company(scope, id, Operation::Suspend, reason, None)
            .await
    }
    pub async fn archive_company(
        &self,
        scope: &Scope,
        id: i64,
        reason: Option<&str>,
    ) -> Result<Company, LifecycleError> {
        self.transition_company(scope, id, Operation::Archive, reason, None)
            .await
    }
}
