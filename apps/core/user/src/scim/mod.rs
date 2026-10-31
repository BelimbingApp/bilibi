//! Company SCIM service provider, extending Bilimbi User's account lifecycle.
//! Bilimbi has no SCIM protocol; the host adapter delegates all ownership here.
mod protocol;
use crate::{sso::config::Connection, UserService};
use bilibi_database::{Context, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use protocol::*;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use subtle::ConstantTimeEq;
use tokio_postgres::GenericClient;

pub(super) const MIGRATION: (&str, &str) = (
    "core-user-020-scim",
    include_str!("../../migrations/020-scim.sql"),
);
const SEALED: &str = "core/user.scim.token";
pub struct ScimRequest<'a> {
    pub connection: &'a str,
    pub bearer: &'a str,
    pub method: &'a str,
    pub resource: &'a str,
    pub id: Option<&'a str>,
    pub query: &'a HashMap<String, String>,
    pub body: Value,
}
pub struct ScimResponse {
    pub status: u16,
    pub body: Value,
    pub location: Option<String>,
}
/// Safe protocol refusal: never carries database messages, credentials or values.
pub struct ScimError {
    pub status: u16,
    pub kind: &'static str,
    pub detail: &'static str,
}
pub type ScimResult<T> = std::result::Result<T, ScimError>;
impl ScimError {
    fn invalid() -> Self {
        Self::bad("invalidValue", "The resource is invalid or incomplete.")
    }
    fn filter() -> Self {
        Self::bad(
            "invalidFilter",
            "Use supported equality filters joined with and.",
        )
    }
    fn missing() -> Self {
        Self {
            status: 404,
            kind: "",
            detail: "Resource not found.",
        }
    }
    fn unauthorized() -> Self {
        Self {
            status: 401,
            kind: "",
            detail: "A valid company provisioning token is required.",
        }
    }
    fn bad(kind: &'static str, detail: &'static str) -> Self {
        Self {
            status: 400,
            kind,
            detail,
        }
    }
    pub fn document(&self) -> Value {
        let mut v = json!({"schemas":["urn:ietf:params:scim:api:messages:2.0:Error"],"status":self.status.to_string(),"detail":self.detail});
        if !self.kind.is_empty() {
            v["scimType"] = json!(self.kind);
        }
        v
    }
}
impl From<Error> for ScimError {
    fn from(error: Error) -> Self {
        match error {
            Error::NotFound => Self::missing(),
            Error::Unauthorized => Self::unauthorized(),
            Error::Forbidden => Self {
                status: 403,
                kind: "",
                detail: "The company or account is unavailable.",
            },
            Error::Invalid => Self::invalid(),
            Error::Conflict => Self {
                status: 409,
                kind: "uniqueness",
                detail: "A resource with that identifier already exists.",
            },
            Error::Database(ref e)
                if std::error::Error::source(e)
                    .and_then(|s| s.downcast_ref::<tokio_postgres::error::DbError>())
                    .is_some_and(|e| {
                        e.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
                    }) =>
            {
                Self {
                    status: 409,
                    kind: "uniqueness",
                    detail: "A resource with that identifier already exists.",
                }
            }
            _ => Self {
                status: 503,
                kind: "",
                detail: "Provisioning is temporarily unavailable.",
            },
        }
    }
}
impl From<tokio_postgres::Error> for ScimError {
    fn from(e: tokio_postgres::Error) -> Self {
        Error::from(e).into()
    }
}
fn change(kind: &str, id: i64, old: Value, new: Value) -> bilibi_database::Change {
    crate::sso::change(kind, id, old, new)
}
fn source(id: i64) -> String {
    format!("core.user.scim.{id}")
}
fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn table(group: bool) -> &'static str {
    if group {
        "user_scim_groups"
    } else {
        "user_scim_users"
    }
}

