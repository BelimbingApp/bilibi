-- An operator clock replaces the contributed expression and zone for display
-- and for the next run. No row means the contribution's own clock. The
-- fingerprint stays on the contribution: a redeploy still asks for review,
-- and saving a clock does not.
CREATE TABLE base_schedule_clocks (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key varchar(255) NOT NULL UNIQUE CHECK (key ~ '^[a-z0-9][a-z0-9._/-]{0,254}$'),
    expression varchar(64) NOT NULL CHECK (char_length(expression) BETWEEN 1 AND 64),
    timezone varchar(255) NOT NULL CHECK (char_length(timezone) BETWEEN 1 AND 255),
    updated_at timestamptz NOT NULL DEFAULT now()
);
