# Core Customer Access

A tenant's customers sign in to its portal (`/portal/...`), never to the staff application ([ADR 16](../../../docs/architecture/decisions/16-customer-accounts-and-portal.md)). This module owns the portal site, the invitations that admit a customer and the portal's own home and account pages. Bilimbi has no counterpart.

## Contract

| Item | Value |
| --- | --- |
| Facade | `CustomerAccess`, service `core/customer_access.access` |
| Staff page | `/customer-access` (`admin.customer-access.view`; writes need `admin.customer-access.manage`) |
| Portal pages | `/portal` and `/portal/account` (`portal.account.view`) |
| Portal role | `portal_customer`, given to every customer an invitation admits |
| Setting | `customer_portal.invitation_days` (tenant, default 7) |
| Tables | `customer_portal_sites`, `customer_invitations` |

## Portal site

- One per tenant: an **address** (`acme`, lowercase letters, digits and inner hyphens) and a **title**.
- The address names the tenant before anyone signs in: customers sign in at `/portal/sign-in/{address}`. It is unique across the installation.
- The title brands the portal's anonymous cards, its layout and its emails.
- Changing the address breaks links already emailed.

## Invitations

- Staff invite a **Person**, optionally through one of its **affiliations** (a business customer, signing in for that company); without one the customer is a private individual.
- The address must not already belong to a customer of the tenant. Inviting it again revokes the open invitation first.
- The email carries a one-time link, `/portal/accept/{address}/{token}`. The token is stored hashed and never read back, not even by agents.
- Accepting creates the account through Core User (`create_customer_in`, address verified), gives it `portal_customer` through Authz (`assign_portal_role_in`) and closes the invitation, in one transaction audited as `system · customer_access`. The customer then signs in; no session is made at acceptance.
- A Person that retired, an affiliation that ended or a company that was archived since the invitation makes the link invalid.

## Agent operations

| Key | Kind | Capability |
| --- | --- | --- |
| `core.customer_access.site.get` | read | view |
| `core.customer_access.site.set` | safe write | manage |
| `core.customer_access.invitation.list` | read | view |
| `core.customer_access.invitation.create` | safe write | manage |
| `core.customer_access.invitation.revoke` | safe write | manage |

The staff page writes through the same contracts.

## What the host does

The host (`apps/web`) serves the credential pages under `/portal` (`sign-in`, `forgot`, `reset`, `accept`) and asks this facade which site an address names (`portal_site`), whether an invitation link is open (`open_invitation`) and to redeem it (`accept`).

## Not yet

- Self-registration, and suspending or reactivating a customer from the staff page.
- A second sign-in step for customers: a tenant that requires one for portal sign-in refuses customers with a message.
- Changing the password in place on `/portal/account`; it links to the emailed reset.
