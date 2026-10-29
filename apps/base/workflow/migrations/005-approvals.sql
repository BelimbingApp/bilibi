-- Approval evidence extends Bilimbi Workflow's durable human-action records.
CREATE TABLE base_workflow_approval_policies (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
    case_key text NOT NULL, version bigint NOT NULL, policy jsonb NOT NULL,
    digest text NOT NULL, reason text NOT NULL CHECK (length(btrim(reason))>0),
    publisher bigint NOT NULL, impersonator bigint, created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(tenant_id,company_id,case_key,version), UNIQUE(tenant_id,id)
);
CREATE TABLE base_workflow_approval_requests (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
    case_key text NOT NULL, subject_type text NOT NULL, subject_id bigint NOT NULL,
    revision text NOT NULL, policy_id bigint NOT NULL, facts jsonb NOT NULL,
    stages jsonb NOT NULL, stage_index integer NOT NULL DEFAULT 0,
    state text NOT NULL DEFAULT 'awaiting' CHECK(state IN ('awaiting','approved','rejected','recalled','superseded')),
    version bigint NOT NULL DEFAULT 1, routing_version bigint NOT NULL DEFAULT 1,
    initiator bigint NOT NULL, prior_request bigint, run_id bigint,
    submitted_at timestamptz NOT NULL DEFAULT now(), activated_at timestamptz NOT NULL DEFAULT now(),
    idempotency_key text NOT NULL, UNIQUE(tenant_id,idempotency_key), UNIQUE(tenant_id,id),
    FOREIGN KEY(tenant_id,policy_id) REFERENCES base_workflow_approval_policies(tenant_id,id),
    FOREIGN KEY(tenant_id,prior_request) REFERENCES base_workflow_approval_requests(tenant_id,id)
);
CREATE UNIQUE INDEX base_workflow_approval_active ON base_workflow_approval_requests(tenant_id,case_key,subject_id,revision) WHERE state='awaiting';
CREATE INDEX base_workflow_approval_inbox ON base_workflow_approval_requests(tenant_id,id) WHERE state='awaiting';
CREATE TABLE base_workflow_approval_participants (
    tenant_id bigint NOT NULL, request_id bigint NOT NULL, kind text NOT NULL CHECK(kind IN ('account','person')),
    identity_id bigint NOT NULL CHECK(identity_id>0), role text NOT NULL CHECK(role IN ('preparer','initiator','beneficiary')),
    PRIMARY KEY(tenant_id,request_id,kind,identity_id,role),
    FOREIGN KEY(tenant_id,request_id) REFERENCES base_workflow_approval_requests(tenant_id,id)
);
CREATE TABLE base_workflow_approval_decisions (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, request_id bigint NOT NULL,
    stage_key text NOT NULL, seat_key text NOT NULL, outcome text NOT NULL,
    reason text NOT NULL, owner_input jsonb NOT NULL, actor bigint NOT NULL, impersonator bigint,
    identities jsonb NOT NULL, delegation_id bigint, delegation_version bigint, grantor bigint,
    agent_connection bigint, trace_id text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(tenant_id,request_id,stage_key,seat_key),
    FOREIGN KEY(tenant_id,request_id) REFERENCES base_workflow_approval_requests(tenant_id,id)
);
CREATE TABLE base_workflow_approval_events (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, request_id bigint NOT NULL,
    event text NOT NULL, actor bigint NOT NULL, impersonator bigint, detail jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY(tenant_id,request_id) REFERENCES base_workflow_approval_requests(tenant_id,id)
);
CREATE TABLE base_workflow_approval_delegations (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL, case_key text NOT NULL,
    grantor bigint NOT NULL, delegate bigint NOT NULL CHECK(delegate<>grantor), issuer bigint NOT NULL,
    starts_at timestamptz NOT NULL, ends_at timestamptz NOT NULL CHECK(ends_at>starts_at),
    reason text NOT NULL CHECK(length(btrim(reason))>0), version bigint NOT NULL DEFAULT 1,
    revoked_at timestamptz, created_at timestamptz NOT NULL DEFAULT now(), UNIQUE(tenant_id,id)
);
CREATE TABLE base_workflow_approval_delegation_events (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, delegation_id bigint NOT NULL,
    event text NOT NULL, actor bigint NOT NULL, detail jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY(tenant_id,delegation_id) REFERENCES base_workflow_approval_delegations(tenant_id,id)
);
CREATE TABLE base_workflow_control_exceptions (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
    case_key text NOT NULL, subject_type text NOT NULL, subject_id bigint NOT NULL, revision text NOT NULL,
    request_id bigint, decision_id bigint, kind text NOT NULL, control_reference text NOT NULL,
    reason text NOT NULL CHECK(length(btrim(reason))>0), participants jsonb NOT NULL,
    actor bigint NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), UNIQUE(tenant_id,id),
    FOREIGN KEY(tenant_id,request_id) REFERENCES base_workflow_approval_requests(tenant_id,id),
    UNIQUE(tenant_id,decision_id,kind)
);
CREATE TABLE base_workflow_control_reviews (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, exception_id bigint NOT NULL,
    reviewer bigint NOT NULL, impersonator bigint, identities jsonb NOT NULL, note text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), UNIQUE(tenant_id,exception_id),
    FOREIGN KEY(tenant_id,exception_id) REFERENCES base_workflow_control_exceptions(tenant_id,id)
);
CREATE TABLE base_workflow_approval_notices (
    id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, request_id bigint NOT NULL,
    stage_key text NOT NULL, seat_key text NOT NULL, routing_version bigint NOT NULL,
    notice_key text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(tenant_id,request_id,stage_key,seat_key,routing_version,notice_key),
    FOREIGN KEY(tenant_id,request_id) REFERENCES base_workflow_approval_requests(tenant_id,id)
);
CREATE FUNCTION base_workflow_approval_evidence_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'Workflow approval evidence is append-only'; END; $$;
DO $$ DECLARE tab text; BEGIN
    FOREACH tab IN ARRAY ARRAY['base_workflow_approval_policies','base_workflow_approval_participants','base_workflow_approval_decisions','base_workflow_approval_events','base_workflow_approval_delegation_events','base_workflow_control_exceptions','base_workflow_control_reviews','base_workflow_approval_notices'] LOOP
        EXECUTE format('CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE OR TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION base_workflow_approval_evidence_guard()',tab);
    END LOOP;
END; $$;
