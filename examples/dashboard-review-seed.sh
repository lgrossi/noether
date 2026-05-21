#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://127.0.0.1:4051}"
DB_PATH="${2:-/tmp/noether-dashboard-review/live.sqlite}"
SIMULATION_DIR="${3:-/tmp/noether-dashboard-review/simulations}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_tool() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required tool: $1" >&2
    exit 1
  fi
}

require_tool curl
require_tool jq
require_tool sqlite3
require_tool cargo

post_json() {
  local path="$1"
  local payload="$2"
  curl -fsS \
    -H 'content-type: application/json' \
    -X POST \
    -d "$payload" \
    "${BASE_URL}${path}"
}

emit_event() {
  local trace_id="$1"
  local occurred_at="$2"
  local kind="$3"
  local payload="$4"
  post_json "/v1/events" "$(jq -n \
    --arg trace_id "$trace_id" \
    --arg occurred_at "$occurred_at" \
    --arg kind "$kind" \
    --argjson payload "$payload" \
    '{trace_id:$trace_id, occurred_at:$occurred_at, kind:$kind, payload:$payload}')" >/dev/null
}

backfill_trace() {
  local trace_id="$1"
  local created_at="$2"
  sqlite3 "$DB_PATH" <<SQL
UPDATE decisions SET created_at = '$created_at' WHERE trace_id = '$trace_id';
UPDATE usage_observations SET created_at = '$created_at' WHERE trace_id = '$trace_id';
UPDATE reservations
SET created_at = '$created_at', expires_at = datetime('$created_at', '+1 hour')
WHERE id IN (
  SELECT reservation_id FROM usage_observations WHERE trace_id = '$trace_id'
);
SQL
}

