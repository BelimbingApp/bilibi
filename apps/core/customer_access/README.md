# Core Customer Access

A tenant's customers sign in to its portal (`/portal/...`), never to the staff application ([ADR 16](../../../docs/architecture/decisions/16-customer-accounts-and-portal.md)). This module owns the portal site, the invitations that admit a customer, the requests people make for an account, the staff controls over the accounts invitations made, and the portal's own home and account pages.

## Contract

| Item | Value |
| --- | --- |
| Facade | `CustomerAccess`, service `core/customer_access.access` |
| Staff page | `/customer-access` (`admin.customer-access.view`; writes need `admin.customer-access.manage`) |
| Portal pages | `/portal` and `/portal/account` (`portal.account.view`) |
| Portal role | `portal_customer`, given to every customer an invitation admits |
| Settings | `customer_portal.invitation_days` (tenant, default 7), `customer_portal.registration_days` (installation, default 3) |
| Tables | `customer_portal_sites`, `customer_invitations`, `customer_registrations`; customer accounts stay in Core User's `users` |

## Portal site

- One per tenant: an **address** (`acme`, lowercase letters, digits and inner hyphens) and a **title**.
- The address names the tenant before anyone signs in: customers sign in at `/portal/sign-in/{address}`. It is unique across the installation.
- The title brands the portal's anonymous cards, its layout and its emails.
- The **logo** is a PNG, JPEG, GIF or WebP image of at most 1 MiB, chosen in the site form (or `logo_asset_id` / `remove_logo` on `core.customer_access.site.set`, naming an image already stored in Media). It is stored through `Media::put_public_image` on the attachment disk and shows on the anonymous cards and in the portal layout through a short-lived signed Media link (`Media::tenant_stream_url`, the tenant taken from the site record). A replaced logo's file stays in Media until an operator removes it.
- Changing the address breaks links already emailed.
- **Off until staff turn it on** (`enabled`, issue 137 behaviour 1). While off, its anonymous pages answer 404 (`portal_site` returns `None`), its signed-in customers' pages, downloads and live socket answer 404 (the host checks `site_of`), and no invitation is sent or redeemed.

## Invitations

- Staff invite a **Person**, optionally through one of its **affiliations** (a business customer, signing in for that company); without one the customer is a private individual.
- The invite dialog finds the Person by name on the server (`base/records` search, with the actor's permission to see people), so any number of people can be invited. An agent Person is refused: agents act through Agent Access, never the portal.
- The address must not already belong to a customer of the tenant. Inviting it again revokes the open invitation first.
- The email carries a one-time link, `/portal/accept/{address}/{token}`. The token is stored hashed and never read back, not even by agents.
- Accepting creates the account through Core User (`create_customer_in`, address verified), gives it `portal_customer` through Authz (`assign_portal_role_in`) and closes the invitation, in one transaction audited as `system · customer_access`. The host then signs the customer in with the password just chosen, through the ordinary portal sign-in; when that cannot finish (a tenant that requires a second step), the customer signs in on the next page.
- **Resend** sends a new link to an invitation's Person and address through the same contract as a new invitation, which revokes the old link.
- A Person that retired, an affiliation that ended or a company that was archived since the invitation makes the link invalid.

## Requests for an account

- **Off until staff turn it on** (`registration` on the site, issue 137 behaviour 7). While off, or while the portal is off, `/portal/register/{address}` and `/portal/verify/...` answer 404 and the sign-in card has no link to them.
- Someone gives a name, an email and an optional company name. The answer is the same for known and unknown addresses. A live unverified request or one waiting for staff stays unchanged and sends no new mail, even for concurrent requests from different senders; only an expired unverified request is replaced. The existing token remains valid until `customer_portal.registration_days` expires. The tenant/normalized-recipient unique index arbitrates creation in the owner transaction. Requests are also bounded per sender with password recovery (`user.recovery.per_sender_per_minute`).
- The emailed link (`/portal/verify/{address}/{token}`, hashed, single use) shows a button; pressing it confirms the address, so a mail scanner that opens links confirms nothing. An unconfirmed request expires after `customer_portal.registration_days`; a confirmed one waits.
- Nothing is written to Person or Company. Staff **approve** a confirmed request by choosing the Person (matching or creating it first) and, for a business customer, its affiliation: this sends the ordinary invitation with the request's name and confirmed address, through the same checks. **Reject** closes it; the person is not told and may ask again.
- Each step is audited: the request and its confirmation as `system · customer_access`, the decision as the staff member.

## Portal access panel

- A contact's record page (`core/person`) shows this panel (embed `person.portal-access`, `src/panel.rs`, `VIEW`): the Person's customer accounts and the invitations sent to them, with Invite, Resend, Revoke invitation, Revoke access and Restore.
- Invite asks which of the Person's current affiliations the customer signs in under, or "as an individual", and an address (default: the chosen affiliation's work email, else the Person's email).
- It writes through the same contracts as the staff page and agents (`core.customer_access.invitation.create`, `.invitation.revoke`, `.account.revoke`, `.account.restore`); there is no second code path.

## Second sign-in step

- A tenant turns on the step-up operation `core.user.portal_sign_in` (default off, on its 2FA requirements page) to ask customers for an authenticator code after the password. The portal sign-in then sends the customer to `/portal/sign-in/{site}/challenge` (the host's page; the pending sign-in is kept in the `bilibi_portal_sign_in` cookie, `Path=/portal/sign-in`), where they start the challenge and enter the code, or cancel. Core User proves the code and admits the session as for staff (`portal_pending_sign_in`, `portal_start_sign_in_challenge`, `portal_complete_sign_in`).
- A customer sets up the authenticator app on `/portal/account/two-factor` (linked from My account): the staff page's stages (password, QR code and key, first code, move to a new phone) drawn without the settings shell (`bilibi_user::PortalTwoFactor`). A customer who never set one up cannot finish a sign-in while the tenant asks for it; staff reset the factor as for staff.

## Customer accounts

- The staff page lists the tenant's customer accounts (`bilibi_user::customer_accounts`, the one read of `users` that lists customers; staff reads never do).
- **Revoke access** suspends the account through Core User (`set_customer_active_in`): it cannot sign in, fails its standing, and every session it holds ends at once. **Restore** lets it sign in again; no session comes back.

## Agent operations

| Key | Kind | Capability |
| --- | --- | --- |
| `core.customer_access.site.get` | read | view |
| `core.customer_access.site.set` | safe write | manage |
| `core.customer_access.invitation.list` | read | view |
| `core.customer_access.invitation.create` | safe write | manage |
| `core.customer_access.invitation.revoke` | safe write | manage |
| `core.customer_access.account.list` | read | view |
| `core.customer_access.account.revoke` | safe write | manage |
| `core.customer_access.account.restore` | safe write | manage |
| `core.customer_access.registration.list` | read | view |
| `core.customer_access.registration.approve` | safe write | manage |
| `core.customer_access.registration.reject` | safe write | manage |

The staff page writes through the same contracts.

## What the host does

The host (`apps/web`) serves the credential pages under `/portal` (`sign-in`, `forgot`, `reset`, `accept`, `register`, `verify`) and asks this facade which site an address names (`portal_site`, `registration_site`), whether an invitation or confirmation link is open (`open_invitation`, `verification_open`), and to redeem an invitation (`accept`), take a request (`register`) or confirm one (`verify`).

## Not yet

- Changing the password in place on `/portal/account`; it links to the emailed reset.
