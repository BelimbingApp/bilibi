//! Names accounts for Base screens. Deleted accounts and other tenants resolve
//! to no name, so the asking screen keeps its row and shows the durable ID.
//! A customer account (ADR 15) keeps its name, so audit can label its writes,
//! but staff pickers never find one.
use bilibi_principal_directory::{
    DirectoryClient, DirectoryFuture, PrincipalKind, PrincipalProvider,
};
use bilibi_security::Scope;
use std::collections::HashMap;

pub struct UserDirectory;
impl PrincipalProvider for UserDirectory {
    fn kind(&self) -> PrincipalKind {
        PrincipalKind::User
    }
    fn names<'a>(
        &'a self,
        client: &'a dyn DirectoryClient,
        scope: &'a Scope,
        ids: &'a [i64],
    ) -> DirectoryFuture<'a, HashMap<i64, String>> {
        Box::pin(async move {
            let tenant = scope.tenant();
            let rows = client
                .query(
                    "SELECT id,name FROM users WHERE tenant_id=$1 AND id=ANY($2) AND deleted_at IS NULL",
                    &[&tenant, &ids],
                )
                .await?;
            Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
        })
    }
    /// Email is an account attribute Base never sees; position() avoids LIKE escaping.
    fn search<'a>(
        &'a self,
        client: &'a dyn DirectoryClient,
        scope: &'a Scope,
        ids: &'a [i64],
        needle: &'a str,
    ) -> DirectoryFuture<'a, Vec<i64>> {
        Box::pin(async move {
            let tenant = scope.tenant();
            let rows = client
                .query(
                    "SELECT id FROM users WHERE tenant_id=$1 AND id=ANY($2) AND deleted_at IS NULL AND kind='staff' AND position($3 in lower(email))>0",
                    &[&tenant, &ids, &needle],
                )
                .await?;
            Ok(rows.iter().map(|row| row.get(0)).collect())
        })
    }
}
