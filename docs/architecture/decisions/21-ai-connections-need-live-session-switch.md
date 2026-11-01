# AI connections need a live browser-session switch

**Document Type:** Architecture decision record
**Status:** Accepted; implemented
**Agents:** codex/gpt-daybreak-blue-medium
**Scope:** OAuth agent connections, browser sessions and the AI status control
**Last Updated:** 2026-11-01

OAuth approval establishes an AI connection once and does not expire. Approval alone does not let the AI act.

Each connection has its own on/off switch. Switching it on records the exact signed-in browser session that did so. The connection works only while that session remains live, even when the same account has another live session. Any signed-in session for the account may switch a connection on or off. Switching it on from a new session transfers the connection to that session.

Every AI request checks the connection, account, permissions and the recorded session. The session check reads the idle and absolute limits without recording browser activity. Signing out, session expiry and account credential resets therefore switch access off without a cleanup job. A job already accepted continues under the normal background-job rules.

The status-bar AI tag is grey when every connection is off and green when at least one is on. Its panel and the AI connections page show one switch per connection. Changes use the existing live connection-update channel, so the account's other open pages update too.

This chooses security over the convenience of a connection that stays active away from the browser. We rejected a fixed connection expiry because it leaves unattended access until an arbitrary date. We also rejected a device cookie because pairing a browser and AI client is harder to explain and cannot prove that the person still has a live signed-in session.

The operating contract is in [Core Agent Access](../../../apps/core/agent_access/README.md).
