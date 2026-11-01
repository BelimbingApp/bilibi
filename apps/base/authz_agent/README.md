# Base Authz Agent

Reviewed operations for Authz grant writes. The Users page and agents both call `base.authz_agent.principal_role.set` for role assignment and removal; other grant pages use the `Grants` facade.

- Authz sits below Agent API, so these adapters live in a module above both, as Audit Views sits above Audit.

| Operation | Capability | `Grants` call | Step-up operation(s) |
|---|---|---|---|
| `base.authz_agent.role.list` (read) | `admin.authz.role.list` | `roles::list` | — |
| `base.authz_agent.principal_role.set` | `admin.authz.role.update` | `assign_role_with_in` | `base.authz.principal_role.set`, plus `base.authz.grant_all_role.assign` when giving a full-access role |
| `base.authz_agent.grant_all_role.assign` | `admin.authz.role.update` | `assign_role_with_in` (full-access roles only) | `base.authz.principal_role.set`, `base.authz.grant_all_role.assign` |
| `base.authz_agent.principal_capability.set` | `admin.authz.role.update` | `set_direct_in` | `base.authz.principal_capability.set` |
| `base.authz_agent.role_capability.set` | `admin.authz.role.update` | `set_role_capability_in` | `base.authz.role_capability.set` |

## An agent never exceeds its person

- The plan service authorizes each operation's capability.
- `Grants` re-proves it inside the write.
- The account page's escalation guard holds here as well, at review and again at commit:
  - a role or capability is grantable only by an actor who holds everything it grants;
  - a full-access role is grantable only by a full-access actor.
- System roles' capabilities are refused: reconciliation owns them.
- Impersonation is never agent reach (`apps/base/agent_api/README.md`).

## Two-factor

Every write is a step-up operation declared by Authz:

- A commit may answer `step_up_required`. The agent asks its person for a code and commits the same plan again with `step_up_code`.
- The review lists the step-up keys under `step_up`.
- Policy, windows and audit: `apps/base/step_up/README.md`.

## Assignment bounds

`base.authz_agent.principal_role.set` accepts an optional `assignment` object with `company_id`, inclusive UTC `valid_from`/`valid_until` dates and `purpose`. An omitted start means effective now; an omitted end means no expiry, including on engagement roles. Company scope is inside `assignment`, so it does not select the acting account's workplace. The preview names the exact assignment and its previous bounds. Commit repeats the owner's company, date, role-policy and escalation checks on the plan transaction. Omitting company targets the unscoped assignment; removing Company A's assignment leaves Company B's grant intact. A role requiring an engagement refuses missing company or purpose in preview and commit. Company-scoped roles may contain only owner-opted-in company capabilities. Full-access roles can have dates, but cannot have company scope.

Direct grants and custom-role capability changes require authority without a company bound. Holding a capability within one company cannot be used to grant it across the tenant. Grant administration never adds a desk.
