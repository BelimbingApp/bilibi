-- An agent's step-up window: opened when the account holder's code arrives
-- through one agent connection (an OAuth grant), for that holder only. Not
-- shared with the holder's browser sessions or with other agents. The window
-- slides from `used_at` and is capped from `checked_at`; a revoked or expired
-- grant can no longer authenticate, so its row covers nothing, and signing
-- the holder out everywhere deletes the rows. An "ask every time" action
-- never uses the window: it needs a code each time.
CREATE TABLE base_step_up_agent_windows (
 tenant_id bigint NOT NULL,
 user_id bigint NOT NULL,
 connection_id bigint NOT NULL,
 checked_at timestamptz NOT NULL,
 used_at timestamptz NOT NULL,
 CONSTRAINT base_step_up_agent_windows_pkey PRIMARY KEY (tenant_id, user_id, connection_id)
);
CREATE INDEX base_step_up_agent_windows_checked_idx ON base_step_up_agent_windows(checked_at);
