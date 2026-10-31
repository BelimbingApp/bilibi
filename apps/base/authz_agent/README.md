# Base Authz Agent

Reviewed operations for Authz grant writes. The Users page and agents both call `base.authz_agent.principal_role.set` for role assignment and removal; other grant pages use the `Grants` facade.

- Authz sits below Agent API, so these adapters live in a module above both, as Audit Views sits above Audit.
- No Bilimbi counterpart.

| Operation | Capability | `Grants` call | Step-up operation(s) |
|---|---|---|---|
| `base.authz_agent.role.list` (read) | `admin.authz.role.list` | `roles::list` | — |
| `base.authz_agent.principal_role.set` | `admin.authz.role.update` | `assign_role_in` | `base.authz.principal_role.set`, plus `base.authz.grant_all_role.assign` when giving a full-access role |
| `base.authz_agent.grant_all_role.assign` | `admin.authz.role.update` | `assign_role_in` (full-access roles only) | `base.authz.principal_role.set`, `base.authz.grant_all_role.assign` |
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
