# Automatic provisioning (SCIM)

A company can let its identity platform create people, update their profiles and remove their access through SCIM 2.0.

## Company setup

For example, removing someone from the identity platform's Bilibi assignment sends `active=false`. Bilibi ends their sessions immediately and keeps their business history.

1. Open **Company sign-in** for the company and save a connection. Interactive SSO may stay off.
2. Set the connection's default role and group-to-role mapping. These are the same approved roles used for company sign-in.
3. Under **Automatic provisioning (SCIM)**, choose **Create SCIM token**. Copy the masked token into your provider's secret field and store it securely. It appears only on this page immediately after creation; leaving or reloading loses that display.
4. Use the displayed SCIM address as the provider's base URL, with `Authorization: Bearer <credential>` and `Content-Type: application/scim+json`.
5. Provision a disposable person first. Prove creation, group membership, removal and reactivation before assigning everyone.

**There is no separate enable setting.** A company's valid token activates SCIM. Revoke it to turn provisioning off (requests then return 401). Rotation immediately replaces the old token. Revocation keeps existing people and access; deprovision the people first if that is your intention. Only one connection in each company can hold a token.

The token is generated with 244 random bits, stored sealed with the installation settings key and bound to its tenant and connection. It is never included in audit evidence. Creating or rotating a token requires company sign-in management, account creation and role management permission, with the existing sensitive-action confirmation policy. Revocation needs company sign-in management permission.

## People and sign-in

`userName` is the account's email-shaped sign-in name. Bilibi creates a Person, business affiliation, staff account and first desk in one audited transaction. The staff account stores only the Person. The supported primary/work email updates the Person and affiliation; display name updates the Person and account. No password is supplied by SCIM, and mailbox verification remains separate.

An existing local account is never adopted by matching an email. A duplicate sign-in address returns 409. A provisioned account's Person and business affiliation remain bound to the original provisioning record; a later local Person reassignment cannot let the old connection edit a different identity. Removing the provider company's desk does not stop that provider from updating or deactivating its own provisioned record.

`externalId` is optional and immutable once created. When supplied, Bilibi links it as the connection's OIDC subject under its exact issuer. **Configure the provisioning identifier to equal the OIDC subject before creating people.** An Entra object ID and an OIDC pairwise subject are different identifiers: do not assume they match. When your provider cannot send that subject, omit `externalId` and use the existing administrator identity link with the actual issuer and subject. Bilibi never guesses the subject from an email.

`active=false` suspends provisioning access independently of local account status. DELETE Users has the same effect. Sessions and pending sign-ins end in the transaction, and live browsers are notified after commit. Inactive records remain queryable. A PUT that omits `active` keeps the stored value, so a profile sync never restores access; only an explicit `active=true` does. `active=true` restores the same account and identifiers, with fresh sign-in required. It refuses a locally suspended account, retired Person, unlinked provisioning affiliation or reassigned Person. It does not recreate a removed desk. The write is refused with HTTP 403 and rolled back, never 401, which is reserved for a bad provisioning token. Neither SCIM nor SSO can undo those local restrictions.

SCIM is access management, not personal-data erasure. Deactivation preserves people, accounts, memberships and business history. Privacy requests and retention remain separate under [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md).

## Groups and roles

Group `displayName` values match the existing connection's group-to-role mapping exactly.

SCIM manages its own Authz grant source under the existing approved role policy. Adding/removing membership, renaming/deleting a group or saving a new mapping recalculates its grants immediately. Unmapped groups grant nothing; a configured default role applies to active SCIM accounts that still have the provider company's desk. Removing that desk revokes this provider's role source and the person's sessions in the same transaction. A later SCIM sync does not restore those roles without the desk. Manual roles and direct denies remain independent. SSO does not reapply claim-based roles to a SCIM-managed account, so stale claims cannot restore access removed by provisioning.

Only direct memberships of Users created by this connection are accepted. Nested groups, cross-company members and arbitrary role IDs supplied by a client are refused.

## Supported protocol

The base URL is `<APP_ORIGIN>/scim/v2/<connection-public-id>`. All resource URLs and Location headers use the configured origin.

| Resource | Methods and fields |
| --- | --- |
| `/Users`, `/Users/{id}` | GET, POST collection; GET, PUT, PATCH, DELETE individual. `userName`, `externalId`, `displayName`, `name.formatted/givenName/familyName`, primary/work `emails`, Boolean `active`; read-only group membership. |
| `/Groups`, `/Groups/{id}` | GET, POST collection; GET, PUT, PATCH, DELETE individual. Unique company-local `displayName`, `externalId`, direct `members.value`. |
| `/ServiceProviderConfig` | GET; advertises PATCH, filters and a maximum page size of 200. |
| `/ResourceTypes`, `/Schemas` | GET collection and individual entries. |

