# webhook-relay

`webhook-relay` is a production-oriented Rust service for capturing incoming webhooks, storing the original request, inspecting events through authenticated admin endpoints, and replaying events to preconfigured destinations for debugging, recovery, and downstream system backfills.

> Package: `webhook-relay` (library crate: `webhook_relay`)  
> Binary: `webhook-relay` (many teams use the shorthand alias `whrelay` locally)

## Features

- Source-based ingest endpoint: `POST /hooks/:source`
- Raw payload + metadata persistence in PostgreSQL
- Optional per-source HMAC-SHA256 signature verification (`X-Signature`)
- Admin event listing with filters + cursor pagination
- Event inspection endpoint by id
- Replay endpoint with configurable header forwarding and retry policy
- Replay status tracking (`received`, `delivered`, `failed`, `dead`) and delivery attempt records
- Request ID propagation (`X-Request-Id`) for traceability
- Automatic SQL migrations at startup

## Architecture overview

At a high level:

1. **Ingest**: source system sends a webhook to `POST /hooks/:source`.
2. **Store**: the raw request body and key metadata are written to PostgreSQL.
3. **Inspect**: operators use admin endpoints (`/events*`) to list and inspect stored events.
4. **Replay**: operators replay a stored event to the configured destination for that source.

## Quickstart (under 10 minutes)

### Option A: run Postgres with Docker Compose + run app locally

1. Start PostgreSQL:

```bash
docker compose up -d postgres
```

2. Export environment variables:

```bash
export DATABASE_URL='postgres://webhook:webhook@localhost:5432/webhook_relay'
export BIND_ADDR='0.0.0.0:8080'
export ADMIN_BASIC_USER='admin'
export ADMIN_BASIC_PASS='secret'
export SOURCE_CONFIGS='{"stripe":{"url":"http://127.0.0.1:3000/webhooks/stripe","timeout_ms":5000}}'
export SOURCE_SECRETS='{"stripe":"topsecret"}'
export REPLAY_FORWARD_HEADERS='content-type,user-agent,x-github-event,x-github-delivery,stripe-signature'
```

3. Run the service:

```bash
cargo run
```

4. Verify health:

```bash
curl -s http://127.0.0.1:8080/healthz | jq .
```

### Option B: run full service in Docker

```bash
docker build -t webhook-relay:local .
docker run --rm -p 8080:8080 \
  -e DATABASE_URL='postgres://webhook:webhook@host.docker.internal:5432/webhook_relay' \
  -e BIND_ADDR='0.0.0.0:8080' \
  -e ADMIN_BASIC_USER='admin' \
  -e ADMIN_BASIC_PASS='secret' \
  -e SOURCE_CONFIGS='{"stripe":{"url":"http://host.docker.internal:3000/webhooks/stripe","timeout_ms":5000}}' \
  -e SOURCE_SECRETS='{"stripe":"topsecret"}' \
  webhook-relay:local
```

## Example walkthrough

### a) Run a local receiver server

Use a tiny Python receiver that prints webhook requests:

```bash
python - <<'PY'
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get('Content-Length', '0'))
        body = self.rfile.read(length)
        print('\n--- received replay ---')
        print('path:', self.path)
        print('headers:', dict(self.headers))
        print('body:', body.decode('utf-8', errors='replace'))
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'ok')

HTTPServer(('127.0.0.1', 3000), Handler).serve_forever()
PY
```

### b) Ingest a webhook with curl

If `SOURCE_SECRETS` includes `stripe`, compute signature over the exact raw body:

```bash
body='{"type":"invoice.paid","data":{"id":"inv_123"}}'
sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac 'topsecret' -hex | sed 's/^.* //')

curl -i -X POST 'http://127.0.0.1:8080/hooks/stripe?attempt=1' \
  -H 'content-type: application/json' \
  -H "x-signature: sha256=$sig" \
  -H 'x-idempotency-key: demo-evt-1' \
  --data "$body"
```

Expected: HTTP `201 Created` with event id in JSON.

### c) List events (admin auth)

