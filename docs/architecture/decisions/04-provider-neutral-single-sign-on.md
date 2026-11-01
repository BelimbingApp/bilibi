# ADR 04: Provider-neutral single sign-on

**Document Type:** Architecture Decision Record
**Status:** Accepted; implemented
**Agents:** codex/gpt-6 (author)
**Scope:** Optional company SSO, identity-provider registration and protocol Extensions
**Last Updated:** 2026-10-27

## Decision

**Core supports OpenID Connect (OIDC); providers are configuration, and other protocols use Extensions.** Company single sign-on (SSO) is optional. Companies that do not enable it keep their existing sign-in behaviour.

- A provider-neutral registry lets Extensions add identity protocols without Core depending on them.
- One built-in OIDC method serves providers such as Microsoft Entra ID, Keycloak, Google, Okta and Auth0. Provider presets and claim differences are data.
- Core retains account linking, admission, permissions and audit across all sign-in methods.
- Use an identity platform such as Keycloak as the enterprise broker when several identity sources are needed. Bilibi speaks OIDC to the broker; the broker federates upstream sources and enforces their authentication policy.

The decision's first phase shipped in [the SSO implementation](https://github.com/BelimbingApp/bilibi/pull/144), closing [issue 109](https://github.com/BelimbingApp/bilibi/issues/109). The second phase ([issue 190](https://github.com/BelimbingApp/bilibi/issues/190)) adds optional provider access tokens on `/api/v1` and `/mcp`: an explicit public connection selector chooses existing trust, distinct resource audiences prevent browser/API/MCP token confusion, and Core User verifies the exact linked account. Core Agent Access keeps its normal revocable grant, plan reproof, step-up and audit rather than introducing a parallel machine authority. Provider tokens never mint Bilibi OAuth tokens.

SCIM provisioning and deprovisioning are separate from this sign-in decision. The follow-up [Core User SCIM service provider](../../../apps/core/user/scim.md) handles account provisioning and immediate deactivation. Without it, removing a person in the provider does not remove their Bilibi account until their session ends or an admin acts.

See [Core User](../../../apps/core/user/README.md) and [company sign-in](../../../apps/core/user/sso.md) for the implementation contract, configuration, Extension registration and verification limits.

## Reasons

**Companies need enterprise sign-in without provider-specific Core code.**

- OIDC gives the supported providers one shared protocol implementation. Configuration handles their issuer and claim differences without separate releases of authentication code.
- Optional company connections let adopters choose SSO without adding setup for companies that do not need it.
- A shared admission path keeps account ownership, permissions and audit under Bilibi's control regardless of the identity source.
- A central broker keeps federation and authentication policy in one place. Adding an upstream identity source changes the broker's configuration rather than Bilibi's Core.
- The registry leaves room for direct protocol support or genuine provider-specific behaviour while keeping those dependencies optional.

## Rejected alternatives

| Alternative | Why rejected |
| --- | --- |
| A plug-in for each OIDC provider | Duplicates the same protocol machinery and turns issuer and claim differences into code dependencies. Presets cover those differences as data. |
| SAML or LDAP in Core | Adds protocols that a central broker can translate to OIDC. Adopters needing direct support can use an Extension. |
| Provider-specific behaviour in Core, such as fetching omitted Entra groups through Microsoft Graph | Couples Core to one provider. Such behaviour belongs in the broker or an Extension. |
| A separate external sign-in admission path | Would split account admission and security rules between password and external sign-in. All methods use the shared pending-sign-in lifecycle. |