seed_trace() {
  local trace_id="$1"
  local created_at="$2"
  local subject="$3"
  local project="$4"
  local entities_json="$5"
  local provider="$6"
  local model="$7"
  local estimated_cost="$8"
  local actual_cost="$9"
  local input_tokens="${10}"
  local output_tokens="${11}"
  local cache_read_tokens="${12}"
  local cache_write_tokens="${13}"
  local tool_count="${14}"
  local turn_count="${15}"
  local provider_call_count="${16}"
  local requested_budget="${17}"
  local eval_score="${18}"

  local authorize_payload
  authorize_payload="$(jq -n \
    --arg trace_id "$trace_id" \
    --arg request_id "$trace_id" \
    --arg session_id "dashboard-review" \
    --arg subject "$subject" \
    --arg project "$project" \
    --arg provider "$provider" \
    --arg model "$model" \
    --argjson estimated_cost "$estimated_cost" \
    --argjson entities "$entities_json" \
    --arg requested_budget "$requested_budget" \
    '{
      subject:$subject,
      project:$project,
      provider:$provider,
      model:$model,
      estimated_cost_usd:$estimated_cost,
      entities:$entities,
      metadata:{trace_id:$trace_id, request_id:$request_id, session_id:$session_id, review_seed:true}
    } + (if $requested_budget == "" then {} else {budget_id:$requested_budget} end)')"

  local decision reservation_id outcome
  decision="$(post_json "/v1/authorize" "$authorize_payload")"
  outcome="$(jq -r '.outcome' <<<"$decision")"
  reservation_id="$(jq -r '.reservation.id // empty' <<<"$decision")"

  emit_event "$trace_id" "$created_at" "pi.agent_context" "$(jq -n \
    --arg request_id "$trace_id" \
    --arg skill "dashboard-review" \
    '{request_id:$request_id, skill:$skill, workflow:"review", source:"seed"}')"

  if [[ "$outcome" != "deny" && -n "$reservation_id" ]]; then
    local total_tokens=$((input_tokens + output_tokens))
    local finalize_payload
    finalize_payload="$(jq -n \
      --arg trace_id "$trace_id" \
      --arg request_id "$trace_id" \
      --arg provider "$provider" \
      --arg model "$model" \
      --argjson actual_cost "$actual_cost" \
      --argjson input_tokens "$input_tokens" \
      --argjson output_tokens "$output_tokens" \
      --argjson total_tokens "$total_tokens" \
      --argjson cache_read_tokens "$cache_read_tokens" \
      --argjson cache_write_tokens "$cache_write_tokens" \
      '{
        actual_cost_usd:$actual_cost,
        usage:{
          provider:$provider,
          model:$model,
          input_tokens:$input_tokens,
          output_tokens:$output_tokens,
          total_tokens:$total_tokens,
          cost_usd:$actual_cost,
          latency_ms: 600,
          stop_reason:"stop"
        },
        metadata:{
          trace_id:$trace_id,
          request_id:$request_id,
          source:"seed",
          usage_details:{
            cache_read_tokens:$cache_read_tokens,
            cache_write_tokens:$cache_write_tokens,
            cache_read_cost_usd:(($cache_read_tokens / 1000) * 0.0001),
            cache_write_cost_usd:(($cache_write_tokens / 1000) * 0.00005)
          }
        }
      }')"
    post_json "/v1/reservations/${reservation_id}/finalize" "$finalize_payload" >/dev/null
  fi

  local minute=5
  local index
  for index in $(seq 1 "$provider_call_count"); do
    emit_event "$trace_id" "$(date -u -d "$created_at +${minute} minutes" '+%Y-%m-%dT%H:%M:%SZ')" \
      "pi.provider_call.started" "$(jq -n --arg request_id "$trace_id" --arg provider "$provider" --arg model "$model" '{request_id:$request_id, provider:$provider, model:$model, source:"seed"}')"
    minute=$((minute + 2))
  done

  for index in $(seq 1 "$tool_count"); do
    emit_event "$trace_id" "$(date -u -d "$created_at +${minute} minutes" '+%Y-%m-%dT%H:%M:%SZ')" \
      "pi.tool_call" "$(jq -n --arg request_id "$trace_id" --arg tool "tool-$index" '{request_id:$request_id, name:$tool, source:"seed"}')"
    emit_event "$trace_id" "$(date -u -d "$created_at +$((minute + 1)) minutes" '+%Y-%m-%dT%H:%M:%SZ')" \
      "tool.observed" "$(jq -n --arg request_id "$trace_id" --arg tool "tool-$index" '{request_id:$request_id, name:$tool, duration_ms:(20 + ($tool | ltrimstr("tool-") | tonumber * 15)), success:true, source:"seed"}')"
    minute=$((minute + 3))
  done

  for index in $(seq 1 "$turn_count"); do
    emit_event "$trace_id" "$(date -u -d "$created_at +${minute} minutes" '+%Y-%m-%dT%H:%M:%SZ')" \
      "pi.turn_end" "$(jq -n --arg request_id "$trace_id" '{request_id:$request_id, source:"seed"}')"
    minute=$((minute + 3))
  done

  emit_event "$trace_id" "$(date -u -d "$created_at +${minute} minutes" '+%Y-%m-%dT%H:%M:%SZ')" \
    "eval.score" "$(jq -n --arg request_id "$trace_id" --argjson score "$eval_score" '{request_id:$request_id, label:"quality", score:$score, source:"seed"}')"
  emit_event "$trace_id" "$(date -u -d "$created_at +$((minute + 2)) minutes" '+%Y-%m-%dT%H:%M:%SZ')" \
    "event.annotation" "$(jq -n --arg request_id "$trace_id" '{request_id:$request_id, note:"seeded dashboard review evidence", source:"seed"}')"

  backfill_trace "$trace_id" "$created_at"
  echo "seeded ${trace_id} (${outcome})"
}

wait_for_server() {
  local attempts=40
  while (( attempts > 0 )); do
    if curl -fsS "${BASE_URL}/health" >/dev/null 2>&1; then
      return 0
    fi
    attempts=$((attempts - 1))
    sleep 0.25
  done
  echo "server not reachable at ${BASE_URL}" >&2
  exit 1
}

seed_simulations() {
  mkdir -p "$SIMULATION_DIR"
  (
    cd "$ROOT_DIR"
    cargo run --quiet --bin noet -- simulate --out-dir "${SIMULATION_DIR}/synthetic-company" "${ROOT_DIR}/examples/simulations/synthetic-company.noet.yaml" >/dev/null
    cargo run --quiet --bin noet -- simulate --out-dir "${SIMULATION_DIR}/runaway-pressure" "${ROOT_DIR}/examples/simulations/runaway-pressure.noet.yaml" >/dev/null
    cargo run --quiet --bin noet -- simulate --out-dir "${SIMULATION_DIR}/adoption-pressure" "${ROOT_DIR}/examples/simulations/adoption-pressure.noet.yaml" >/dev/null
  )
}

