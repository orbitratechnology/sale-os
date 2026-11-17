#!/usr/bin/env bash
# E16-T06: property-based contract tests of the cloud API with Schemathesis.
#
# Starts the real `cloud-api` against $TEST_DATABASE_URL, creates an owner, a business and a
# linked register, then lets Schemathesis generate valid and invalid requests for every
# operation in contracts/openapi/pos-cloud.v1.yaml and check each answer: no 5xx, a status,
# content type and body the contract declares, invalid input refused.
#
# Owner routes run with the owner's token, register routes with the register's token.
#
# Usage: TEST_DATABASE_URL=postgres://... scripts/contract-fuzz.sh [extra schemathesis args]
# Needs: curl, jq, and `st` (schemathesis 4.x) on PATH.
set -euo pipefail

: "${TEST_DATABASE_URL:?set TEST_DATABASE_URL}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPEC="$ROOT/contracts/openapi/pos-cloud.v1.yaml"
PORT="${CONTRACT_PORT:-18080}"
BASE="http://127.0.0.1:$PORT"
OUT="${CONTRACT_REPORT_DIR:-$ROOT/target/contract-fuzz}"
mkdir -p "$OUT"

cargo build -q -p cloud-api --locked
BIN="$ROOT/target/debug/cloud-api"

export DATABASE_URL="$TEST_DATABASE_URL"
export POS_BIND_ADDR="127.0.0.1:$PORT"
# A throwaway key for this run only.
POS_JWT_SIGNING_KEY="$(head -c 32 /dev/urandom | base64)"
export POS_JWT_SIGNING_KEY
export POS_ARGON2_MEMORY_KIB=1024 POS_ARGON2_PASSES=1

"$BIN" migrate
"$BIN" serve >"$OUT/server.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do
  curl -fsS "$BASE/v1/health" >/dev/null 2>&1 && break
  sleep 0.2
done
curl -fsS "$BASE/v1/health" >/dev/null

uuid() { python3 -c 'import uuid; print(uuid.uuid4())'; }
post() { # path token json
  curl -fsS -X POST "$BASE$1" -H 'content-type: application/json' ${2:+-H "authorization: Bearer $2"} -d "$3"
}

EMAIL="fuzz-$(uuid)@example.com"
CREDS="{\"email\":\"$EMAIL\",\"password\":\"correct horse battery staple\"}"
post /v1/auth/signup "" "$CREDS" >/dev/null
USER_TOKEN="$(post /v1/auth/login "" "$CREDS" | jq -r .userAccessToken)"
BUSINESS="$(uuid)"
post /v1/businesses "$USER_TOKEN" "{\"businessId\":\"$BUSINESS\",\"name\":\"Fuzz shop\",\"countryCode\":\"GB\",
  \"timezone\":\"Europe/London\",\"currencyCode\":\"GBP\",\"currencyExponent\":2,\"pricesIncludeTax\":true}" >/dev/null
DEVICE_TOKEN="$(post /v1/devices/register "$USER_TOKEN" "{\"businessId\":\"$BUSINESS\",\"registerId\":\"$(uuid)\",
  \"registerName\":\"Till 1\",\"registerCode\":\"R01\",\"locationId\":\"$(uuid)\",\"locationName\":\"Main\",
  \"installationId\":\"$(uuid)\",\"appVersion\":\"0.1.0\",\"osInfo\":null}" | jq -r .accessToken)"

# Checks: every built-in one except
# - positive_data_acceptance: valid-by-schema data can still be refused by a rule the
#   schema cannot express (another account's businessId is 404; a SKU already taken is 409;
#   `from` after `to` is 422). Those answers are declared, so the conformance checks still
#   cover them.
# - use_after_free / ensure_resource_availability: stateful checks for REST resources with
#   DELETE/GET-by-id pairs, which this API does not have.
CHECKS=(--checks all --exclude-checks positive_data_acceptance,use_after_free,ensure_resource_availability)
COMMON=(--url "$BASE" "${CHECKS[@]}" --max-examples "${CONTRACT_EXAMPLES:-50}" --generation-deterministic
        --workers 1 --report junit --report-dir "$OUT" "$@")

status=0
# The register's routes (sync, and its own reports).
st run "$SPEC" "${COMMON[@]}" -H "Authorization: Bearer $DEVICE_TOKEN" \
  --include-path-regex '^/v1/(sync/|reports/)' --report-junit-path "$OUT/device.xml" || status=$?
# The owner's routes. Revoke is left out: it would cut off the register's token mid-run, and
# the Rust contract test covers it (apps/cloud-api/tests/contract_routes.rs).
st run "$SPEC" "${COMMON[@]}" -H "Authorization: Bearer $USER_TOKEN" \
  --exclude-path-regex '^/v1/(sync/|devices/\{deviceId\}/revoke)' --report-junit-path "$OUT/owner.xml" || status=$?
exit $status
