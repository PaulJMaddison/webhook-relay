# Extending webhook-relay

This guide is for teams forking `webhook-relay` or maintaining a long-lived branch with custom behavior.

## Design goals for extension work

- Keep HTTP/API and config behavior backward-compatible by default.
- Prefer additive changes (new optional fields, new endpoints) over breaking modifications.
- Keep branch-local customizations isolated to avoid painful merges from upstream.

## Module layout (no `mod.rs`)

The project uses modern flat Rust module files under `src/`:

- `src/main.rs`: process bootstrap (config, logging, DB, router)
- `src/api.rs`: routing, handlers, middleware, request/response models
- `src/config.rs`: environment parsing and defaults
- `src/db.rs`: pool + migration helpers
- `src/domain.rs`: shared domain models
- `src/errors.rs`: typed API errors and HTTP mapping

When adding a new module:

1. Create `src/<module>.rs` (for example `src/metrics.rs`).
2. Export it from `src/lib.rs` with `pub mod metrics;`.
3. Wire use-sites from `main.rs` or `api.rs`.

This avoids nested `mod.rs` churn and keeps merge conflicts low.

## Endpoints, routing, and middleware

- Core router construction is in `api::router(...)`.
- Admin routes are grouped and protected via `basic_auth_middleware`.
- Request correlation is applied globally via `request_id_middleware`.

Typical endpoint addition flow:

1. Add a handler function in `src/api.rs`.
2. Add route wiring in `router_with_state(...)`.
3. If endpoint is admin-only, place it in the `events_routes` group (or a similar auth-scoped group).
4. Add unit/integration-style tests in `src/api.rs` test module.

## DB migrations and schema changes

Migrations live in `migrations/` and run at startup through `db::run_migrations`.

To add a migration:

1. Create a new timestamped SQL file:
   - `migrations/YYYYMMDDHHMMSS_<description>.sql`
2. Write forward-only SQL, including indexes needed by new query patterns.
3. Run tests against a local database.
4. Keep API compatibility in mind (default values for new columns, nullable columns for staged rollout).

Guidelines:

- Never rewrite old migration files once merged.
- Add indexes in the same release when introducing new filters/orderings.

## Adding a new replay policy or signature scheme

### Replay policy

Replay behavior is implemented in the replay handler logic in `src/api.rs`.

Extension approach:

- Introduce a policy enum/config struct (for example fixed retry vs exponential backoff).
- Keep current defaults unchanged.
- Add tests for:
  - success on first try
  - retriable failure then success
  - exhausted retries -> terminal status
  - non-retriable 4xx path

### Signature scheme

Current verification is HMAC-SHA256 via `X-Signature`.

To add a new scheme safely:

1. Add scheme-specific parser/validator helpers.
2. Select scheme by source configuration (new optional source field).
3. Keep existing HMAC behavior as default for existing configurations.
4. Add compatibility tests for old + new schemes.

## Add a new admin endpoint and tests

Checklist:

1. Add request/response structs in `src/api.rs`.
2. Add route under admin-auth middleware.
3. Reuse `AppError` for typed failures.
4. Add tests that verify:
   - endpoint rejects missing/invalid basic auth
   - endpoint works with valid auth
   - endpoint handles happy path + failure path

## Backward compatibility guidelines

### API compatibility

- Avoid changing existing request/response field names.
- Add fields as optional when possible.
- Do not change endpoint semantics silently.

### Config compatibility

- Keep existing environment variables functional.
- If replacing old config keys, keep a fallback parse path (as with `SOURCE_DESTINATIONS` fallback).
- Document deprecations in `README.md` + `CHANGELOG.md` before removal.

## Common extension recipes

### 1) Add a new filter to `GET /events`

- Add optional query field to the query struct.
- Extend filter parsing/validation.
- Update SQL with guarded predicates and required index migration.
- Add pagination tests to ensure cursor behavior remains stable.

### 2) Add a new webhook source config field

- Extend `RawSourceConfig` and `SourceConfig` in `src/config.rs`.
- Provide a sensible default to preserve old payload compatibility.
- Thread the new field through state and the handler that consumes it.

### 3) Add a new destination transport option

- Add transport enum in config (`http`, `queue`, etc.).
- Keep HTTP as default.
- Introduce a transport abstraction and route replay through it.
- Add integration tests per transport implementation.

### 4) Add a retention/pruning job

- Add a background task started from `main.rs` after startup.
- Gate job behavior with explicit env vars (disabled by default).
- Implement deletion in batches and log counts + durations.
- Add tests for retention boundary and safe no-op behavior.
