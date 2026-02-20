CREATE INDEX IF NOT EXISTS events_source_idx ON events (source);
CREATE INDEX IF NOT EXISTS events_received_at_idx ON events (received_at DESC);
CREATE INDEX IF NOT EXISTS events_status_idx ON events (status);
