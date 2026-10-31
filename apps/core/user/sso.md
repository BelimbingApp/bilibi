# Company sign-in

Connect each company to its identity provider with OpenID Connect (OIDC); leave it unconfigured to keep password sign-in unchanged.

## Connect Keycloak

Register Bilibi as a confidential OIDC client in your Keycloak realm, then configure the connection in Bilibi.

1. Register the exact callback `<APP_ORIGIN>/login/sso/callback`, with authorization code flow and PKCE S256. Disable implicit flow. Add `<APP_ORIGIN>/login` as an allowed post-logout address.
2. Open `/companies/<company ID>/sso` as an administrator with `admin.user.sso.manage`. Choose Keycloak, replace the realm address, and enter the client ID and secret. Save before enabling the connection.
3. Share the connection's **Sign-in address** with that company's people. Bilibi does not publish a directory of companies or providers on the anonymous sign-in page.
4. Add each account and link its exact provider subject, or let people who already signed in with their password link at `/settings/sso`. Self-linking also requires the provider's verified email to equal their existing account's email. An email alone never links an account.
5. Optionally enable account creation, list the **allowed email domains** and select a default role. A new person's email must be verified by the provider, end in a listed domain (exact match, no wildcards) and be unused in Bilibi. Saving with account creation on and no domain is refused. Without the list, a provider that serves many organisations under one issuer, such as Google, would let any of its users in. Account emails remain unique across the installation. Creation of Person, affiliation, account, external link and role grants commits together, attributed in audit to the connection rather than to an administrator. Saving a connection with account creation on needs `admin.user.create` as well as `admin.user.sso.manage`; a save by someone without it cannot turn account creation on or keep it on.
6. For roles, add the desired claim to **ID tokens** in Keycloak. Choose `realm_access.roles`, `resource_access.<client>.roles`, or a groups claim. Set the group-to-role mapping. Bilibi reapplies those grants at each sign-in and preserves manually assigned roles and direct permission denies.
7. For provider logout, register `<APP_ORIGIN>/login/sso/backchannel/<public connection ID>` on the Keycloak client. Enable back-channel logout with session IDs. Optionally enable **Also sign out of the provider** in Bilibi.

Use HTTPS in service. A company administrator names the issuer, so by default every discovery, key and token call goes only to public HTTPS addresses and an internal or loopback issuer is refused. The server operator can allow internal HTTPS providers (for example a self-hosted Keycloak) and literal-loopback HTTP for a disposable local provider by setting `BILIBI_SSO_ALLOW_PRIVATE_ENDPOINTS=1` in the server's environment, then restarting. No tenant or company setting can turn this on. The issuer must match discovery and the token exactly, including any trailing slash. By default, the generic method requests `openid email profile`, exchanges a code with PKCE, and verifies the ID token through fresh JWKS. Provider access tokens require the separate resource-server opt-in below.

## Provider access tokens

Leave **API access-token audience** and **MCP access-token audience** empty to accept only Bilibi's own credentials; configure either audience to enable that edge on this connection.

- Configure distinct resource audiences at the provider, different from the browser client ID and from each other. Set **Required access-token scopes** (default `bilibi`) to the space-separated scopes the provider must issue. An empty scope requirement is refused.
- Send the access token as `Authorization: Bearer …` together with `X-Bilibi-Identity-Provider: <public connection ID>` on every `/api/v1` or `/mcp` request. The public ID is in the connection's sign-in address. This explicit trust selector is not a credential and grants nothing; Bilibi does not scan companies or follow an issuer URL from an unverified token. Without the selector, existing Bilibi OAuth authentication is unchanged.
- The built-in OIDC verifier accepts signed JWT access tokens marked by Keycloak's signed `typ: Bearer` claim or the RFC 9068 protected `at+jwt` type. It checks the exact issuer, selected resource audience, signature, expiry, not-before and required scopes against fresh discovery and JWKS on every request. ID tokens and opaque provider tokens are not accepted. Provider key rotation therefore takes effect without restarting Bilibi.
- The exact issuer and subject must already be linked to an active staff account of this company and tenant. Access-token claims neither provision accounts nor assign roles. Ordinary permissions, direct denies, plan review, step-up and audit use that account, not a claim-supplied actor. Provider MFA claims alone never satisfy sensitive-action confirmation.
- First use creates an ordinary revocable AI connection, storing only a token digest and the provider/link/configuration binding. It expires with the provider token and issues no Bilibi access or refresh token. Revoking that connection refuses the same token on subsequent requests; a newly issued provider token can create a new connection. Provider-side revocation is not introspected: short token lifetimes and Bilibi suspension/unlinking bound that window.
- Disabling or saving the connection, unlinking its subject, suspending the account, or changing its company standing invalidates the grant on the next request and when a reviewed write reproves its authority. Saving configuration invalidates previously used tokens; obtain a fresh provider token afterwards.

