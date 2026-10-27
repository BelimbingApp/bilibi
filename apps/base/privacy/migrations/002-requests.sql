-- Data-subject requests (ADR 05): access, portability and erasure, moved
-- through the contributed Workflow flow `privacy.request`. One row per mounted
-- store records that store's answer; a request completes only when every store
-- has answered or deferred. Access exports are staged per store and deleted
-- once bundled into the expiring export artifact.
CREATE TABLE base_privacy_requests (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    kind text NOT NULL CHECK (kind IN ('access','portability','erasure')),
    person_id bigint NOT NULL CHECK (person_id > 0),
    -- The email and phone values the requester gave: [{"kind":"email","value":"…"}].
    identifiers jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(identifiers) = 'array'),
    -- The company the export and certificate are filed under.
    company_id bigint NOT NULL CHECK (company_id > 0),
    status text NOT NULL DEFAULT 'received'
        CHECK (status IN ('received','verified','approved','running','completed','refused')),
    version bigint NOT NULL DEFAULT 0 CHECK (version >= 0),
    received_by bigint NOT NULL CHECK (received_by > 0),
    received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    due_at timestamptz NOT NULL,
    verification text CHECK (length(verification) BETWEEN 1 AND 1000),
    verified_by bigint CHECK (verified_by > 0),
    verified_at timestamptz,
    approved_by bigint CHECK (approved_by > 0),
    approved_at timestamptz,
    refused_by bigint CHECK (refused_by > 0),
    refused_reason text CHECK (length(refused_reason) BETWEEN 1 AND 1000),
    refused_at timestamptz,
    completed_at timestamptz,
    export_artifact text CHECK (length(export_artifact) BETWEEN 1 AND 64),
    certificate_artifact text CHECK (length(certificate_artifact) BETWEEN 1 AND 64),
    -- The finish step (bundle or certificate) has failed this many times, and
    -- why, as a short code: shown on the request so a failure is never silent.
    finish_attempts integer NOT NULL DEFAULT 0 CHECK (finish_attempts >= 0),
    finish_error text CHECK (length(finish_error) <= 64),
    -- When an erasure of this request's person cleared this row's personal
    -- text (identifiers, identity note, refusal reason).
    erased_at timestamptz,
    -- Four eyes: whoever entered an erasure request does not approve it.
    CHECK (kind <> 'erasure' OR approved_by IS NULL OR approved_by <> received_by),
    CHECK (status IN ('received','refused') OR (verification IS NOT NULL AND verified_by IS NOT NULL)),
    CHECK (status NOT IN ('approved','running','completed') OR approved_by IS NOT NULL),
    CHECK ((status = 'refused') = (refused_at IS NOT NULL)),
    CHECK ((status = 'completed') = (completed_at IS NOT NULL))
);
CREATE INDEX base_privacy_requests_tenant ON base_privacy_requests(tenant_id, id DESC);
CREATE INDEX base_privacy_requests_open ON base_privacy_requests(tenant_id, due_at)
    WHERE status NOT IN ('completed','refused');

CREATE TABLE base_privacy_request_stores (
    id bigserial PRIMARY KEY,
    request_id bigint NOT NULL REFERENCES base_privacy_requests(id) ON DELETE RESTRICT,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    store text NOT NULL CHECK (length(store) BETWEEN 1 AND 255),
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','answered','deferred')),
    -- Counts, kept records and deferral only: never a personal value.
    result jsonb,
    -- Erasure: whether the store's after-commit step has run.
    finished boolean NOT NULL DEFAULT false,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error text CHECK (length(last_error) <= 64),
    last_attempt_at timestamptz,
    answered_at timestamptz,
    UNIQUE (request_id, store),
    CHECK ((state = 'pending') = (answered_at IS NULL)),
    CHECK (state = 'pending' OR result IS NOT NULL)
);
CREATE INDEX base_privacy_request_stores_tenant ON base_privacy_request_stores(tenant_id, request_id);

-- Staged access exports. They hold personal data, so they live only until the
-- export bundle is stored, and their audit changes name the store, not the data.
CREATE TABLE base_privacy_request_exports (
    id bigserial PRIMARY KEY,
    request_id bigint NOT NULL REFERENCES base_privacy_requests(id) ON DELETE RESTRICT,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    store text NOT NULL CHECK (length(store) BETWEEN 1 AND 255),
    records jsonb NOT NULL,
    files jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(files) = 'array'),
    -- How many erasures of the person had started running when the store read
    -- these records. An export publishes only if none has been since.
    generation bigint NOT NULL CHECK (generation >= 0),
    UNIQUE (request_id, store)
);

-- Records the operator confirmed are the person's although a store found them
-- only by an email or phone (ADR 05, no silent matching). Confirmed when the
-- request is verified; a store acts on these and on records linked to the
-- person, never on another match.
CREATE TABLE base_privacy_request_candidates (
    id bigserial PRIMARY KEY,
    request_id bigint NOT NULL REFERENCES base_privacy_requests(id) ON DELETE RESTRICT,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    store text NOT NULL CHECK (length(store) BETWEEN 1 AND 255),
    kind text NOT NULL CHECK (length(kind) BETWEEN 1 AND 255),
    record_id text NOT NULL CHECK (length(record_id) BETWEEN 1 AND 128),
    confirmed_by bigint NOT NULL CHECK (confirmed_by > 0),
    confirmed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (request_id, store, kind, record_id)
);
CREATE INDEX base_privacy_request_candidates_store ON base_privacy_request_candidates(tenant_id, request_id, store);
CREATE INDEX base_privacy_requests_person ON base_privacy_requests(tenant_id, person_id, id);
