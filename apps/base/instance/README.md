# Instance

Base Instance tells people and agents which Bilibi they are using: development, staging or production, plus an optional name like "SBG Staging". You build and test on development and staging, and promote what works to production, so a person must never mistake a test copy for the live system, or the live system for a test copy.

## What people see

| Where | Development | Staging | Production |
| --- | --- | --- | --- |
| Top bar of every page | Blue badge "DEVELOPMENT · name" | Amber badge "STAGING · name" | No badge |
| Sign-in, forgot and reset password | Same badge under the logo | Same badge | No badge |
| Account panel | "Bilibi: Development (name)" | "Bilibi: Staging (name)" | "Bilibi: Production (name)" |
| Agent consent (`/oauth/authorize`, `/oauth/device`) | Blue banner: "This is a development copy. Changes here do not affect production." | Amber banner: "This is a test copy. Changes here do not affect production." | Sober banner: "This is the live system. Changes here change real data." |

- **Why no production badge?** Production is the normal case. A badge on every production page would teach people to stop looking at badges. The account panel and every review still name it.
- The label is always in words, so colour is never the only cue.

## What agents get

Plan previews, commit results, MCP `initialize`, the MCP `plan` and `commit` tool descriptions and `/.well-known/bilibi` carry the same identity. Contract: [Which Bilibi a plan lands on](../agent_api/README.md#which-bilibi-a-plan-lands-on).

## Setting it

Two environment variables, read once at boot:

| Variable | Values | Unset |
| --- | --- | --- |
| `BILIBI_ENV` | `development`, `staging`, `production` | `production` |
| `BILIBI_INSTANCE_NAME` | One line, up to 60 characters, like `SBG Staging` | No name |

- A bad value refuses to boot with a message saying what is wrong.
- The name shows before sign-in (sign-in page, MCP, `/.well-known/bilibi`). Do not put anything secret in it.
- **Why not a setting?** Settings live in the database, and databases get copied. A production copy restored into staging would still call itself production. These two values belong to the deployment, not its data. `BILIBI_ENV` also switches step-up off in development, and the settings store must not be able to do that.

## Using it in code

- Get the `bilibi_instance::SERVICE` service (`Arc<Instance>`).
- `badge()` for a top bar, `banner(id, class)` for the top of a review or confirmation, `agent_json()` for an agent answer. The markup is `bilibi_ui::InstanceBadge` and `InstanceBanner` (`templates/feedback.html`).
- `environment().requires_step_up()` is what the host uses to switch step-up on.