The Keycloak preset has running verification; other providers must issue one of the supported signed access-token shapes and be verified before deployment.

## Keep identity in one place

Use Keycloak as the central broker when an organisation has several identity systems.

Bilibi speaks OIDC only to Keycloak. Keycloak connects Entra ID over OIDC or SAML, LDAP/Active Directory and legacy SAML applications, and applies its authentication policy. A new upstream provider changes Keycloak configuration. It does not change Bilibi authentication code. SAML, LDAP and provider-specific group lookup belong in the broker or an adopter-owned Extension. The platform can send [SCIM provisioning](scim.md) to Core User, using a SCIM client extension when the broker needs one.

## Require company sign-in

Keep a working emergency administrator before disabling passwords for linked people.

- Select an active administrator of that company as the emergency account. Bilibi checks its standing and sign-in management permission. Keep its password and local 2FA available outside the provider, and periodically prove a password sign-in.
- Enable **Require company sign-in for linked accounts**. It applies only to people linked to an enabled enforcing connection. Other companies and unlinked accounts keep their existing flow. A linked person who enters a correct password is told that their company requires company sign-in and to contact their administrator.
- The designated emergency account retains password entry. Turning SSO off restores the ordinary password path. To recover without the provider, use that account to disable enforcement or the connection.
- Whether a connection enforces sign-in and who the emergency account is are stored as plain columns beside the sealed record, so they apply even when the sealed settings cannot be opened (for example after the installation's settings key changed). Then the emergency account still signs in with a password, members of an enforcing connection get the company sign-in message, other members sign in as usual, and the connection is left out of sign-in and confirmation lists with a line in the server log. Open `/companies/<id>/sso?connection=<id>`: the page loads, names the connection as unreadable and shows an empty form. Enter its settings and client secret again and save, or turn it off.
- Provider configuration and the client secret are sealed with the installation's settings key, bound to the tenant and connection, in the connection's own record. Sign-in reads it without any administrator's session. The record names the administrator who last saved it for audit only: suspending or deleting that person leaves password sign-in, the emergency account and company sign-in working. Account creation on first sign-in does not depend on that administrator either: it runs as the connection, under the saved settings, and also keeps working after a re-save by an administrator who cannot create accounts, as long as account creation stays off or was saved by someone who can.

## Trust provider 2FA

Accept only authentication methods or assurance levels your identity provider actually enforces.

The default `amr` match is `mfa`; no `acr` value is trusted by default. Add an `acr` value only after configuring and proving its authentication flow at the provider. A matching signed result satisfies the sign-in factor. Otherwise the same pending sign-in stage used for passwords requires a local factor before admitting a session. An ordinary SSO sign-in never opens a sensitive-action confirmation window.

Set **Assurance level for confirming access** to allow provider re-authentication as the `core.user.provider` second-factor method. Confirm access at `/settings/sso/confirm`; Bilibi requests `max_age=0` and essential `acr`, and requires that exact assurance level and a recent `auth_time` in the signed result. The linked subject must still be the signed-in account, in the same browser session. Re-authentication opens that session's normal Step-up window. Existing Step-up policies still decide which operations require confirmation.

An explicit `StepUp::start` for this method records a short-lived purpose, with a per-account cooldown. After provider re-authentication, the browser can show a one-use confirmation code for that exact operation, target and channel. It expires after two minutes. This also lets an agent carry the account holder's proof using the existing method-qualified code format. Starting, reading or choosing a method never authenticates the person by itself.

## Claims and role grants

Read nested objects with dotted paths, exact claim keys, or JSON Pointers.

For example, `realm_access.roles` reads Keycloak realm roles, and `resource_access.bilibi.roles` reads roles for the `bilibi` client. An exact namespaced claim such as `https://example.com/groups` is read as one key. JSON Pointer `/resource_access/client.with.dots/roles` handles a dotted client ID. A mapping claim may be one string or an array of strings; malformed data refuses sign-in. An absent claim contributes no mapped roles, removing prior grants from that source on the next sign-in.

Authz owns the approved role bound (`authz_identity_policies`) and its separate automatic grants (`authz_identity_roles`). The effective role view combines those grants with `principal_roles`. Changing a policy removes grants that it no longer permits immediately. Changing claim-based group membership takes effect at the next sign-in. Accounts managed by [SCIM](scim.md) instead take their mapped roles from provisioned groups immediately; SSO claims cannot restore a role removed through provisioning. A default role belongs to accounts provisioned through that connection, rather than pre-existing accounts. Unlinking removes the source's role grants and revokes that person's sessions.

## Provider presets

The presets in [`presets/oidc.json`](presets/oidc.json) are data used by the same OIDC implementation.

| Preset | Issuer | Claim notes | Live account verification |
| --- | --- | --- | --- |
| Keycloak | `https://<host>/realms/<realm>` | Put realm/client roles or groups in ID tokens. Configure assurance levels at Keycloak. | Disposable local distribution; see runtime evidence |
| Microsoft Entra ID | `https://login.microsoftonline.com/<tenant-id>/v2.0` | Use a concrete tenant issuer. Configure optional email and group claims. Do not infer verified email from `preferred_username`; admin-link when no trustworthy verified-email claim is available. Group overage requires a broker or Extension. | Not verified |
| Google | `https://accounts.google.com` | No groups claim. Leave mapping empty unless an Extension supplies one. One issuer serves every Google account, so list your own email domains before enabling account creation. | Not verified |
| Okta | `https://<organisation>.okta.com/oauth2/default` | Configure groups in the ID token for the chosen authorization server. An organisation issuer can be configured instead. | Not verified |
| Auth0 | The exact tenant/custom-domain issuer, including its trailing slash | Configure a namespaced custom groups claim in an Action and put its exact name in Bilibi. | Not verified |

Provider references: [Entra claims](https://learn.microsoft.com/en-us/entra/identity-platform/id-token-claims-reference), [Google OIDC](https://developers.google.com/identity/openid-connect/openid-connect), [Okta groups](https://developer.okta.com/docs/guides/customize-tokens-groups-claim/), [Auth0 custom claims](https://auth0.com/docs/secure/tokens/json-web-tokens/create-custom-claims), and [Keycloak OIDC](https://www.keycloak.org/securing-apps/oidc-layers).

The four hosted-provider presets are documented templates. No live account was requested or used to verify them. Verify with an adopter before relying on provider-specific claim behavior. A provider without RP-initiated or back-channel logout still supports local Bilibi logout.

## Extension contract

Register another protocol through `UserService::register_identity_provider` in the Extension's service factory.

Declare `core/user` as a dependency. Implement `IdentityProvider`: `start`, `finish`, `logout`, and `backchannel`. The request carries opaque state, nonce, PKCE verifier, callback and optional required assurance level. Authenticate the protocol response before returning `ExternalIdentity { issuer, subject, claims, session_id, logout_hint }`. Those fields are trusted code output, never values copied from an unverified request. A protocol must enforce the requested freshness and assurance before returning a step-up result. Unavailable logout may return `None`; unsupported back-channel requests must refuse. The optional `access_token` method defaults to refusal; implementations authenticate the signature, exact configured issuer, requested audience, token kind and lifetime before returning trusted claims. Core checks the required scopes and existing account link.

Core retains browser binding, one-use flow state, tenant/company ownership, links, admission, role bounds and audit. An Extension must not create sessions, match email as identity, or write another owner's tables. Registration rejects duplicate or malformed method names. Adding a provider preset requires editing data only; another protocol requires an Extension.

## Runtime walkthrough

Use a disposable database and a local Keycloak distribution; never install the fixture into an adopter's database.

Run migration, then set `BILIBI_SSO_ALLOW_PRIVATE_ENDPOINTS=1` for both the app and the example, since the local Keycloak is on loopback HTTP, and run `BILIBI_SSO_WALKTHROUGH=disposable cargo run -p bilibi-user --example sso-walkthrough` with `DATABASE_URL` and the exact `APP_ORIGIN`. Without `BILIBI_SSO_ISSUER`, the command only installs the repository fixture. With that issuer and `BILIBI_SSO_CLIENT_SECRET`, it configures a local client named `bilibi`, links the fixture manager subject `bilibi-a-manager`, and prints only public connection addresses and an isolation outcome. The secret comes from the environment and is never printed.

Start the app with `./bilibi start` and use the printed sign-in address in a browser at that exact origin. Record successful linked sign-in, JIT creation, role changes, ordinary pending admission without provider 2FA, trusted provider 2FA, provider re-authentication, password enforcement and emergency recovery, local/provider/back-channel logout, isolation, state replay and invalid token refusals. To prove that an unreadable connection locks nobody out, run the example with `BILIBI_SSO_CASE=unreadable-connection` and `BILIBI_SSO_LOCAL_PASSWORD` set, before the next case: it damages the sealed record of connection 1, then reports the emergency and unenforced members still signing in, the enforced member's refusal, the tolerant lists and the re-save that restores it. To prove that retiring the administrator who last saved the connection locks nobody out, run the example last with `BILIBI_SSO_CASE=offboard-configurer`. It first checks that account creation without allowed domains is refused, then enforces company sign-in, deletes that administrator, then reports that a company member and the emergency account still sign in with a password and that company sign-in still starts. Finish in the browser: a linked person still signs in, a new person with a verified email on an allowed domain (`BILIBI_SSO_JIT_DOMAINS`, default `example.test`) is created, and one on another domain is refused. Keep credentials, cookies and tokens out of screenshots and evidence. See [`docs/runtime-parity.md`](../../../docs/runtime-parity.md).

## Upgrade notes for agents

Existing password adapters and factor implementations keep their current APIs. The operator-only environment variable `BILIBI_SSO_ALLOW_PRIVATE_ENDPOINTS=1` is new; leave it unset in service, and set it only where an identity provider is on a private network or loopback HTTP. `CredentialError` gains `CompanySignInRequired` for a correct password refused by enforcement; handle it where you match sign-in errors. Migration `core-user-017-sso-runtime` adds the sealed connection record; connections saved by earlier development builds must be saved once more, and a connection with account creation on must list `allowed_email_domains`. Person and Affiliation gain `provision_in` for a system principal (`Context::system`) that has authorized the creation itself; user contexts are refused, and `bilibi_company::lock_account_company_in` locks the company without an account scope. An optional factor may implement the new defaulted `Provider::available` read to hide itself until configured. Read effective role memberships through Authz's owner APIs or `effective_principal_roles`; `principal_roles` contains administrator assignments only. Other protocols depend on `core/user` and register through the identity seam above. Provider access-token audiences default to disabled; protocols may implement the default-denying `IdentityProvider::access_token` hook. Core Agent Access migration `core-agent-access-005-provider-access` records provider grant bindings; no account conversion or data backfill is required.

## OIDC client options

**Use the requested scopes and client authentication fields when your provider requires them.** Scopes default to `openid email profile`; include `openid` and add a group scope when the provider needs one. Automatic authentication uses a public client when no secret is supplied, otherwise discovery chooses `client_secret_basic` when supported and `client_secret_post` as the fallback. You can select either confidential-client method explicitly. Extensions receive these as `protocol.scopes` and `protocol.token_auth`. Changing the method, issuer, client ID or client authentication method while accounts are linked requires explicit unlinking first, because it creates a new trust relationship. Scopes and other options can change at any time.

**Configuration refusals identify the failing check without including secret values.** Check your signed-in account, company access, installation encryption key, or saved configuration as the message directs. The messages are written by this module and never include credentials, claims or request data.
