# Cloud REST API contract

`pos-cloud.v1.yaml` is the source of truth for the cloud REST API
(`docs/backlog/07-contracts.md` §3). It uses OpenAPI 3.1, so component schemas are plain
JSON Schema 2020-12.

Checks in CI:

- **Lint:** Spectral (`.spectral.yaml`).
- **Schema checks without a database:** `apps/cloud-api/tests/contract.rs`. Health and version
  policy against their schemas, problem bodies, every documented GET path served, and the
  `ErrorCode` enum equal to `pos_domain::ErrorCode`.
- **Every route** (E16-T06): `apps/cloud-api/tests/contract_routes.rs`, against PostgreSQL. Real
  requests to every operation; each answer's status, content type and body must be ones the
  operation declares, and a route without a case fails the test. Every response example must
  match its schema.
- **Generated requests:** `scripts/contract-fuzz.sh` runs Schemathesis against the real
  server, with an owner's token and a register's token.
- **The register's client:** `crates/pos-sync-http` parses each example it would receive.

Adding a route means adding it here with its responses (problem+json errors included), a
case in `contract_routes.rs`, and an AT-12 case in `apps/cloud-api/tests/isolation.rs`.