impl UserService {
    pub(crate) async fn scim_token_present(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
    ) -> Result<bool> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        self.sso_company(&**client, &scope, company).await?;
        Ok(client.query_opt("SELECT sealed_scim_token IS NOT NULL FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&scope.tenant(),&company,&id]).await?.ok_or(Error::NotFound)?.get(0))
    }
    /// The only plaintext return is a new credential, delivered once to the
    /// creating administrator. No read method returns the stored credential.
    pub(crate) async fn rotate_scim_token(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        revoke: bool,
    ) -> Result<Option<String>> {
        self.step_up
            .require(scope, crate::sso::config::CONFIGURE, &company.to_string())
            .await?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context(if revoke {
                    "user.scim.token_revoked"
                } else {
                    "user.scim.token_rotated"
                }),
            )
            .await?;
        let scope = self.authentication.reprove(tx.sql(), scope).await?;
        self.sso_company(tx.sql(), &scope, company).await?;
        if !revoke {
            bilibi_authz::authorize_key(tx.sql(), &scope, crate::contributions::CREATE).await?;
        }
        bilibi_company::lock_account_company(
            tx.sql(),
            &scope,
            bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
        )
        .await
        .map_err(|_| Error::Forbidden)?;
        let row=tx.sql().query_opt("SELECT public_id,sealed_scim_token IS NOT NULL FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND id=$3 FOR UPDATE",&[&scope.tenant(),&company,&id]).await?.ok_or(Error::NotFound)?;
        if !revoke {
            let connection = self
                .sso_connection_in(tx.sql(), &row.get::<_, String>(0), false)
                .await?;
            let roles = connection
                .config
                .role_mapping
                .values()
                .flatten()
                .copied()
                .chain(connection.config.default_role)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            self.sso_grants
                .configure_identity_roles_in(
                    &mut tx,
                    &scope,
                    bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                    &source(id),
                    &roles,
                )
                .await?;
        }
        let fresh = (!revoke).then(token);
        let sealed = fresh
            .as_ref()
            .map(|v| {
                self.settings
                    .seal_owned(SEALED, scope.tenant(), id, &json!({"token":v}))
            })
            .transpose()?;
        let tenant = scope.tenant();
        let had: bool = row.get(1);
        tx.mutate(move|sql|Box::pin(async move{
            sql.execute("UPDATE user_identity_providers SET sealed_scim_token=$3 WHERE tenant_id=$1 AND id=$2",&[&tenant,&id,&sealed]).await?;
            Ok(((),vec![change("user.scim_configuration",id,json!({"active":had}),json!({"active":!revoke,"rotated":!revoke,"company":company}))]))
        })).await?;
        tx.commit().await?;
        Ok(fresh)
    }
    /// Authentication bootstrap, like SSO and sessions: the public connection
    /// locates a sealed company credential. All later reads bind that tenant.
    pub async fn scim_request(&self, request: ScimRequest<'_>) -> ScimResult<ScimResponse> {
        let (public, bearer) = (request.connection, request.bearer);
        if public.len() != 32 || bearer.len() != 64 {
            return Err(ScimError::unauthorized());
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let located=client.query_opt("SELECT p.tenant_id,p.id,p.sealed_scim_token FROM user_identity_providers p JOIN tenants t ON t.id=p.tenant_id AND t.status='active' AND t.deleted_at IS NULL WHERE p.public_id=$1 AND p.sealed_scim_token IS NOT NULL", &[&public]).await?.ok_or_else(ScimError::unauthorized)?;
        let (tenant, provider): (i64, i64) = (located.get(0), located.get(1));
        self.scim_verify_bearer(tenant, provider, located.get(2), bearer)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(tenant, &source(provider), "user.scim.request"),
            )
            .await?;
        // Company lock precedes connection, Person, Affiliation and account locks.
        let company:i64=tx.sql().query_one("SELECT company_id FROM user_identity_providers WHERE tenant_id=$1 AND public_id=$2",&[&tenant,&public]).await?.get(0);
        let company_id =
            bilibi_party_ids::OrganisationId::new(company).map_err(|_| ScimError::invalid())?;
        bilibi_company::lock_account_company_in(tx.sql(), tenant, company_id)
            .await
            .map_err(|_| ScimError::unauthorized())?;
        let row=tx.sql().query_opt("SELECT id,sealed_scim_token FROM user_identity_providers WHERE tenant_id=$1 AND public_id=$2 FOR UPDATE",&[&tenant,&public]).await?.ok_or_else(ScimError::unauthorized)?;
        let provider: i64 = row.get(0);
        self.scim_verify_bearer(tenant, provider, row.get(1), bearer)?;
        let c = self.sso_connection_in(tx.sql(), public, false).await?;
        let response = self.scim_dispatch(&mut tx, &c, &request).await?;
        tx.commit().await?;
        Ok(response)
    }
    fn scim_verify_bearer(
        &self,
        tenant: i64,
        provider: i64,
        sealed: Option<Value>,
        bearer: &str,
    ) -> ScimResult<()> {
        let sealed = sealed.ok_or_else(ScimError::unauthorized)?;
        let secret = self
            .settings
            .open_owned(SEALED, tenant, provider, &sealed)
            .map_err(|_| ScimError::unauthorized())?;
        let expected = secret["token"]
            .as_str()
            .ok_or_else(ScimError::unauthorized)?;
        if bool::from(expected.as_bytes().ct_eq(bearer.as_bytes())) {
            Ok(())
        } else {
            Err(ScimError::unauthorized())
        }
    }
    async fn scim_dispatch(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        request: &ScimRequest<'_>,
    ) -> ScimResult<ScimResponse> {
        let (method, resource, id, query, body) = (
            request.method,
            request.resource,
            request.id,
            request.query,
            &request.body,
        );
        if query.contains_key("sortBy") || query.contains_key("sortOrder") {
            return Err(ScimError::bad("invalidValue", "Sorting is not supported."));
        }
        if method == "GET" {
            if let Some(body) = discovery(resource, id)? {
                return Ok(ScimResponse {
                    status: 200,
                    body,
                    location: None,
                });
            }
        }
        let group = match resource {
            "Users" => false,
            "Groups" => true,
            _ => return Err(ScimError::missing()),
        };
        if method == "GET" {
            let body = if let Some(id) = id {
                project(self.scim_document(tx.sql(), c, group, id).await?, query)?
            } else {
                self.scim_list(tx.sql(), c, group, query).await?
            };
            return Ok(ScimResponse {
                status: 200,
                body,
                location: None,
            });
        }
        if method == "POST" && id.is_none() {
            let mut doc = normalize(body, group)?;
            doc["id"] = json!(uuid::Uuid::new_v4().to_string());
            if group {
                self.scim_save_group(tx, c, None, doc.clone()).await?;
            } else {
                self.scim_create_user(tx, c, doc.clone()).await?;
            }
            let id = doc["id"].as_str().ok_or_else(ScimError::invalid)?;
            let body = self.scim_document(tx.sql(), c, group, id).await?;
            let location = body["meta"]["location"].as_str().map(str::to_owned);
            return Ok(ScimResponse {
                status: 201,
                body: project(body, query)?,
                location,
            });
        }
        let id = id.ok_or_else(ScimError::missing)?;
        let old = self.scim_document(tx.sql(), c, group, id).await?;
        if method == "DELETE" {
            if group {
                self.scim_delete_group(tx, c, id).await?;
            } else {
                let mut doc = old.clone();
                doc["active"] = json!(false);
                self.scim_update_user(tx, c, &old, normalize(&doc, false)?)
                    .await?;
            }
            return Ok(ScimResponse {
                status: 204,
                body: Value::Null,
                location: None,
            });
        }
        let mut doc = match method {
            "PUT" => normalize(body, group)?,
            "PATCH" => patch(old.clone(), body, group)?,
            _ => {
                return Err(ScimError {
                    status: 405,
                    kind: "",
                    detail: "Method not supported.",
                })
            }
        };
        // PUT omits optional immutable attributes in some real clients. Keep
        // the established subject rather than treating omission as an unlink.
        if method == "PUT" && !group && doc.get("externalId").is_none() {
            if let Some(external) = old.get("externalId") {
                doc["externalId"] = external.clone();
            }
        }
        if method == "PUT" && !group && attr(body, "active").is_none() {
            doc["active"] = old["active"].clone();
        }
        if attr(body, "id").is_some_and(|v| v != &json!(id)) {
            return Err(ScimError::bad(
                "mutability",
                "The resource ID cannot change.",
            ));
        }
        doc["id"] = json!(id);
        if group {
            self.scim_save_group(tx, c, Some(old), doc).await?;
        } else {
            self.scim_update_user(tx, c, &old, doc).await?;
        }
        let body = project(self.scim_document(tx.sql(), c, group, id).await?, query)?;
        Ok(ScimResponse {
            status: 200,
            body,
            location: None,
        })
    }
    async fn scim_document(
        &self,
        sql: &impl GenericClient,
        c: &Connection,
        group: bool,
        id: &str,
    ) -> ScimResult<Value> {
        let mut doc:Value=sql.query_opt(&format!("SELECT document FROM {} WHERE tenant_id=$1 AND provider_id=$2 AND resource_id=$3",table(group)),&[&c.tenant,&c.id,&id]).await?.ok_or_else(ScimError::missing)?.get(0);
        let members = if group {
            sql.query("SELECT member_id FROM user_scim_members WHERE tenant_id=$1 AND provider_id=$2 AND group_id=$3 ORDER BY member_id",&[&c.tenant,&c.id,&id]).await?.iter().map(|r|json!({"value":r.get::<_,String>(0)})).collect::<Vec<_>>()
        } else {
            sql.query("SELECT g.resource_id,g.document->>'displayName' FROM user_scim_members m JOIN user_scim_groups g ON g.tenant_id=m.tenant_id AND g.provider_id=m.provider_id AND g.resource_id=m.group_id WHERE m.tenant_id=$1 AND m.provider_id=$2 AND m.member_id=$3 ORDER BY g.resource_id",&[&c.tenant,&c.id,&id]).await?.iter().map(|r|json!({"value":r.get::<_,String>(0),"display":r.get::<_,String>(1),"type":"direct"})).collect::<Vec<_>>()
        };
        doc[if group { "members" } else { "groups" }] = json!(members);
        doc["meta"] = json!({"resourceType":if group{"Group"}else{"User"},"location":format!("{}/scim/v2/{}/{}/{}",self.origin,c.public_id,if group{"Groups"}else{"Users"},id)});
        Ok(doc)
    }
    async fn scim_list(
        &self,
        sql: &impl GenericClient,
        c: &Connection,
        group: bool,
        query: &HashMap<String, String>,
    ) -> ScimResult<Value> {
        let number = |key: &str, default: i64| -> ScimResult<i64> {
            query
                .get(key)
                .map(|s| s.parse::<i64>().map_err(|_| ScimError::invalid()))
                .unwrap_or(Ok(default))
        };
        let start = number("startIndex", 1)?.max(1);
        let count = number("count", 100)?;
        if count < 0 || start > i64::from(i32::MAX) {
            return Err(ScimError::invalid());
        }
        let count = count.min(200);
        let conditions = filter(query.get("filter").map(String::as_str), group)?;
        let mut predicate = String::from("tenant_id=$1 AND provider_id=$2");
        for (i, (field, _)) in conditions.iter().enumerate() {
            if *field == "document->>'externalId'" || *field == "resource_id" {
                predicate.push_str(&format!(" AND {field}=${}", i + 3));
            } else {
                predicate.push_str(&format!(" AND lower({field})=lower(${})", i + 3));
            }
        }
        let mut args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&c.tenant, &c.id];
        for (_, v) in &conditions {
            args.push(v);
        }
        let total: i64 = sql
            .query_one(
                &format!("SELECT count(*) FROM {} WHERE {predicate}", table(group)),
                &args,
            )
            .await?
            .get(0);
        let offset = start - 1;
        let n = args.len();
        args.push(&count);
        args.push(&offset);
        let ids = sql
            .query(
                &format!(
                    "SELECT resource_id FROM {} WHERE {predicate} ORDER BY id LIMIT ${} OFFSET ${}",
                    table(group),
                    n + 1,
                    n + 2
                ),
                &args,
            )
            .await?;
        let mut resources = Vec::new();
        for row in ids {
            resources.push(project(
                self.scim_document(sql, c, group, &row.get::<_, String>(0))
                    .await?,
                query,
            )?);
        }
        Ok(
            json!({"schemas":[LIST],"totalResults":total,"startIndex":start,"itemsPerPage":resources.len(),"Resources":resources}),
        )
    }
    async fn scim_create_user(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        doc: Value,
    ) -> ScimResult<()> {
        let username = doc["userName"]
            .as_str()
            .ok_or_else(ScimError::invalid)?
            .to_owned();
        if crate::accounts::email_taken(tx.sql(), &username, None).await? {
            return Err(Error::Conflict.into());
        }
        let name = doc["displayName"]
            .as_str()
            .ok_or_else(ScimError::invalid)?
            .to_owned();
        let email = mailbox(&doc);
        let person = self
            .persons
            .provision_in(
                tx,
                &bilibi_person::PersonInput {
                    name: name.clone(),
                    kind: bilibi_person::PersonKind::Human,
                    email: Some(email.clone()),
                    phone: None,
                },
            )
            .await
            .map_err(|_| ScimError::invalid())?;
        let affiliation = self
            .affiliations
            .provision_in(
                tx,
                &bilibi_affiliation::AffiliationInput {
                    person_id: person.id,
                    organisation_id: bilibi_party_ids::OrganisationId::new(c.company)
                        .map_err(|_| ScimError::invalid())?,
                    title: None,
                    work_email: Some(email),
                    work_phone: None,
                    dates: bilibi_affiliation::EffectiveDates::new(None, None)
                        .map_err(|_| ScimError::invalid())?,
                },
            )
            .await?;
        let password = crate::password::hash(&token()).await?;
        let (tenant, provider, company, person, affiliation) = (
            c.tenant,
            c.id,
            c.company,
            person.id.get(),
            affiliation.id.get(),
        );
        let id = doc["id"]
            .as_str()
            .ok_or_else(ScimError::invalid)?
            .to_owned();
        let saved = doc.clone();
        let user=tx.mutate(move|sql|Box::pin(async move{
            let user:i64=sql.query_one("INSERT INTO users(tenant_id,name,email,password_hash,person_id) VALUES($1,$2,$3,$4,$5) RETURNING id",&[&tenant,&name,&username,&password,&person]).await?.get(0);
            let row:i64=sql.query_one("INSERT INTO user_scim_users(tenant_id,provider_id,resource_id,user_id,document,person_id,affiliation_id) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",&[&tenant,&provider,&id,&user,&saved,&person,&affiliation]).await?.get(0);
            Ok((user,vec![change("user",user,Value::Null,json!({"name":name,"email":username,"person_id":person,"affiliation_id":affiliation,"organisation_id":company,"provisioned_by":"scim"})),change("user.scim_user",row,Value::Null,saved)]))
        })).await?;
        crate::desks::provision_in(tx, company, person).await?;
        // The provisioning administrator explicitly controls the immutable subject.
        // No email lookup ever links an existing account.
        if let (Some(subject), Some(issuer)) = (
            doc["externalId"].as_str(),
            c.config.protocol["issuer"]
                .as_str()
                .filter(|s| !s.is_empty()),
        ) {
            self.insert_external_link(
                tx,
                c,
                user,
                &crate::ExternalIdentity {
                    issuer: issuer.into(),
                    subject: subject.into(),
                    claims: json!({}),
                    session_id: None,
                    logout_hint: None,
                },
            )
            .await?;
        }
        if doc["active"] == false {
            let mut initial = doc.clone();
            initial["active"] = json!(true);
            self.scim_update_user(tx, c, &initial, doc).await?;
        } else {
            self.scim_apply_roles(tx, c, user).await?;
        }
        Ok(())
    }
    async fn scim_update_user(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        old: &Value,
        doc: Value,
    ) -> ScimResult<()> {
        if old.get("externalId") != doc.get("externalId") {
            return Err(ScimError::bad(
                "mutability",
                "externalId cannot change; use an explicit administrator identity link.",
            ));
        }
        let id = doc["id"].as_str().ok_or_else(ScimError::invalid)?;
        let row=tx.sql().query_opt("SELECT s.id,s.user_id,u.person_id,s.affiliation_id FROM user_scim_users s JOIN users u ON u.tenant_id=s.tenant_id AND u.id=s.user_id AND u.person_id=s.person_id AND u.deleted_at IS NULL WHERE s.tenant_id=$1 AND s.provider_id=$2 AND s.resource_id=$3 FOR UPDATE OF s",&[&c.tenant,&c.id,&id]).await?.ok_or_else(ScimError::missing)?;
        let (record, user, person, affiliation): (i64, i64, i64, i64) =
            (row.get(0), row.get(1), row.get(2), row.get(3));
        let username = doc["userName"]
            .as_str()
            .ok_or_else(ScimError::invalid)?
            .to_owned();
        if crate::accounts::email_taken(tx.sql(), &username, Some(user)).await? {
            return Err(Error::Conflict.into());
        }
        let name = doc["displayName"]
            .as_str()
            .ok_or_else(ScimError::invalid)?
            .to_owned();
        let email = mailbox(&doc);
        if old["displayName"] != doc["displayName"] || mailbox(old) != email {
            self.persons
                .update_provisioned_contact_in(
                    tx,
                    bilibi_party_ids::PersonId::new(person).map_err(|_| ScimError::invalid())?,
                    &name,
                    &email,
                )
                .await?;
            self.affiliations
                .update_provisioned_email_in(
                    tx,
                    bilibi_party_ids::AffiliationId::new(affiliation)
                        .map_err(|_| ScimError::invalid())?,
                    bilibi_party_ids::PersonId::new(person).map_err(|_| ScimError::invalid())?,
                    bilibi_party_ids::OrganisationId::new(c.company)
                        .map_err(|_| ScimError::invalid())?,
                    &email,
                )
                .await?;
        }
        let active = doc["active"].as_bool().ok_or_else(ScimError::invalid)?;
        if !active {
            self.sso_grants
                .revoke_identity_source_roles_in(tx, &source(c.id), user)
                .await?;
        }
        let tenant = c.tenant;
        let old_doc = old.clone();
        let saved = doc.clone();
        tx.mutate(move|sql|Box::pin(async move{
            let previous=sql.query_one("SELECT name,email FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&tenant,&user]).await?;
            sql.execute("UPDATE users SET name=$3,email=$4,email_verified_at=CASE WHEN email=$4 THEN email_verified_at ELSE NULL END,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",&[&tenant,&user,&name,&username]).await?;
            sql.execute("UPDATE user_scim_users SET document=$3,active=$4 WHERE tenant_id=$1 AND id=$2",&[&tenant,&record,&saved,&active]).await?;
            Ok(((),vec![change("user",user,json!({"name":previous.get::<_,String>(0),"email":previous.get::<_,String>(1)}),json!({"name":name,"email":username})),change("user.scim_user",record,old_doc,saved)]))
        })).await?;
        if active {
            let has_desk = tx.sql().query_opt("SELECT 1 FROM user_desks WHERE tenant_id=$1 AND person_id=$2 AND company_id=$3", &[&c.tenant,&person,&c.company]).await?.is_some();
            if has_desk {
                self.scim_apply_roles(tx, c, user).await?;
            } else {
                self.sso_grants
                    .revoke_identity_source_roles_in(tx, &source(c.id), user)
                    .await?;
            }
        } else {
            let ended = self.sessions.revoke(tx, user, None).await?;
            let sessions = self.sessions.clone();
            tx.after_commit(move || sessions.announce(ended));
        }
        Ok(())
    }
    async fn scim_apply_roles_with(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        user: i64,
        roles: &[i64],
    ) -> Result<()> {
        let scope = self
            .authentication
            .authenticate_session(
                tx.sql(),
                c.tenant,
                user,
                None,
                Some(c.company),
                String::new(),
            )
            .await
            .map_err(|e| match e {
                Error::Unauthorized => Error::Forbidden,
                e => e,
            })?;
        if scope.current_company().map(|id| id.get()) != Some(c.company) {
            return Err(Error::Forbidden);
        }
        self.sso_grants
            .apply_identity_roles_in(tx, &scope, &source(c.id), roles)
            .await?;
        Ok(())
    }
    async fn scim_apply_roles(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        user: i64,
    ) -> Result<()> {
        let mut roles = c.config.default_role.into_iter().collect::<BTreeSet<_>>();
        for row in tx.sql().query("SELECT g.document->>'displayName' FROM user_scim_groups g JOIN user_scim_members m ON m.tenant_id=g.tenant_id AND m.provider_id=g.provider_id AND m.group_id=g.resource_id JOIN user_scim_users u ON u.tenant_id=m.tenant_id AND u.provider_id=m.provider_id AND u.resource_id=m.member_id WHERE u.tenant_id=$1 AND u.provider_id=$2 AND u.user_id=$3",&[&c.tenant,&c.id,&user]).await?{
            if let Some(mapped)=c.config.role_mapping.get(&row.get::<_,String>(0)){roles.extend(mapped);}
        }
        self.scim_apply_roles_with(tx, c, user, &roles.into_iter().collect::<Vec<_>>())
            .await
    }
    pub(crate) async fn scim_refresh_roles(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        administrator: Option<&Scope>,
        members: Option<&[String]>,
    ) -> Result<()> {
        let mut after = 0_i64;
        loop {
            let users=tx.sql().query("SELECT s.user_id,s.active,(EXISTS(SELECT 1 FROM user_desks d WHERE d.tenant_id=u.tenant_id AND d.person_id=u.person_id AND d.company_id=$3) AND u.person_id=s.person_id AND u.deleted_at IS NULL) IS TRUE FROM user_scim_users s JOIN users u ON u.tenant_id=s.tenant_id AND u.id=s.user_id WHERE s.tenant_id=$1 AND s.provider_id=$2 AND ($4::text[] IS NULL OR s.resource_id=ANY($4)) AND s.user_id>$5 ORDER BY s.user_id LIMIT 200", &[&c.tenant,&c.id,&c.company,&members,&after]).await?;
            if users.is_empty() {
                break;
            }
            for user in users {
                let id = user.get(0);
                after = id;
                let eligible = user.get::<_, bool>(1) && user.get::<_, bool>(2);
                let standing = self
                    .authentication
                    .authenticate(tx.sql(), c.tenant, id, String::new())
                    .await;
                match standing {
                    Ok(_) if eligible => self.scim_apply_roles(tx, c, id).await?,
                    Ok(_) | Err(Error::Unauthorized) => {
                        if let Some(scope) = administrator {
                            self.sso_grants
                                .clear_identity_roles_in(tx, scope, &source(c.id), id)
                                .await?;
                        } else {
                            self.sso_grants
                                .revoke_identity_source_roles_in(tx, &source(c.id), id)
                                .await?;
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
    async fn scim_save_group(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        old: Option<Value>,
        mut doc: Value,
    ) -> ScimResult<()> {
        let members = doc
            .as_object_mut()
            .ok_or_else(ScimError::invalid)?
            .remove("members")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let ids = members
            .iter()
            .map(|m| {
                m["value"]
                    .as_str()
                    .ok_or_else(ScimError::invalid)
                    .map(str::to_owned)
            })
            .collect::<ScimResult<Vec<_>>>()?;
        let id = doc["id"]
            .as_str()
            .ok_or_else(ScimError::invalid)?
            .to_owned();
        for member in &ids {
            if tx.sql().query_opt("SELECT 1 FROM user_scim_users WHERE tenant_id=$1 AND provider_id=$2 AND resource_id=$3",&[&c.tenant,&c.id,member]).await?.is_none(){return Err(ScimError::bad("invalidValue","A member does not belong to this provisioning connection."));}
        }
        let mut affected = ids.iter().cloned().collect::<BTreeSet<_>>();
        if let Some(previous) = old.as_ref().and_then(|v| v["members"].as_array()) {
            affected.extend(
                previous
                    .iter()
                    .filter_map(|v| v["value"].as_str().map(str::to_owned)),
            );
        }
        let affected = affected.into_iter().collect::<Vec<_>>();
        let (tenant, provider) = (c.tenant, c.id);
        let previous = old.unwrap_or(Value::Null);
        tx.mutate(move|sql|Box::pin(async move{
            let row:i64=if previous.is_null(){sql.query_one("INSERT INTO user_scim_groups(tenant_id,provider_id,resource_id,document) VALUES($1,$2,$3,$4) RETURNING id",&[&tenant,&provider,&id,&doc]).await?.get(0)}else{sql.query_one("UPDATE user_scim_groups SET document=$4 WHERE tenant_id=$1 AND provider_id=$2 AND resource_id=$3 RETURNING id",&[&tenant,&provider,&id,&doc]).await?.get(0)};
            let removed=sql.query("DELETE FROM user_scim_members WHERE tenant_id=$1 AND provider_id=$2 AND group_id=$3 AND NOT(member_id=ANY($4)) RETURNING id,member_id",&[&tenant,&provider,&id,&ids]).await?;
            let mut changes=vec![change("user.scim_group",row,previous,doc)];
            changes.extend(removed.iter().map(|r|change("user.scim_member",r.get(0),json!({"group":id,"member":r.get::<_,String>(1)}),Value::Null)));
            for member in ids {
                if let Some(r)=sql.query_opt("INSERT INTO user_scim_members(tenant_id,provider_id,group_id,member_id) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING RETURNING id",&[&tenant,&provider,&id,&member]).await?{changes.push(change("user.scim_member",r.get(0),Value::Null,json!({"group":id,"member":member})));}
            }
            Ok(((),changes))
        })).await?;
        self.scim_refresh_roles(tx, c, None, Some(&affected))
            .await?;
        Ok(())
    }
    async fn scim_delete_group(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        id: &str,
    ) -> ScimResult<()> {
        let (tenant, provider, id) = (c.tenant, c.id, id.to_owned());
        let affected = tx.mutate(move|sql|Box::pin(async move{
            let members=sql.query("DELETE FROM user_scim_members WHERE tenant_id=$1 AND provider_id=$2 AND group_id=$3 RETURNING id,member_id",&[&tenant,&provider,&id]).await?;
            let r=sql.query_one("DELETE FROM user_scim_groups WHERE tenant_id=$1 AND provider_id=$2 AND resource_id=$3 RETURNING id,document",&[&tenant,&provider,&id]).await?;
            let mut changes=vec![change("user.scim_group",r.get(0),r.get(1),Value::Null)];
            changes.extend(members.iter().map(|r|change("user.scim_member",r.get(0),json!({"group":id,"member":r.get::<_,String>(1)}),Value::Null)));
            Ok((members.iter().map(|r|r.get::<_,String>(1)).collect::<Vec<_>>(),changes))
        })).await?;
        self.scim_refresh_roles(tx, c, None, Some(&affected))
            .await?;
        Ok(())
    }
}
/// Parse conjunctions outside JSON strings. Only enum-selected SQL expressions
/// reach the query builder; all client values remain bound parameters.
fn filter(input: Option<&str>, group: bool) -> ScimResult<Vec<(&'static str, String)>> {
    let Some(input) = input else {
        return Ok(Vec::new());
    };
    if input.len() > 2048 {
        return Err(ScimError::filter());
    }
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escape) = (0, false, false);
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            escape = false;
        } else if quoted && b == b'\\' {
            escape = true;
        } else if b == b'"' {
            quoted = !quoted;
        }
        if !quoted
            && i + 5 <= bytes.len()
            && input.is_char_boundary(i)
            && bytes[i..i + 5].eq_ignore_ascii_case(b" and ")
        {
            parts.push(&input[start..i]);
            i += 5;
            start = i;
            continue;
        }
        i += 1;
    }
    if quoted {
        return Err(ScimError::filter());
    }
    parts.push(&input[start..]);
    if parts.len() > 8 {
        return Err(ScimError::filter());
    }
    parts
        .into_iter()
        .map(|s| {
            let (key, value) = quoted_eq(s)?;
            let sql = match key.as_str() {
                "id" => "resource_id",
                "externalid" => "document->>'externalId'",
                "username" if !group => "document->>'userName'",
                "displayname" => "document->>'displayName'",
                "active" if !group => "document->>'active'",
                "emails.value" | "emails[type eq \"work\"].value" if !group => {
                    "document#>>'{emails,0,value}'"
                }
                _ => return Err(ScimError::filter()),
            };
            Ok((sql, value))
        })
        .collect()
}
