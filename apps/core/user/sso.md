# Company sign-in

Connect each company to its identity provider with OpenID Connect (OIDC); leave it unconfigured to keep password sign-in unchanged.

## Connect Keycloak

Register Bilibi as a confidential OIDC client in your Keycloak realm, then configure the connection in Bilibi.

1. Register the exact callback `<APP_ORIGIN>/login/sso/callback`, with authorization code flow and PKCE S256. Disable implicit flow. Add `<APP_ORIGIN>/login` as an allowed post-logout address.
2. Open `/companies/<company ID>/sso` as an administrator with `admin.user.sso.manage`. Choose Keycloak, replace the realm address, and enter the client ID and secret. Save before enabling the connection.
3. Share the connection's **Sign-in address** with that company's people. Bilibi does not publish a directory of companies or providers on the anonymous sign-in page.
4. Add each account and link its exact provider subject, or let people who already signed in with their password link at `/settings/sso`. Self-linking also requires the provider's verified email to equal their existing account's email. An email alone never links an account.
5. Optionally enable account creation and select a default role. A new person's email must be verified by the provider and unused in Bilibi. Account emails remain unique across the installation. Creation of Person, affiliation, account, external link and role grants commits together.
6. For roles, add the desired claim to **ID tokens** in Keycloak. Choose `realm_access.roles`, `resource_access.<client>.roles`, or a groups claim. Set the group-to-role mapping. Bilibi reapplies those grants at each sign-in and preserves manually assigned roles and direct permission denies.
7. For provider logout, register `<APP_ORIGIN>/login/sso/backchannel/<public connection ID>` on the Keycloak client. Enable back-channel logout with session IDs. Optionally enable **Also sign out of the provider** in Bilibi.

Use HTTPS in service. Literal loopback HTTP is accepted for a disposable local identity provider. The issuer must match discovery and the token exactly, including any trailing slash. By default, the generic method requests `openid email profile`, exchanges a code with PKCE, and verifies the ID token through fresh JWKS; it never accepts a provider access token as a Bilibi credential.

## Keep identity in one place

Use Keycloak as the central broker when an organisation has several identity systems.

Bilibi speaks OIDC only to Keycloak. Keycloak connects Entra ID over OIDC or SAML, LDAP/Active Directory and legacy SAML applications, and applies its authentication policy. A new upstream provider changes Keycloak configuration. It does not change Bilibi authentication code. SAML, LDAP, SCIM and provider-specific group lookup belong in the broker or an adopter-owned Extension.

## Require company sign-in

Keep a working emergency administrator before disabling passwords for linked people.

- Select an active administrator of that company as the emergency account. Bilibi checks its standing and sign-in management permission. Keep its password and local 2FA available outside the provider, and periodically prove a password sign-in.
- Enable **Require company sign-in for linked accounts**. It applies only to people linked to an enabled enforcing connection. Other companies and unlinked accounts keep their existing flow.
- The designated emergency account retains password entry. Turning SSO off restores the ordinary password path. To recover without the provider, use that account to disable enforcement or the connection.
- Provider configuration is stored in encrypted company Settings as `user.sso.<connection ID>`. The administrator who saved it supplies the proved settings-reader identity at authentication bootstrap. Before retiring that administrator, save the connection as its successor; a frozen or deleted configuration owner makes the connection unavailable.

## Trust provider 2FA

Accept only authentication methods or assurance levels your identity provider actually enforces.

The default `amr` match is `mfa`; no `acr` value is trusted by default. Add an `acr` value only after configuring and proving its authentication flow at the provider. A matching signed result satisfies the sign-in factor. Otherwise the same pending sign-in stage used for passwords requires a local factor before admitting a session. An ordinary SSO sign-in never opens a sensitive-action confirmation window.

Set **Assurance level for confirming access** to allow provider re-authentication as the `core.user.provider` second-factor method. Confirm access at `/settings/sso/confirm`; Bilibi requests `max_age=0` and essential `acr`, and requires that exact assurance level and a recent `auth_time` in the signed result. The linked subject must still be the signed-in account, in the same browser session. Re-authentication opens that session's normal Step-up window. Existing Step-up policies still decide which operations require confirmation.

An explicit `StepUp::start` for this method records a short-lived purpose, with a per-account cooldown. After provider re-authentication, the browser can show a one-use confirmation code for that exact operation, target and channel. It expires after two minutes. This also lets an agent carry the account holder's proof using the existing method-qualified code format. Starting, reading or choosing a method never authenticates the person by itself.

## Claims and role grants

Read nested objects with dotted paths, exact claim keys, or JSON Pointers.

For example, `realm_access.roles` reads Keycloak realm roles, and `resource_access.bilibi.roles` reads roles for the `bilibi` client. An exact namespaced claim such as `https://example.com/groups` is read as one key. JSON Pointer `/resource_access/client.with.dots/roles` handles a dotted client ID. A mapping claim may be one string or an array of strings; malformed data refuses sign-in. An absent claim contributes no mapped roles, removing prior grants from that source on the next sign-in.

Authz owns the approved role bound (`authz_identity_policies`) and its separate automatic grants (`authz_identity_roles`). The effective role view combines those grants with `principal_roles`. Changing a policy removes grants that it no longer permits immediately. Changing group membership takes effect at the next sign-in. A default role belongs to accounts provisioned through that connection, rather than pre-existing accounts. Unlinking removes the source's role grants and revokes that person's sessions.

