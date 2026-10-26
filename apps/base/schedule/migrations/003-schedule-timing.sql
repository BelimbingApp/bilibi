-- Extends Bilimbi Schedule definition reviews with operator clock settings. A reset keeps a revision
-- tombstone so a queued job from before a time change cannot become current again.
CREATE TABLE base_schedule_timings (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key varchar(255) NOT NULL UNIQUE CHECK (key ~ '^[a-z0-9][a-z0-9._/-]{0,254}$'),
    definition_fingerprint char(64) NOT NULL CHECK (definition_fingerprint ~ '^[0-9a-f]{64}$'),
    expression varchar(64),
    timezone varchar(255),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    changed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((expression IS NULL) = (timezone IS NULL))
);