```bash
curl -s -u admin:secret 'http://127.0.0.1:8080/events?source=stripe&limit=5' | jq .
```

### d) Inspect one event

```bash
EVENT_ID='<paste-event-id>'
curl -s -u admin:secret "http://127.0.0.1:8080/events/$EVENT_ID" | jq .
```

### e) Replay to the receiver

```bash
curl -i -X POST -u admin:secret "http://127.0.0.1:8080/events/$EVENT_ID/replay?retries=2"
```

Expected:

- HTTP `200 OK`
- receiver process prints the replayed request
- event status transitions to `delivered` on success

## Configuration reference

### Required

- `DATABASE_URL`: PostgreSQL DSN.
- `BIND_ADDR`: bind address (`host:port`), e.g. `0.0.0.0:8080`.
- `ADMIN_BASIC_USER`: HTTP Basic username for admin endpoints.
- `ADMIN_BASIC_PASS`: HTTP Basic password for admin endpoints.

### Optional

- `MAX_WEBHOOK_SIZE_BYTES`: max accepted webhook body size. Default: `5242880` (5 MiB).
- `SOURCE_CONFIGS`: JSON map of source → object:
  - `url` (required): replay destination URL.
  - `timeout_ms` (optional): per-source replay timeout in ms. Default: `10000`.
  - Example:
    ```json
    {
      "stripe": { "url": "https://internal/stripe/webhook", "timeout_ms": 5000 },
      "github": { "url": "https://internal/github/webhook" }
    }
    ```
- `SOURCE_DESTINATIONS` (legacy/backward compatible): JSON map of source → destination URL. Used only when `SOURCE_CONFIGS` is unset.
- `SOURCE_SECRETS`: JSON map of source → HMAC secret for ingest signature verification.
- `REPLAY_FORWARD_HEADERS`: comma-separated lower/any-case header allowlist forwarded from original request during replay.

### Retry behavior

Replay supports `?retries=N` where `N` is the number of *extra* attempts after the first send.

- max supported `N` is `2` (hard cap of 3 total attempts)
- retries happen for network/5xx style failures
- 4xx responses are treated as non-retriable
- when retries are exhausted and delivery still fails, status becomes `dead`

## Security notes

- Admin endpoints (`/events`, `/events/:id`, `/events/:id/replay`, `/events/:id/reset`) require HTTP Basic auth.
- Signature verification is **optional per source**: only enforced when `SOURCE_SECRETS[source]` exists.
- Ingest body size is limited by `MAX_WEBHOOK_SIZE_BYTES`.
- Replay destination is server-side configuration only (reduces open relay risk).

## Operations notes

- Logging uses `tracing` JSON output by default.
- Set log level with `RUST_LOG` (for example `RUST_LOG=webhook_relay=debug`).
- Every request has an `X-Request-Id` (incoming value preserved or generated UUID).
- Startup automatically runs SQL migrations from `migrations/`; app fails fast if migrations cannot be applied.

## FAQ / Troubleshooting

- **Database connection fails at startup**
  - Verify `DATABASE_URL`, database reachability, and credentials.
  - Confirm Postgres is healthy: `docker compose ps` and `docker compose logs postgres`.
- **`sqlx` offline expectations**
  - This repository currently does not require checked-in `.sqlx` offline metadata.
  - If you enable sqlx offline mode, regenerate metadata in CI and commit it with schema changes.
- **Replay fails repeatedly**
  - Check destination URL in `SOURCE_CONFIGS`, receiver availability, and response codes.
  - Inspect recorded delivery attempts and error payloads for context.
- **Signature mismatch errors**
  - Ensure the HMAC is computed from the exact raw request body bytes.
  - Confirm header format is `X-Signature: sha256=<hex>` and source secret is correct.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local setup, test strategy, style, and PR workflow.

## Roadmap (v0.2+ ideas)

- Configurable replay backoff policies
- Optional scheduled retention/pruning worker
- Richer filtering/query ergonomics for admin APIs
- Redaction/encryption hooks for sensitive payload segments
- Optional metrics endpoint and dashboards
