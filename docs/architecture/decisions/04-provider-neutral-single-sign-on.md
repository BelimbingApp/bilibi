# ADR 04: Provider-neutral single sign-on

**Document Type:** Architecture Decision Record
**Status:** Accepted
**Agents:** codex/gpt-6 (author)
**Scope:** Optional company SSO, identity-provider registration and protocol Extensions
**Last Updated:** 2026-10-21

> Source: [issue 109](https://github.com/BelimbingApp/bilibi/issues/109). Only known agent identity parts are included.

## Context

**Companies need enterprise sign-in without provider-specific Core code.** Initial presets cover Microsoft Entra ID (Azure AD), Keycloak, Google, Okta and Auth0. Adopters that do not need SSO must have no extra setup or change in behaviour.

## Decision

**Core supports OpenID Connect (OIDC); providers are configuration, and other protocols use Extensions.**

| Layer | Where | Responsibility |
| --- | --- | --- |
| Identity-provider registry | Base/Core | A provider-neutral hook starts external sign-in, receives its result and hands Core a verified identity: issuer, subject and claims. All methods share the pending-sign-in lifecycle. |
| Built-in OIDC method | Core | Discovery, authorization code flow with PKCE, state and nonce checks, ID token signature/issuer/audience/expiry validation and JWKS key rotation. No provider-specific code. |
| Provider presets | Data | Display name, issuer URL pattern and email/name/group claim names for Entra ID, Keycloak, Google, Okta and Auth0. Company admins supply their provider URL, client ID and secret. Adding a preset needs no code change. Claim differences are data: Google has no groups claim; Auth0 groups use a namespaced custom claim. |
| Other protocols and provider quirks | Extensions | Register through the same hook, without Core depending on them. Examples include SAML, LDAP and fetching omitted Entra groups through Microsoft Graph. |

### Behaviour and defaults

- **Off by default.** Each company connects its own providers inside its tenant. Configuration and client secrets are sealed with the installation key and bound to their tenant and connection; there is no server-wide provider.
- **Link by issuer and subject.** Never match by email alone. Linking an existing account first requires a verified email match and its existing sign-in, or an admin link.
- **Pre-added accounts by default.** A company may enable account creation on first sign-in with a default role.
- **Bilibi roles by default.** A company may map provider groups or roles to Bilibi roles using configurable claims, including nested `realm_access.roles` and `resource_access.<client>.roles`. Reapply mappings at every sign-in and audit them. Bilibi retains its own fine-grained permissions.
- **Provider MFA satisfies sign-in 2FA.** When `amr`/`acr` reports multi-factor authentication, skip Bilibi's sign-in second factor. Sensitive actions retain the existing Step-up policy. A company may register provider re-authentication (`max_age=0` and a required `acr` level) as a step-up method.
- **Optional SSO enforcement.** A company may disable password sign-in for SSO-linked people while retaining a documented break-glass administrator.
- **Local sign-out always ends the Bilibi session.** Provider sign-out (RP-initiated logout) is optional per provider. Accept OIDC back-channel logout to end the person's Bilibi sessions promptly when the provider signals logout or disablement.
- **Audit configuration changes, links, unlinks, provisioning, role mapping changes and every SSO sign-in**, including success or refusal reason. Secrets must never appear in logs, saved-value pages, Git or release artifacts.
- **Agent access stays unchanged.** OAuth for a person, `/api/v1` and `/mcp` keep acting for that person, whose account may be SSO-linked.
- **Provider access tokens are a second phase.** A company may opt into `/api/v1` and `/mcp` accepting its provider's access tokens. Validate issuer, audience, signature, expiry and scopes; act as the linked person under Bilibi's normal permissions, step-up and audit. Refuse tokens for another company.

### Central identity platform

**Recommend an identity platform such as Keycloak as the enterprise broker.** Bilibi speaks OIDC to it; the platform federates Entra ID, LDAP/Active Directory and SAML sources and enforces MFA. Adding an identity source changes the platform's configuration. Document this pattern with Keycloak as the worked example.

## Dependencies

**Implementation builds on the merged pending-sign-in stage.** Password and external identity must both enter pending sign-in before admission. SSO must use that stage rather than create a separate admission path.

## Consequences

**All five providers use configuration rather than separate plug-ins.** Keycloak is the provider to test against a disposable instance. Entra ID, Google, Okta and Auth0 ship as documented presets marked not yet verified against a live account; live verification waits for adopters. Extensions are needed only for another protocol or genuine provider-specific behaviour. The hook keeps those additions optional.

Implementation must prove provider and account isolation between companies and tenants, unchanged behaviour with SSO off, and the sign-in and token-refusal cases in [issue 109's acceptance criteria](https://github.com/BelimbingApp/bilibi/issues/109). Browser evidence follows [runtime parity](../../runtime-parity.md).

## Out of scope

- SAML and LDAP inside Bilibi; use the broker, or an Extension for adopters needing direct support.
- MFA mechanisms themselves; the provider owns them and Bilibi honours its result.
- SCIM provisioning and deprovisioning, recommended as the next enterprise item. Provider removal alone does not remove a Bilibi account; its session must end or an admin must act.
- Bilibi acting as an identity provider for other systems.

## Current implementation

**The built-in OIDC method and optional company configuration implement the first phase.** See [Core User company sign-in](../../../apps/core/user/sso.md) for the registry, behavior, presets and live verification limits. Provider access tokens for APIs remain a follow-up.
