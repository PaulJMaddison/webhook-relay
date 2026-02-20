CREATE EXTENSION IF NOT EXISTS "pgcrypto";

ALTER TABLE IF EXISTS events
    RENAME COLUMN created_at TO received_at;

ALTER TABLE IF EXISTS events
    ADD COLUMN IF NOT EXISTS received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ADD COLUMN IF NOT EXISTS content_type TEXT,
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'received';

CREATE TABLE IF NOT EXISTS delivery_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id UUID NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    destination TEXT NOT NULL,
    response_status INT,
    response_headers JSONB,
    response_body BYTEA,
    error TEXT,
    duration_ms INT NOT NULL
);

CREATE INDEX IF NOT EXISTS delivery_attempts_event_id_idx ON delivery_attempts (event_id);
CREATE INDEX IF NOT EXISTS delivery_attempts_attempted_at_idx ON delivery_attempts (attempted_at DESC);
