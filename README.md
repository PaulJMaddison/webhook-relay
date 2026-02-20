# webhook-relay

`webhook-relay` is a Rust webhook ingestion and replay gateway.

## Features (MVP)

- `POST /hooks/{source}`: ingest webhook requests.
- `GET /events`: list stored events.
- `GET /events/{id}`: fetch event details.
- `POST /events/{id}/replay`: replay a stored event to a downstream URL.

Data persisted per event:

- source
- HTTP method/path/query
- headers
- raw body bytes (`BYTEA`) exactly as received
- creation timestamp

## Quickstart (Docker)

```bash
docker compose up --build
```

API will be available at `http://localhost:8080`.

### Example flow

Ingest:

```bash
curl -i -X POST "http://localhost:8080/hooks/stripe?attempt=1" \
  -H "content-type: application/json" \
  -d '{"type":"invoice.paid"}'
```

List events:

```bash
curl -s http://localhost:8080/events | jq .
```

Get one event:

```bash
curl -s http://localhost:8080/events/<event-id> | jq .
```

Replay:

```bash
curl -s -X POST http://localhost:8080/events/<event-id>/replay \
  -H "content-type: application/json" \
  -d '{"target_url":"http://host.docker.internal:3000"}' | jq .
```

## Local dev (without Docker)

1. Run PostgreSQL and create a database.
2. Set env vars:

```bash
export DATABASE_URL=postgres://webhook:webhook@localhost:5432/webhook_relay
export BIND_ADDR=0.0.0.0:8080
export SOURCE_SECRETS={"stripe":"topsecret"}
```

3. Start service:

```bash
cargo run
```

Migrations are applied automatically at startup.

## Testing

```bash
cargo test
```

Tests cover core ingestion/get/replay endpoint behavior with an in-memory store.

When `SOURCE_SECRETS` is configured for a source, `/hooks/{source}` requires an `X-Signature: sha256=<hex>` header computed as HMAC-SHA256 over the raw request body.
