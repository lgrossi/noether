#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://127.0.0.1:4051}"
DB_PATH="${2:-/tmp/noether-dashboard-review/live.sqlite}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

USER_COUNT="${NOET_STRESS_USERS:-50}"
TEAM_COUNT="${NOET_STRESS_TEAMS:-6}"
COMPANY_COUNT="${NOET_STRESS_COMPANIES:-3}"
DAYS_BACK="${NOET_STRESS_DAYS_BACK:-180}"
WORKDAY_MINUTES="${NOET_STRESS_WORKDAY_MINUTES:-480}"

PROJECTS=(noether search editor pricing incident labs docs billing support studio api control-plane)
WORKFLOWS=(coding review research rollout triage support incident enablement migration)
SURFACES=(editor terminal automation)
PROVIDERS=(openai anthropic)
MODELS_OPENAI=(gpt-4.1 gpt-4.1-mini)
MODELS_ANTHROPIC=(claude-sonnet-4)

require_tool() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required tool: $1" >&2
    exit 1
  fi
}

require_tool curl
require_tool jq
require_tool sqlite3

post_json() {
  local path="$1"
  local payload="$2"
  curl -fsS -H 'content-type: application/json' -X POST -d "$payload" "${BASE_URL}${path}"
}

seeded_number() {
  local key="$1"
  local modulo="$2"
  printf '%s' "$key" | cksum | awk -v modulo="$modulo" '{print $1 % modulo}'
}

pick_from() {
  local key="$1"
  shift
  local values=("$@")
  local index
  index="$(seeded_number "$key" "${#values[@]}")"
  printf '%s' "${values[$index]}"
}

user_name() {
  printf 'user-%02d' "$1"
}

team_name() {
  printf 'team-%02d' "$1"
}

company_name() {
  printf 'company-%02d' "$1"
}

timestamp_at_slot() {
  local start_at="$1"
  local slot="$2"
  local total_slots="$3"
  local minute=$(( slot * WORKDAY_MINUTES / (total_slots + 1) ))
  date -u -d "$start_at +${minute} minutes" '+%Y-%m-%dT%H:%M:%SZ'
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
SET created_at = '$created_at', expires_at = datetime('$created_at', '+30 days')
WHERE id IN (
  SELECT reservation_id FROM usage_observations WHERE trace_id = '$trace_id'
);
SQL
}

wait_for_server() {
  local attempts=80
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
  local eval_score="${17}"

  local authorize_payload decision reservation_id outcome total_tokens
  authorize_payload="$(jq -n \
    --arg trace_id "$trace_id" \
    --arg request_id "$trace_id" \
    --arg session_id "pi-stress" \
    --arg subject "$subject" \
    --arg project "$project" \
    --arg provider "$provider" \
    --arg model "$model" \
    --argjson estimated_cost "$estimated_cost" \
    --argjson entities "$entities_json" \
    '{
      subject:$subject,
      project:$project,
      provider:$provider,
      model:$model,
      estimated_cost_usd:$estimated_cost,
      entities:$entities,
      metadata:{trace_id:$trace_id, request_id:$request_id, session_id:$session_id, stress_seed:true}
    }')"

  decision="$(post_json "/v1/authorize" "$authorize_payload")"
  outcome="$(jq -r '.outcome' <<<"$decision")"
  reservation_id="$(jq -r '.reservation.id // empty' <<<"$decision")"
  total_tokens=$((input_tokens + output_tokens))

  emit_event "$trace_id" "$created_at" "pi.agent_context" "$(jq -n \
    --arg request_id "$trace_id" \
    --arg cwd "/work/${project}" \
    '{request_id:$request_id, cwd:$cwd, selected_tools:["bash","read","edit"], skills:["diagnose","research"], source:"stress-seed"}')"

  if [[ "$outcome" != "deny" && -n "$reservation_id" ]]; then
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
          latency_ms: 900,
          stop_reason:"stop"
        },
        metadata:{
          trace_id:$trace_id,
          request_id:$request_id,
          source:"stress-seed",
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

  local total_slots=$((provider_call_count + (tool_count * 2) + turn_count + 3))
  local slot=1
  local index
  for index in $(seq 1 "$provider_call_count"); do
    emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
      "pi.provider_call.started" "$(jq -n --arg request_id "$trace_id" --arg provider "$provider" --arg model "$model" '{request_id:$request_id, provider:$provider, model:$model, source:"stress-seed"}')"
    slot=$((slot + 1))
    emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
      "pi.message_end" "$(jq -n --arg request_id "$trace_id" --arg provider "$provider" --arg model "$model" --argjson total_tokens "$total_tokens" --argjson cost "$actual_cost" '{request_id:$request_id, usage:{provider:$provider, model:$model, total_tokens:$total_tokens, cost_usd:$cost}, source:"stress-seed"}')"
    slot=$((slot + 1))
  done

  for index in $(seq 1 "$tool_count"); do
    emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
      "pi.tool_call" "$(jq -n --arg request_id "$trace_id" --arg tool "tool-$index" '{request_id:$request_id, name:$tool, source:"stress-seed"}')"
    slot=$((slot + 1))
    emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
      "tool.observed" "$(jq -n --arg request_id "$trace_id" --arg tool "tool-$index" '{request_id:$request_id, name:$tool, duration_ms:(120 + ($tool | ltrimstr("tool-") | tonumber * 37)), success:true, source:"stress-seed"}')"
    slot=$((slot + 1))
  done

  for index in $(seq 1 "$turn_count"); do
    emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
      "pi.turn_end" "$(jq -n --arg request_id "$trace_id" --argjson turn "$index" '{request_id:$request_id, turn_index:$turn, source:"stress-seed"}')"
    slot=$((slot + 1))
  done

  emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
    "eval.score" "$(jq -n --arg request_id "$trace_id" --argjson score "$eval_score" '{request_id:$request_id, label:"quality", score:$score, source:"stress-seed"}')"
  slot=$((slot + 1))
  emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
    "event.annotation" "$(jq -n --arg request_id "$trace_id" '{request_id:$request_id, note:"synthetic six-month population", source:"stress-seed"}')"
  slot=$((slot + 1))
  emit_event "$trace_id" "$(timestamp_at_slot "$created_at" "$slot" "$total_slots")" \
    "pi.agent_end" "$(jq -n --arg request_id "$trace_id" --argjson turns "$turn_count" '{request_id:$request_id, turn_count:$turns, source:"stress-seed"}')"

  backfill_trace "$trace_id" "$created_at"
}

