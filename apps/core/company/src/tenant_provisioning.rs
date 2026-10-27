//! Company half of tenant provisioning (Bilimbi `PrimaryCompanyManager`).
//! Only a sealed receipt from Tenancy's fresh insert permits these writes;
//! no target tenant id or retargeted actor Scope is accepted.
use crate::{organisation, primary, Company, CompanyService, NewCompany, ValidCompany};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::{CreatedTenant, Scope};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use tokio_postgres::GenericClient;

pub type TenantCompanyErrors = BTreeMap<String, Vec<String>>;

impl CompanyService {
    /// The exact company defaults the review and commit share. Names and
    /// generated codes obey ordinary Company validation and field policy.
    pub async fn preview_tenant_companies(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        names: &[String],
    ) -> Result<std::result::Result<Vec<ValidCompany>, TenantCompanyErrors>> {
        bilibi_authz::authorize_operator(client, scope, bilibi_tenancy::CREATE).await?;
        let restricted = bilibi_authz::restrictions(client, scope).await?;
        let mut errors = BTreeMap::<String, Vec<String>>::new();
        if names.is_empty() || names.len() > 100 {
            errors.insert(
                "additional_companies".into(),
                vec!["provide a primary company and at most 99 additional companies".into()],
            );
            return Ok(Err(errors));
        }
        let mut codes = HashSet::new();
        let mut companies = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            let field = if index == 0 {
                "primary_company".into()
            } else {
                format!("additional_companies.{}", index - 1)
            };
            match organisation::validate(
                &NewCompany {
                    name: name.clone(),
                    ..NewCompany::default()
                },
                &restricted,
            ) {
                Ok(company) => {
                    if !codes.insert(company.code.clone()) {
                        errors
                            .entry(field)
                            .or_default()
                            .push("generates the same company code as another name".into());
                    }
                    companies.push(company);
                }
                Err(fields) => {
                    for error in fields {
                        errors
                            .entry(field.clone())
                            .or_default()
                            .push(format!("{}: {}", error.field, error.message));
                    }
                }
            }
        }
        Ok(if errors.is_empty() {
            Ok(companies)
        } else {
            Err(errors)
        })
    }

    /// Provisions companies only in the tenant this transaction just created.
    /// The first becomes primary; all rows and the assignment are audited
    /// under the original person's identity and agent connection. The tenant
    /// also receives every linked pick-list's starting values.
    pub async fn provision_new_tenant_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        receipt: &CreatedTenant,
        names: &[String],
    ) -> Result<Vec<Company>> {
        let tenant = receipt.verify(tx, scope).await?;
        let companies = self
            .preview_tenant_companies(tx.sql(), scope, names)
            .await?
            .map_err(|_| Error::Invalid)?;
        if primary::primary_company(tx.sql(), tenant).await?.is_some() {
            return Err(Error::Conflict);
        }
        // The starting pick-list values, attributed to this transaction's
        // person or agent like the companies below.
        for change in bilibi_picklists::provision(tx.sql(), tenant).await? {
            tx.record(change).await?;
        }
        let created = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut created = Vec::with_capacity(companies.len());
                    let mut changes = Vec::with_capacity(companies.len());
                    for company in companies {
                        let company = organisation::insert(sql, tenant, &company).await?;
                        changes.push(Change {
                            kind: "company".into(),
                            id: company.id,
                            old: Value::Null,
                            new: company.to_json(),
                        });
                        created.push(company);
                    }
                    Ok((created, changes))
                })
            })
            .await?;
        primary::assign(
            tx,
            tenant,
            created.first().ok_or(Error::Internal)?.id,
            false,
        )
        .await
        .map_err(|_| Error::Internal)?;
        Ok(created)
    }
}