## Provider presets

The presets in [`presets/oidc.json`](presets/oidc.json) are data used by the same OIDC implementation.

| Preset | Issuer | Claim notes | Live account verification |
| --- | --- | --- | --- |
| Keycloak | `https://<host>/realms/<realm>` | Put realm/client roles or groups in ID tokens. Configure assurance levels at Keycloak. | Disposable local distribution; see runtime evidence |
| Microsoft Entra ID | `https://login.microsoftonline.com/<tenant-id>/v2.0` | Use a concrete tenant issuer. Configure optional email and group claims. Do not infer verified email from `preferred_username`; admin-link when no trustworthy verified-email claim is available. Group overage requires a broker or Extension. | Not verified |
| Google | `https://accounts.google.com` | No groups claim. Leave mapping empty unless an Extension supplies one. | Not verified |
| Okta | `https://<organisation>.okta.com/oauth2/default` | Configure groups in the ID token for the chosen authorization server. An organisation issuer can be configured instead. | Not verified |
| Auth0 | The exact tenant/custom-domain issuer, including its trailing slash | Configure a namespaced custom groups claim in an Action and put its exact name in Bilibi. | Not verified |

Provider references: [Entra claims](https://learn.microsoft.com/en-us/entra/identity-platform/id-token-claims-reference), [Google OIDC](https://developers.google.com/identity/openid-connect/openid-connect), [Okta groups](https://developer.okta.com/docs/guides/customize-tokens-groups-claim/), [Auth0 custom claims](https://auth0.com/docs/secure/tokens/json-web-tokens/create-custom-claims), and [Keycloak OIDC](https://www.keycloak.org/securing-apps/oidc-layers).

The four hosted-provider presets are documented templates. No live account was requested or used to verify them. Verify with an adopter before relying on provider-specific claim behavior. A provider without RP-initiated or back-channel logout still supports local Bilibi logout.

## Extension contract

Register another protocol through `UserService::register_identity_provider` in the Extension's service factory.

Declare `core/user` as a dependency. Implement `IdentityProvider`: `start`, `finish`, `logout`, and `backchannel`. The request carries opaque state, nonce, PKCE verifier, callback and optional required assurance level. Authenticate the protocol response before returning `ExternalIdentity { issuer, subject, claims, session_id, logout_hint }`. Those fields are trusted code output, never values copied from an unverified request. A protocol must enforce the requested freshness and assurance before returning a step-up result. Unavailable logout may return `None`; unsupported back-channel requests must refuse.

Core retains browser binding, one-use flow state, tenant/company ownership, links, admission, role bounds and audit. An Extension must not create sessions, match email as identity, or write another owner's tables. Registration rejects duplicate or malformed method names. Adding a provider preset requires editing data only; another protocol requires an Extension.

## Runtime walkthrough

Use a disposable database and a local Keycloak distribution; never install the fixture into an adopter's database.

Run migration, then `BILIBI_SSO_WALKTHROUGH=disposable cargo run -p bilibi-user --example sso-walkthrough` with `DATABASE_URL` and the exact `APP_ORIGIN`. Without `BILIBI_SSO_ISSUER`, the command only installs the repository fixture. With that issuer and `BILIBI_SSO_CLIENT_SECRET`, it configures a local client named `bilibi`, links the fixture manager subject `bilibi-a-manager`, and prints only public connection addresses and an isolation outcome. The secret comes from the environment and is never printed.

Start the app with `./scripts/start-app.sh` and use the printed sign-in address in a browser at that exact origin. Record successful linked sign-in, JIT creation, role changes, ordinary pending admission without provider 2FA, trusted provider 2FA, provider re-authentication, password enforcement and emergency recovery, local/provider/back-channel logout, isolation, state replay and invalid token refusals. Keep credentials, cookies and tokens out of screenshots and evidence. See [`docs/runtime-parity.md`](../../../docs/runtime-parity.md).

## Upgrade notes for agents

Existing password adapters and factor implementations keep their current APIs. If your code exhaustively matches `bilibi_database::Error`, handle the new `InvalidReason` variant as invalid input and preserve its safe message. An optional factor may implement the new defaulted `Provider::available` read to hide itself until configured. Read effective role memberships through Authz's owner APIs or `effective_principal_roles`; `principal_roles` contains administrator assignments only. Other protocols depend on `core/user` and register through the identity seam above. APIs accepting provider access tokens remain a separate follow-up; `/api/v1` and `/mcp` accept their existing Bilibi credentials only.

## OIDC client options

**Use the requested scopes and client authentication fields when your provider requires them.** Scopes default to `openid email profile`; include `openid` and add a group scope when the provider needs one. Automatic authentication uses a public client when no secret is supplied, otherwise discovery chooses `client_secret_basic` when supported and `client_secret_post` as the fallback. You can select either confidential-client method explicitly. Extensions receive these as `protocol.scopes` and `protocol.token_auth`. Changing protocol configuration while accounts are linked requires explicit unlinking first.

**Configuration refusals identify the failing check without including secret values.** Check your signed-in account, company access, installation encryption key, or saved configuration as the message directs. `bilibi_database::Error::InvalidReason` carries only static, owner-written messages; never construct a message from credentials, claims or request data.
