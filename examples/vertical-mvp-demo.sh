#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for bin in cargo curl jq; do
	if ! command -v "$bin" >/dev/null 2>&1; then
		echo "missing required command: $bin" >&2
		exit 1
	fi
done

PORT="${NOET_DEMO_PORT:-4050}"
BASE_URL="http://127.0.0.1:${PORT}"
DEMO_DIR=".noet/demo"
DB_PATH="${DEMO_DIR}/vertical-mvp.sqlite"
FIXTURE_DIR="${DEMO_DIR}/fixtures"
TRACE_ID="demo-trace-$(date +%s)"
REQUEST_ID="demo-request-1"

rm -rf "$DEMO_DIR"
mkdir -p "$FIXTURE_DIR"

cargo run --quiet --bin noet -- serve \
	--bind "127.0.0.1:${PORT}" \
	--policy examples/policy.noet.yaml \
	--decision-mode enforce \
	--db-path "$DB_PATH" \
	--fixture-dir "$FIXTURE_DIR" &
SERVER_PID=$!

cleanup() {
	kill "$SERVER_PID" >/dev/null 2>&1 || true
	wait "$SERVER_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 50); do
	if curl -fsS "${BASE_URL}/health" >/dev/null 2>&1; then
		break
	fi
	sleep 0.1
done
curl -fsS "${BASE_URL}/health" >/dev/null

echo "== authorize + reserve =="
AUTHORIZE_RESPONSE="$(
	curl -fsS "${BASE_URL}/v1/authorize" \
		-H 'content-type: application/json' \
		-d "{
			\"subject\":\"user:demo\",
			\"project\":\"noether\",
			\"provider\":\"openai-codex\",
			\"model\":\"gpt-demo\",
			\"estimated_tokens\":1200,
			\"estimated_cost_usd\":0.0024,
			\"metadata\":{
				\"trace_id\":\"${TRACE_ID}\",
				\"request_id\":\"${REQUEST_ID}\",
				\"harness\":\"pi\",
				\"extension\":\"noether-pi\",
				\"body_mode\":\"bodyless\"
			}
		}"
)"
echo "$AUTHORIZE_RESPONSE" | jq '{decision_id, outcome, reservation}'
RESERVATION_ID="$(echo "$AUTHORIZE_RESPONSE" | jq -r '.reservation.id')"

echo
echo "== finalize usage =="
curl -fsS "${BASE_URL}/v1/reservations/${RESERVATION_ID}/finalize" \
	-H 'content-type: application/json' \
	-d "{
		\"actual_cost_usd\":0.0019,
		\"usage\":{
			\"provider\":\"openai-codex\",
			\"model\":\"gpt-demo\",
			\"input_tokens\":900,
			\"output_tokens\":180,
			\"total_tokens\":1080,
			\"cost_usd\":0.0019,
			\"latency_ms\":1450,
			\"stop_reason\":\"stop\"
		},
		\"metadata\":{
			\"trace_id\":\"${TRACE_ID}\",
			\"request_id\":\"${REQUEST_ID}\",
			\"source\":\"noether-demo\"
		}
	}" | jq '{id, amount_usd, status}'

echo
echo "== ingest trace/tool/eval observations =="
curl -fsS "${BASE_URL}/v1/events" \
	-H 'content-type: application/json' \
	-d "{
		\"trace_id\":\"${TRACE_ID}\",
		\"kind\":\"request.completed\",
		\"payload\":{\"source\":\"noether-demo\",\"reservation_id\":\"${RESERVATION_ID}\",\"status\":\"ok\"}
	}" | jq .
curl -fsS "${BASE_URL}/v1/events" \
	-H 'content-type: application/json' \
	-d "{
		\"trace_id\":\"${TRACE_ID}\",
		\"kind\":\"tool.observed\",
		\"payload\":{\"name\":\"shell\",\"duration_ms\":42,\"success\":true,\"metadata\":{\"command\":\"cargo test --quiet\"}}
	}" | jq .
curl -fsS "${BASE_URL}/v1/events" \
	-H 'content-type: application/json' \
	-d "{
		\"trace_id\":\"${TRACE_ID}\",
		\"kind\":\"eval.annotation\",
		\"payload\":{\"label\":\"demo_passed\",\"score\":1.0,\"annotator\":\"noether-demo\",\"metadata\":{\"note\":\"vertical MVP story complete\"}}
	}" | jq .

echo
echo "== reports =="
echo
echo "-- usage --"
cargo run --quiet --bin noet -- report --db-path "$DB_PATH" usage
echo
echo "-- decisions --"
cargo run --quiet --bin noet -- report --db-path "$DB_PATH" decisions
echo
echo "-- trace ${TRACE_ID} --"
cargo run --quiet --bin noet -- report --db-path "$DB_PATH" trace "$TRACE_ID"
echo
echo "-- observations --"
cargo run --quiet --bin noet -- report --db-path "$DB_PATH" observations

echo
echo "demo db: ${DB_PATH}"
