# webhook-relay

`webhook-relay` is a Rust service that accepts incoming webhooks, stores the raw request payload, and lets operators replay captured events to preconfigured downstream destinations.

## What it does

- Ingests webhook calls via `POST /hooks/:source`.
- Persists event metadata and raw bytes in PostgreSQL.
- Lists events with filtering/pagination via `GET /events` (admin-auth protected).
- Replays an event to a configured destination via `POST /events/:id/replay` (admin-auth protected).
- Tracks replay outcomes (`delivered` / `failed`) and delivery attempts.

## Quickstart

### 1) Start with Docker Compose

```bash
docker compose up --build
```

Service listens on `http://localhost:8080` by default.

### 2) Health check

```bash
curl -s http://localhost:8080/healthz
```

### 3) Local run (without Docker)

```bash
export DATABASE_URL='postgres://webhook:webhook@localhost:5432/webhook_relay'
export BIND_ADDR='0.0.0.0:8080'
export ADMIN_BASIC_USER='admin'
export ADMIN_BASIC_PASS='secret'
export SOURCE_CONFIGS='{"stripe":{"url":"http://host.docker.internal:3000/","timeout_ms":5000}}'
export SOURCE_SECRETS='{"stripe":"topsecret"}'
export REPLAY_FORWARD_HEADERS='content-type,user-agent,x-github-event,x-github-delivery,stripe-signature'

cargo run
```

Migrations are applied automatically on startup.

## Env vars

Required:

- `DATABASE_URL` — PostgreSQL DSN.
- `BIND_ADDR` — bind address (`host:port`).
- `ADMIN_BASIC_USER` — username for `/events*` and replay endpoints.
- `ADMIN_BASIC_PASS` — password for `/events*` and replay endpoints.

Optional (JSON maps):

- `SOURCE_CONFIGS` — map of source to replay config (`url`, optional `timeout_ms`).
- `SOURCE_DESTINATIONS` — legacy map of source to replay destination URL (used only when `SOURCE_CONFIGS` is unset; default timeout applies).
  - Example: `{"stripe":"https://example.internal/webhooks"}`
- `SOURCE_SECRETS` — map of source to HMAC secret for ingest signature validation.
  - Example: `{"stripe":"whsec_..."}`
- `REPLAY_FORWARD_HEADERS` — comma-separated header names to forward from original event on replay.
  - Default: `content-type,user-agent,x-github-event,x-github-delivery,stripe-signature`

Notes:

- If a source is missing in `SOURCE_SECRETS`, ingest for that source is accepted without signature validation.
- Replays require a destination for the event source in `SOURCE_CONFIGS` (or `SOURCE_DESTINATIONS` for legacy config).

## curl ingest example

If a secret exists for `stripe`, compute and send `X-Signature: sha256=<hex>` over the **raw body**:

```bash
body='{"type":"invoice.paid"}'
sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac 'topsecret' -hex | sed 's/^.* //')

curl -i -X POST 'http://localhost:8080/hooks/stripe?attempt=1' \
  -H 'content-type: application/json' \
  -H "X-Signature: sha256=$sig" \
  --data "$body"
```

If no secret is configured for the source, omit `X-Signature`.

## curl replay example

1) List events (admin basic auth):

```bash
curl -s -u admin:secret 'http://localhost:8080/events?source=stripe&limit=1' | jq .
```

2) Replay one event by id:

```bash
EVENT_ID='<event-uuid>'

curl -i -X POST -u admin:secret \
  "http://localhost:8080/events/$EVENT_ID/replay"
```

Replay uses the event's original method/body and forwards with:

- `X-Webhook-Replay: true`
- `X-Original-Event-Id: <event-id>`
- A safe allowlist of original headers (default: `content-type`, `user-agent`, `x-github-event`, `x-github-delivery`, `stripe-signature`, configurable via `REPLAY_FORWARD_HEADERS`).
- Hop-by-hop headers are always stripped (`connection`, `keep-alive`, `transfer-encoding`, etc.), even if allowlisted.


## Recommended database indexes

The event listing endpoint (`GET /events`) uses cursor pagination ordered by
`received_at DESC, id DESC`. The following indexes are recommended and are
already created by migrations:

- `events_received_at_id_desc_idx` on `(received_at DESC, id DESC)`
- `events_source_received_at_idx` on `(source, received_at DESC)`
- `events_status_received_at_idx` on `(status, received_at DESC)`

## Security notes

- Protect admin endpoints with strong credentials and TLS in production.
- Use per-source secrets in `SOURCE_SECRETS` to verify ingest authenticity.
- Signature validation is HMAC-SHA256 over raw bytes with `X-Signature: sha256=<hex>`.
- Replay destinations are server-side configured (not user-supplied per request), reducing open-relay risk.
- Preserve and pass `X-Request-Id` for traceability (auto-generated if absent).

## Roadmap

- Retry policies and dead-letter queues for failed replays.
- Per-source rate limits and ingest quotas.
- Event retention policies and cleanup jobs.
- Richer admin querying (status/time/source dashboards).
- Optional redaction/encryption controls for sensitive payload fields.
