# webhook-relay

Capture, inspect, and replay real webhooks so your team can debug failures fast and recover safely.

**What you get:**
- Stop guessing why a webhook failed—store the exact request body + metadata for later inspection.
- Replay real events into staging or recovery workflows after outages, deploys, or downstream incidents.
- Build a practical webhook audit trail your team owns in Postgres.
- Reduce “works locally, fails in prod” gaps by developing against real provider payloads.
- Keep operations predictable with authenticated admin APIs, delivery tracking, and request IDs.

**It takes ~60 seconds to start:**
```bash
docker compose up -d postgres
export DATABASE_URL='postgres://webhook:webhook@localhost:5432/webhook_relay' BIND_ADDR='0.0.0.0:8080' ADMIN_BASIC_USER='admin' ADMIN_BASIC_PASS='secret' SOURCE_CONFIGS='{"stripe":{"url":"http://127.0.0.1:3000/webhooks/stripe","timeout_ms":5000}}' SOURCE_SECRETS='{"stripe":"topsecret"}' REPLAY_FORWARD_HEADERS='content-type,user-agent,x-github-event,x-github-delivery,stripe-signature'
cargo run
```

Docs: [Extending](docs/EXTENDING.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

---

## Why webhook-relay?

If you integrate Stripe, GitHub, Slack, or similar webhook providers, this probably sounds familiar:

- A payment/provider event “definitely sent” but never hit your business logic.
- A temporary 500 during deploy caused retries, partial processing, and hard-to-reproduce states.
- A bug only appears with real provider payloads/headers—not your mocked fixture.
- Staging behaves differently from production because real traffic is messy.
- Signature verification fails and everyone debates whether payload bytes changed in transit.

`webhook-relay` exists to make those incidents diagnosable and recoverable: capture exactly what came in, inspect it later, and replay safely to known destinations.

## How it helps (real use-cases)

1. **Replay the exact event that failed in production into staging**  
   Reproduce a bug with the original payload + key headers before shipping a fix.

2. **Capture and inspect actual provider requests**  
   Verify real body bytes, delivery IDs, and event headers instead of reverse-engineering from logs.

3. **Build a dead-letter style webhook workflow**  
   Track failed deliveries, inspect attempts, and replay after downstream fixes.

4. **Develop new consumers against recorded events**  
   Use real historical events to validate parsing, idempotency, and side effects.

5. **Recover after downtime**  
   Replay stored events once your service/database is healthy again.

6. **Debug signature confusion quickly**  
   Compare provider signing expectations against captured raw payload bytes and headers.

## Feature highlights

- Capture raw webhook bytes exactly as received.
- Inspect stored events through authenticated admin APIs.
- Replay to configured destinations with safe header forwarding + replay metadata.
- Track delivery attempts and event status (`received`, `delivered`, `failed`, `dead`).
- Enforce payload size limits and optional per-source signature verification.
- Self-hosted, Postgres-backed storage and control.
- Contributor-friendly Rust layout (modern module style, no `mod.rs`).

## Architecture (one-screen view)

```text
Webhook Provider
      |
      v
POST /hooks/:source
      |
      v
webhook-relay ---------> Postgres (raw events + attempts)
      |
      +---- Admin API (/events, /events/:id, /replay)
                               |
                               v
                     Replay to Destination Service
```

## Quickstart (practical)

### 1) Start Postgres

```bash
docker compose up -d postgres
```

### 2) Run webhook-relay

```bash
export DATABASE_URL='postgres://webhook:webhook@localhost:5432/webhook_relay'
export BIND_ADDR='0.0.0.0:8080'
export ADMIN_BASIC_USER='admin'
export ADMIN_BASIC_PASS='secret'
export SOURCE_CONFIGS='{"stripe":{"url":"http://127.0.0.1:3000/webhooks/stripe","timeout_ms":5000}}'
export SOURCE_SECRETS='{"stripe":"topsecret"}'
export REPLAY_FORWARD_HEADERS='content-type,user-agent,x-github-event,x-github-delivery,stripe-signature'
cargo run
```

### 3) Run a local receiver (for replay target)

```bash
python - <<'PY'
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get('Content-Length', '0'))
        body = self.rfile.read(length)
        print("\n--- received replay ---")
        print('path:', self.path)
        print('headers:', dict(self.headers))
        print('body:', body.decode('utf-8', errors='replace'))
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'ok')

HTTPServer(('127.0.0.1', 3000), Handler).serve_forever()
PY
```

### 4) Ingest one webhook and replay it

```bash
body='{"type":"invoice.paid","data":{"id":"inv_123"}}'
sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac 'topsecret' -hex | sed 's/^.* //')

curl -i -X POST 'http://127.0.0.1:8080/hooks/stripe?attempt=1' \
  -H 'content-type: application/json' \
  -H "x-signature: sha256=$sig" \
  -H 'x-idempotency-key: demo-evt-1' \
  --data "$body"

curl -s -u admin:secret 'http://127.0.0.1:8080/events?source=stripe&limit=1' | jq .
# copy the returned id into EVENT_ID

curl -i -X POST -u admin:secret "http://127.0.0.1:8080/events/$EVENT_ID/replay?retries=2"
```

Expected receiver output includes:

```text
--- received replay ---
path: /webhooks/stripe
headers: {...}
body: {"type":"invoice.paid","data":{"id":"inv_123"}}
```

## Is this for me?

**Best fit if you need:**
- A self-hosted webhook relay/audit trail with replay control.
- Better incident response for webhook-driven systems.
- Reliable staging/debug workflows based on real events.
- Straightforward integration into existing Postgres-based infrastructure.

**Probably not a fit if you need:**
- A fully managed SaaS with no infrastructure ownership.
- A UI-first webhook operations platform out of the box.
- Turnkey multi-region HA/DR without additional engineering.

## Alternatives / positioning

- If you want a **hosted webhook operations product**, choose a managed SaaS category tool.
- If you only need a **temporary request bin**, use a lightweight request-capture/bin tool.
- If you want a **self-hosted, replayable webhook audit trail** with Postgres ownership, `webhook-relay` is the target use-case.

## Adoption and trust signals

- **Safety defaults:** admin API auth required; payload size limit via `MAX_WEBHOOK_SIZE_BYTES`; replay destinations are server-configured.
- **Observability:** request ID propagation (`X-Request-Id`) and `tracing` JSON logs.
- **Data ownership:** all events and delivery attempts stored in your Postgres.
- **Compatibility:** legacy `SOURCE_DESTINATIONS` remains supported when `SOURCE_CONFIGS` is unset.

## Configuration reference

### Required

- `DATABASE_URL`: PostgreSQL DSN.
- `BIND_ADDR`: bind address (`host:port`), for example `0.0.0.0:8080`.
- `ADMIN_BASIC_USER`: HTTP Basic username for admin endpoints.
- `ADMIN_BASIC_PASS`: HTTP Basic password for admin endpoints.

### Optional

- `MAX_WEBHOOK_SIZE_BYTES`: max accepted webhook body size (default: `5242880`, 5 MiB).
- `SOURCE_CONFIGS`: JSON map of source → object:
  - `url` (required): replay destination URL.
  - `timeout_ms` (optional): replay timeout in ms (default: `10000`).
- `SOURCE_DESTINATIONS` (legacy/backward compatible): JSON map of source → destination URL. Used only when `SOURCE_CONFIGS` is unset.
- `SOURCE_SECRETS`: JSON map of source → HMAC secret for ingest signature verification.
- `REPLAY_FORWARD_HEADERS`: comma-separated header allowlist forwarded during replay.

### Retry behavior

Replay supports `?retries=N`, where `N` is extra attempts after the first send.

- Max `N` is `2` (3 total attempts).
- Retries apply to network/5xx style failures.
- 4xx responses are non-retriable.
- After retries are exhausted, status becomes `dead`.

## Security notes

- Admin endpoints (`/events`, `/events/:id`, `/events/:id/replay`, `/events/:id/reset`) require HTTP Basic auth.
- Signature verification is optional per source and enforced only when `SOURCE_SECRETS[source]` exists.
- Replay destination URLs come from server-side config (not user-supplied request parameters).

## Extending webhook-relay

Want to adapt this for your org? Great—forks and extensions are welcome. The codebase uses a modern Rust module layout (no `mod.rs`) to keep boundaries clearer and reduce merge-conflict pain for long-lived branches. See [docs/EXTENDING.md](docs/EXTENDING.md).

Concrete extension ideas:

1. A lightweight operator UI for event search + replay workflows.
2. Queue-backed/asynchronous replay workers.
3. Multi-tenant source/admin isolation.
4. Retention policies and pruning automation.
5. Provider-specific signature schemes beyond HMAC-SHA256.
6. External blob storage (for example S3) for large payload archives.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local setup, style, testing, and PR workflow.
