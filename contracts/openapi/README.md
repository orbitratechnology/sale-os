# Cloud REST API contract

`pos-cloud.v1.yaml` is the source of truth for the cloud REST API
(`docs/backlog/07-contracts.md` §3). It uses OpenAPI 3.1, so component schemas are plain
JSON Schema 2020-12.

Two checks run in CI:

- **Lint:** Spectral (`.spectral.yaml`).
- **Contract test:** `apps/cloud-api/tests/contract.rs`. It validates real responses against
  the schemas, checks that every documented path is served, and checks that the
  `ErrorCode` enum equals `pos_domain::ErrorCode`.

E16-T06 completes the spec as routes land.
