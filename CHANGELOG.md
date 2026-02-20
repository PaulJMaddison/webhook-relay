# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-02-20

### Added

- Initial stable release of `webhook-relay` with PostgreSQL-backed webhook capture and replay.
- Ingest endpoint (`POST /hooks/:source`) with optional per-source HMAC-SHA256 signature verification.
- Admin endpoints for listing events, inspecting event details, replaying events, and resetting dead/failed event status.
- Replay delivery attempt tracking and event lifecycle status (`received`, `delivered`, `failed`, `dead`).
- Cursor-based event listing with source/status/time filtering.
- Request ID propagation and structured tracing logs.
- Automatic SQL migrations at startup.
- Project documentation for setup, extension guidance, contribution workflow, and security reporting.