wait_for_server
seed_simulations

seed_trace "trace-support-onboarding-q1" "$(date -u -d '118 days ago 09:15' '+%Y-%m-%dT%H:%M:%SZ')" "user:nina"   "knowledge" '["team:support","project:knowledge","user:nina","workflow:onboarding","surface:internal-docs","region:emea"]' "openai" "gpt-4.1-mini"  1.7  1.5  9000   6000   4000  1800  1 1 1 "support-adoption" 0.92
seed_trace "trace-revenue-close-q1"     "$(date -u -d '111 days ago 18:40' '+%Y-%m-%dT%H:%M:%SZ')" "user:jon"    "pricing"   '["team:revenue","project:pricing","user:jon","workflow:quarter-close","surface:customer-pricing","region:us"]' "openai" "gpt-4.1"      14.1 13.8 72000  36000  7000  2600  4 2 3 ""                0.83
seed_trace "trace-security-hotwash"     "$(date -u -d '104 days ago 07:10' '+%Y-%m-%dT%H:%M:%SZ')" "user:maya"   "incident"  '["team:security","project:incident","user:maya","workflow:incident","surface:prod","tier:critical"]' "anthropic" "claude-sonnet-4" 9.4 9.0  41000 21000  6000  2200  3 2 2 "security-incident" 0.79
seed_trace "trace-noether-retro"        "$(date -u -d '97 days ago 10:25' '+%Y-%m-%dT%H:%M:%SZ')" "user:alice"  "noether"   '["team:platform","project:noether","user:alice","workflow:retro","surface:internal","cohort:power-user"]' "openai" "gpt-4.1-mini" 2.6 2.3  21000  9000   8000  3000  2 1 1 ""                0.90
seed_trace "trace-support-macro-rollout" "$(date -u -d '89 days ago 11:30' '+%Y-%m-%dT%H:%M:%SZ')" "user:sam"    "knowledge" '["team:support","project:knowledge","user:sam","workflow:macro-rollout","surface:support","region:us"]' "openai" "gpt-4.1-mini" 2.9 2.7  16000  9000   9000  3800  2 1 2 "support-adoption" 0.88
seed_trace "trace-labs-agent-loop-1"    "$(date -u -d '82 days ago 16:05' '+%Y-%m-%dT%H:%M:%SZ')" "user:omar"   "labs"      '["team:platform","project:labs","user:omar","workflow:experiments","surface:sandbox","risk:loop"]' "openai" "gpt-4.1"     11.3 10.8 54000  29000  5000  1400  7 4 5 "runaway-sandbox" 0.55
seed_trace "trace-revenue-segmentation" "$(date -u -d '74 days ago 08:20' '+%Y-%m-%dT%H:%M:%SZ')" "user:iris"   "pricing"   '["team:revenue","project:pricing","user:iris","workflow:segmentation","surface:crm","region:emea"]' "anthropic" "claude-sonnet-4" 6.2 5.9  28000 17000  5000  1900  2 1 2 "revenue-pricing" 0.87
seed_trace "trace-security-playbook"    "$(date -u -d '68 days ago 13:45' '+%Y-%m-%dT%H:%M:%SZ')" "user:maya"   "incident"  '["team:security","project:incident","user:maya","workflow:playbook","surface:prod","tier:critical"]' "openai" "gpt-4.1-mini" 3.4 3.1  18000 11000  7000  2600  2 1 2 "security-incident" 0.91
seed_trace "trace-search-launch-week"   "$(date -u -d '59 days ago 09:50' '+%Y-%m-%dT%H:%M:%SZ')" "user:eva"    "search"    '["team:product","project:search","user:eva","workflow:launch","surface:customer","region:global"]' "openai" "gpt-4.1"      18.9 18.1 98000  52000  6000  2000  5 2 4 ""                0.76
seed_trace "trace-editor-adoption-coach" "$(date -u -d '52 days ago 15:10' '+%Y-%m-%dT%H:%M:%SZ')" "user:chloe"  "editor"    '["team:product","project:editor","user:chloe","workflow:enablement","surface:internal","cohort:low-adopter"]' "openai" "gpt-4.1-mini" 1.2 1.0  7000   4000   1500  600   1 1 1 "product-adoption" 0.93
seed_trace "trace-support-night-shift"  "$(date -u -d '46 days ago 22:15' '+%Y-%m-%dT%H:%M:%SZ')" "user:lena"   "knowledge" '["team:support","project:knowledge","user:lena","workflow:night-shift","surface:support","region:apac"]' "anthropic" "claude-sonnet-4" 4.8 4.5  22000 13000  4000  1500  2 2 2 ""                0.85
seed_trace "trace-noether-release-ops"  "$(date -u -d '39 days ago 14:30' '+%Y-%m-%dT%H:%M:%SZ')" "user:ben"    "noether"   '["team:platform","project:noether","user:ben","workflow:release","surface:internal","service:control-plane"]' "openai" "gpt-4.1"      12.9 12.3 61000  31000  12000 4100  4 2 3 "platform-premium" 0.82
seed_trace "trace-revenue-renewals"     "$(date -u -d '33 days ago 09:35' '+%Y-%m-%dT%H:%M:%SZ')" "user:jon"    "pricing"   '["team:revenue","project:pricing","user:jon","workflow:renewals","surface:customer","segment:enterprise"]' "openai" "gpt-4.1-mini" 2.7 2.5  15000 9000   5000  1800  2 1 1 ""                0.89
seed_trace "trace-security-triage-spike" "$(date -u -d '29 days ago 06:55' '+%Y-%m-%dT%H:%M:%SZ')" "user:maya"   "incident"  '["team:security","project:incident","user:maya","workflow:triage","surface:prod","tier:critical"]' "openai" "gpt-4.1"     17.8 17.0 83000  41000  4000  1200  5 3 4 "security-incident" 0.73
seed_trace "trace-support-qa-gap"       "$(date -u -d '25 days ago 10:45' '+%Y-%m-%dT%H:%M:%SZ')" "user:nina"   "knowledge" '["team:support","project:knowledge","user:nina","workflow:qa","surface:docs","cohort:low-adopter"]' "openai" "gpt-4.1-mini" 0.8 0.7  5000   2600   1200  400   1 1 1 "support-adoption" 0.94
seed_trace "trace-labs-runaway-denied-2" "$(date -u -d '21 days ago 19:20' '+%Y-%m-%dT%H:%M:%SZ')" "user:omar"   "labs"      '["team:platform","project:labs","user:omar","workflow:experiments","surface:sandbox","risk:loop"]' "openai" "gpt-4.1"     15.7 0    0      0      0     0     0 0 0 "runaway-sandbox" 0.00
seed_trace "trace-editor-rollout-week"  "$(date -u -d '18 days ago 11:05' '+%Y-%m-%dT%H:%M:%SZ')" "user:marta"  "editor"    '["team:product","project:editor","user:marta","workflow:rollout","surface:customer","segment:mid-market"]' "anthropic" "claude-sonnet-4" 7.2 6.8  32000 18000  6000  2200  3 2 2 "editor-claude"   0.84
seed_trace "trace-noether-api-migration" "$(date -u -d '16 days ago 09:55' '+%Y-%m-%dT%H:%M:%SZ')" "user:alice"  "noether"   '["team:platform","project:noether","user:alice","workflow:migration","surface:api","service:control-plane"]' "openai" "gpt-4.1"      20.8 19.9 108000 56000  9000  3200  5 3 4 "platform-premium" 0.78

