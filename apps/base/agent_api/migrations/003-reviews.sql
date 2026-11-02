CREATE TABLE base_agent_api_reviews (
    id text PRIMARY KEY CHECK (id ~ '^[0-9a-f]{32}$'),
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    user_id bigint NOT NULL CHECK (user_id > 0),
    connection_id bigint NOT NULL CHECK (connection_id > 0),
    browser_session_hash bytea NOT NULL CHECK (octet_length(browser_session_hash) = 32),
    card jsonb NOT NULL,
    original_operations jsonb NOT NULL,
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open','saved','edited_and_saved','cancelled','refused','uncertain')),
    outcome jsonb,
    agent_self_report text,
    client_identity text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    ended_at timestamptz,
    CHECK ((status = 'open') = (ended_at IS NULL)),
    CHECK ((status = 'open') = (outcome IS NULL))
);
CREATE INDEX base_agent_api_reviews_owner ON base_agent_api_reviews(tenant_id,user_id,connection_id,created_at DESC);