- Lists use one-based `startIndex` and `count` (default 100, maximum 200; zero requests a count only), stable ordering and SCIM ListResponse even for zero matches.
- Filters support `eq` on `id`, `externalId`, `displayName`, User `userName`, `active`, `emails.value` and `emails[type eq "work"].value`, joined by `and`. Other expressions return `invalidFilter`.
- `attributes` and `excludedAttributes` select whole attributes, including `excludedAttributes=members`. Mandatory `id` and `schemas` remain.
- PATCH accepts case-insensitive attribute and operation names, pathless objects, profile subattributes, whole membership add/replace/remove, and `members[value eq "<id>"]` removal. Entra's member removal with an explicit value list and string Boolean values are accepted.
- PUT replaces supported writable fields; PATCH changes only its supplied fields. User `externalId` and all resource `id` values cannot change. A PUT that omits the existing immutable User `externalId` preserves it. Unsupported writable fields are ignored on full resource writes, while unsupported PATCH paths are refused.
- Errors use SCIM Error documents and `application/scim+json`. Credentials never appear in responses except the one-time administrator creation display.

There is no bulk, sort, ETag, cursor, arbitrary schema extension, nested group or password synchronization support. DELETE Groups removes the group and its own role grants; DELETE Users deactivates and retains the resource.

## Provider configuration

| Provider | Setup |
| --- | --- |
| Entra ID | Use a non-gallery application's automatic provisioning with the displayed base URL and bearer secret. Map `userName` to the sign-in email and use supported matching attributes. Configure group provisioning and map group display names to Bilibi roles. |
| Okta | Use its SCIM 2.0 connector with bearer authentication. User existence filters, full PUT profile updates, PATCH activation and Group Push use the same endpoint. |
| Keycloak | Install and operate a SCIM client extension in Keycloak; SCIM is not part of Keycloak's built-in OIDC server. Configure its outbound base URL/token and immutable subject mapping. Federated Entra, AD/LDAP or SAML sources stay behind Keycloak. |

The implementation follows [RFC 7643](https://www.rfc-editor.org/rfc/rfc7643), [RFC 7644](https://www.rfc-editor.org/rfc/rfc7644), [Entra's endpoint requirements](https://learn.microsoft.com/en-us/entra/identity/app-provisioning/use-scim-to-provision-users-and-groups) and [Okta's SCIM 2.0 request shapes](https://developer.okta.com/docs/api/openapi/okta-scim/guides/scim-20). Protocol-script proof is separate from live provider verification: no Entra, Okta or Keycloak extension account has been certified by this change.

## Verification

Run `scripts/scim-conformance.py` against a disposable running installation. It reads its bearer credential from a file descriptor or environment and prints only case results and public resource IDs. Use the repository walkthrough fixture, never a real company's accounts. The script is a live integration walkthrough, not a unit-test suite.

Keep `DATABASE_URL`, the installation keys and `APP_ORIGIN` identical to the running disposable app. First build and run `cargo run -p bilibi-user --example scim-walkthrough --locked` with `BILIBI_SCIM_WALKTHROUGH=disposable` to install the walkthrough fixture and two disabled SSO connections in a fresh migrated database. Create their tokens in the browser. Keep the first token in a private file outside the repository, with mode 0600, and supply these environment values:

| Variable | Value |
| --- | --- |
| `SCIM_BASE_URL` | First company's displayed SCIM address |
| `SCIM_OTHER_BASE_URL` | Second company's displayed SCIM address |
| `SCIM_TOKEN_FILE` | Private file containing the first company's token |
| `BILIBI_SCIM_WALKTHROUGH` | `disposable` |

Then run the walkthrough:

```sh
python3 scripts/scim-conformance.py \
  --session-helper target/debug/examples/scim-walkthrough \
  --output /tmp/bilibi-scim-results.json
```

The session helper uses the real account facade to set a random password and admit a session for the newly provisioned disposable account. It writes that credential only to a temporary private file, removed after the session-revocation proof. It also reads effective roles and owner records to verify immediate role and profile changes. Results contain public resource IDs and case outcomes, with no credentials.

The script exercises the HTTP protocol. The token controls also require browser proof at the exact `APP_ORIGIN`, as described in [runtime parity](../../../docs/runtime-parity.md). `/api/v1` and `/mcp` keep their existing credentials and authorization; their account proof now also refuses a SCIM-deactivated person.