wait_for_server

seeded_total=0
for day_offset in $(seq "$DAYS_BACK" -1 1); do
  day="$(date -u -d "${day_offset} days ago" '+%Y-%m-%d')"
  weekday="$(date -u -d "$day" '+%u')"
  if (( weekday > 5 )); then
    continue
  fi

  for user_index in $(seq 1 "$USER_COUNT"); do
    user="$(user_name "$user_index")"
    team_index=$(( ((user_index - 1) % TEAM_COUNT) + 1 ))
    company_index=$(( ((team_index - 1) % COMPANY_COUNT) + 1 ))
    project="$(pick_from "${day}|${user}|project" "${PROJECTS[@]}")"
    workflow="$(pick_from "${day}|${user}|workflow" "${WORKFLOWS[@]}")"
    surface="$(pick_from "${day}|${user}|surface" "${SURFACES[@]}")"
    provider="$(pick_from "${day}|${user}|provider" "${PROVIDERS[@]}")"
    if [[ "$provider" == "openai" ]]; then
      model="$(pick_from "${day}|${user}|model" "${MODELS_OPENAI[@]}")"
    else
      model="$(pick_from "${day}|${user}|model" "${MODELS_ANTHROPIC[@]}")"
    fi

    start_hour=$((8 + $(seeded_number "${day}|${user}|hour" 3) ))
    start_minute=$(( $(seeded_number "${day}|${user}|minute" 60) ))
    created_at="$(printf '%sT%02d:%02d:00Z' "$day" "$start_hour" "$start_minute")"
    trace_id="trace-${day//-/}-${user}-${project}"
    subject="user:${user}"
    entities_json="$(jq -nc \
      --arg user "$subject" \
      --arg team "team:$(team_name "$team_index")" \
      --arg org "org:$(company_name "$company_index")" \
      --arg project "project:${project}" \
      --arg workflow "workflow:${workflow}" \
      --arg surface "surface:${surface}" \
      '[$org, $team, $project, $user, $workflow, $surface]')"

    scale=$(( 1 + $(seeded_number "${day}|${user}|scale" 5) ))
    tool_count=$(( 2 + scale + $(seeded_number "${day}|${user}|tools" 4) ))
    turn_count=$(( 2 + $(seeded_number "${day}|${user}|turns" 4) ))
    provider_call_count=$(( 1 + $(seeded_number "${day}|${user}|calls" 3) ))
    input_tokens=$(( 14000 * scale + $(seeded_number "${day}|${user}|input" 18000) ))
    output_tokens=$(( 7000 * scale + $(seeded_number "${day}|${user}|output" 12000) ))
    cache_read_tokens=$(( $(seeded_number "${day}|${user}|cache-read" 24000) ))
    cache_write_tokens=$(( $(seeded_number "${day}|${user}|cache-write" 9000) ))

    estimated_cost="$(awk -v input_tokens="$input_tokens" -v output_tokens="$output_tokens" 'BEGIN { printf "%.2f", ((input_tokens + output_tokens) / 4200.0) }')"
    actual_cost="$(awk -v est="$estimated_cost" -v delta="$(seeded_number "${day}|${user}|delta" 30)" 'BEGIN { printf "%.2f", (est * (0.92 + (delta / 100.0))) }')"
    eval_score="$(awk -v raw="$(seeded_number "${day}|${user}|score" 45)" 'BEGIN { printf "%.2f", (0.55 + (raw / 100.0)) }')"

    if [[ "$project" == "labs" || "$workflow" == "incident" ]]; then
      tool_count=$((tool_count + 3))
      provider_call_count=$((provider_call_count + 1))
      estimated_cost="$(awk -v est="$estimated_cost" 'BEGIN { printf "%.2f", est * 1.4 }')"
      actual_cost="$(awk -v act="$actual_cost" 'BEGIN { printf "%.2f", act * 1.35 }')"
    fi

    if (( $(seeded_number "${day}|${user}|deny" 17) == 0 )); then
      actual_cost="0"
      input_tokens=0
      output_tokens=0
      cache_read_tokens=0
      cache_write_tokens=0
      tool_count=0
      turn_count=0
      provider_call_count=0
      eval_score="0.00"
    fi

    seed_trace \
      "$trace_id" "$created_at" "$subject" "$project" "$entities_json" "$provider" "$model" \
      "$estimated_cost" "$actual_cost" "$input_tokens" "$output_tokens" "$cache_read_tokens" "$cache_write_tokens" \
      "$tool_count" "$turn_count" "$provider_call_count" "$eval_score"
    seeded_total=$((seeded_total + 1))
  done
done

echo "Seeded ${seeded_total} traces across the last ${DAYS_BACK} days."
echo "Dashboard: ${BASE_URL}/dashboard"
echo "DB: ${DB_PATH}"