seed_trace "trace-noether-burst-a"  "$(date -u -d '13 days ago 09:10' '+%Y-%m-%dT%H:%M:%SZ')" "user:alice" "noether" '["team:platform","project:noether","user:alice"]' "openai" "gpt-4.1"       28.5 27.9 138000 72000 22000 18000 5 3 4 ""              0.94
seed_trace "trace-search-spike"     "$(date -u -d '12 days ago 10:20' '+%Y-%m-%dT%H:%M:%SZ')" "user:eva"   "search"  '["team:product","project:search","user:eva"]'   "openai" "gpt-4.1"       22.0 21.4 128000 64000 14000 6000  4 2 3 ""              0.71
seed_trace "trace-editor-fallback"  "$(date -u -d '11 days ago 11:05' '+%Y-%m-%dT%H:%M:%SZ')" "user:chloe" "editor"  '["team:product","project:editor","user:chloe"]' "openai" "gpt-4.1"       12.8 12.4 64000  34000 9000  4200  3 2 2 "editor-claude"  0.82
seed_trace "trace-labs-loop-safe"   "$(date -u -d '10 days ago 15:15' '+%Y-%m-%dT%H:%M:%SZ')" "user:omar"  "labs"    '["team:platform","project:labs","user:omar"]'   "openai" "gpt-4.1-mini"  6.6  6.1  42000  28000 8000  3500  8 4 6 ""              0.63
seed_trace "trace-product-coach-1"  "$(date -u -d '9 days ago 09:40'  '+%Y-%m-%dT%H:%M:%SZ')" "user:marta" "editor"  '["team:product","project:editor","user:marta"]' "openai" "gpt-4.1-mini"  1.8  1.6  10000  6000  2000  1200  1 1 1 ""              0.88
seed_trace "trace-product-coach-2"  "$(date -u -d '8 days ago 13:25'  '+%Y-%m-%dT%H:%M:%SZ')" "user:ben"   "editor"  '["team:product","project:editor","user:ben"]'   "anthropic" "claude-sonnet-4" 4.2 4.1  18000 12000 3000  1400  2 1 1 ""          0.91
seed_trace "trace-search-cache"     "$(date -u -d '7 days ago 10:50'  '+%Y-%m-%dT%H:%M:%SZ')" "user:diego" "search"  '["team:product","project:search","user:diego"]' "openai" "gpt-4.1-mini"  3.1  2.9  26000 12000 16000 6000  2 1 2 ""              0.86
seed_trace "trace-noether-cache"    "$(date -u -d '6 days ago 09:05'  '+%Y-%m-%dT%H:%M:%SZ')" "user:ben"   "noether" '["team:platform","project:noether","user:ben"]' "anthropic" "claude-sonnet-4" 8.4 8.1  36000 18000 24000 11000 3 2 2 ""           0.93
seed_trace "trace-product-heavy"    "$(date -u -d '5 days ago 16:35'  '+%Y-%m-%dT%H:%M:%SZ')" "user:eva"   "editor"  '["team:product","project:editor","user:eva"]'   "openai" "gpt-4.1"       16.7 16.2 92000  41000 7000  2500  6 2 4 ""              0.68
seed_trace "trace-labs-denied"      "$(date -u -d '4 days ago 12:15'  '+%Y-%m-%dT%H:%M:%SZ')" "user:omar"  "labs"    '["team:platform","project:labs","user:omar"]'   "openai" "gpt-4.1"       18.4 0    0      0      0     0     0 0 0 ""              0.00
seed_trace "trace-search-runup"     "$(date -u -d '3 days ago 14:05'  '+%Y-%m-%dT%H:%M:%SZ')" "user:eva"   "search"  '["team:product","project:search","user:eva"]'   "openai" "gpt-4.1"       24.1 23.6 142000 76000 5000  1800  5 3 4 ""              0.74
seed_trace "trace-product-lowuse"   "$(date -u -d '2 days ago 08:30'  '+%Y-%m-%dT%H:%M:%SZ')" "user:chloe" "editor"  '["team:product","project:editor","user:chloe"]' "openai" "gpt-4.1-mini"  0.9  0.8  6000   3000   1000  400   1 1 1 ""              0.89
seed_trace "trace-noether-spend"    "$(date -u -d '1 day ago 17:20'   '+%Y-%m-%dT%H:%M:%SZ')" "user:alice" "noether" '["team:platform","project:noether","user:alice"]' "openai" "gpt-4.1"      31.2 30.6 154000 82000 12000 4800  7 3 4 ""              0.72
seed_trace "trace-search-steady"    "$(date -u -d '0 days ago 10:45'  '+%Y-%m-%dT%H:%M:%SZ')" "user:diego" "search"  '["team:product","project:search","user:diego"]' "anthropic" "claude-sonnet-4" 5.7 5.4  26000 18000 9000  3000  2 2 2 ""           0.84

echo
echo "Seed complete."
echo "Dashboard: ${BASE_URL}/dashboard"
echo "DB: ${DB_PATH}"
echo "Simulations: ${SIMULATION_DIR}"
