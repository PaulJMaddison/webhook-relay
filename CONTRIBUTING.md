# Contributing to webhook-relay

Thanks for contributing! This guide helps you get productive quickly.

## Local setup

1. Start PostgreSQL:

```bash
docker compose up -d postgres
```

2. Set required environment variables:

```bash
export DATABASE_URL='postgres://webhook:webhook@localhost:5432/webhook_relay'
export BIND_ADDR='0.0.0.0:8080'
export ADMIN_BASIC_USER='admin'
export ADMIN_BASIC_PASS='secret'
```

3. Optional development defaults:

```bash
export SOURCE_CONFIGS='{"stripe":{"url":"http://127.0.0.1:3000/webhooks/stripe","timeout_ms":5000}}'
export SOURCE_SECRETS='{"stripe":"topsecret"}'
export MAX_WEBHOOK_SIZE_BYTES='5242880'
```

4. Run the app:

```bash
cargo run
```

## Quality checks (must pass before PR)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Integration testing approach

Current tests include API behavior tests and database-backed integration tests.

When adding integration tests:

- Spawn the app/router with isolated in-memory or per-test DB setup.
- Use unique identifiers per test case to avoid cross-test data collisions.
- Run migrations before DB assertions when needed.
- Assert both HTTP-level outcomes and persisted state transitions.

## Branch and PR workflow

1. Create a branch from `main`.
2. Keep commits focused and descriptive.
3. Ensure quality gates are green locally.
4. Open a PR with:
   - problem statement
   - implementation summary
   - test evidence
   - compatibility notes (API/config/schema)

## Code style expectations

- Prefer explicit, typed errors (`AppError`) with useful user-facing messages.
- Use `tracing` for operationally relevant logs.
- Keep modules in modern file layout (`src/<name>.rs`), avoid `mod.rs`.
- Preserve backward compatibility for existing API + config unless there is a bug fix justified by tests.
- Keep changes small, testable, and documented.

## Security reporting

Please **do not** open public issues for sensitive vulnerabilities.

Use one of:

- GitHub private vulnerability advisories for this repository, or
- email: `security@example.com` (placeholder; replace for your org before production release)
