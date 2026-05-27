#!/usr/bin/env node
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_NOETHER_URL = "http://127.0.0.1:4051";
const DEFAULT_TIMEOUT_MS = 1000;
const SOURCE = "noether-claude-code";

export async function handleHook(input, env = process.env) {
	const config = claudeCodeConfig(input, env);
	const hook = stringValue(input.hook_event_name);
	if (hook === "PreToolUse" || hook === "PermissionRequest") {
		return authorizeToolHook(config, input, hook);
	}
	if (hook === "PostToolUse") {
		await observeToolResult(config, input, "success");
		return undefined;
	}
	if (hook === "PostToolUseFailure") {
		await observeToolResult(config, input, "failure");
		return undefined;
	}
	await postEvent(config, buildTraceEvent(input, `claude_code.${hook || "event"}`, {
		event: summarizeValue(input, config.includeBody),
	}));
	return undefined;
}

export function claudeCodeConfig(input = {}, env = process.env) {
	const cwd = stringValue(input.cwd) || process.cwd();
	return {
		noetherUrl: stripTrailingSlash(env.NOET_CC_URL || DEFAULT_NOETHER_URL),
		failMode: normalizeFailMode(env.NOET_CC_FAIL_MODE) || "fail_open",
		timeoutMs: positiveInteger(env.NOET_CC_TIMEOUT_MS) || DEFAULT_TIMEOUT_MS,
		project: emptyToUndefined(env.NOET_CC_PROJECT) || basename(cwd),
		subject: emptyToUndefined(env.NOET_CC_SUBJECT),
		stateDir: emptyToUndefined(env.NOET_CC_STATE_DIR) || join(cwd, ".noether", "claude-code"),
		includeBody: env.NOET_CC_INCLUDE_BODY === "1" || env.NOET_CC_INCLUDE_BODY === "true",
	};
}

export function buildAuthorizeRequest(config, input) {
	const toolName = stringValue(input.tool_name);
	const toolInput = record(input.tool_input);
	return dropUndefined({
		entities: unique([
			config.project ? `project:${config.project}` : undefined,
			config.subject ? normalizeSubject(config.subject) : undefined,
			toolName ? `tool:${toolName}` : undefined,
		]),
		subject: config.subject,
		project: config.project,
		provider: "claude-code",
		model: stringValue(toolInput.model),
		metadata: dropUndefined({
			harness: "claude-code",
			integration: SOURCE,
			hook_event_name: input.hook_event_name,
			session_id: input.session_id,
			tool_name: toolName,
			tool_use_id: input.tool_use_id,
			cwd: input.cwd,
			permission_mode: input.permission_mode,
			transcript_path_present: typeof input.transcript_path === "string",
			tool_input_summary: summarizeValue(toolInput, config.includeBody),
		}),
	});
}

export function denyOutput(hook, decision) {
	const reason = decisionReason(decision);
	if (hook === "PermissionRequest") {
		return {
			hookSpecificOutput: {
				hookEventName: "PermissionRequest",
				decision: {
					behavior: "deny",
					message: reason,
					interrupt: false,
				},
			},
		};
	}
	return {
		hookSpecificOutput: {
			hookEventName: "PreToolUse",
			permissionDecision: "deny",
			permissionDecisionReason: reason,
		},
	};
}

export function extractAgentUsage(input) {
	if (input.tool_name !== "Agent") {
		return undefined;
	}
	const response = record(input.tool_response);
	const usage = record(response.usage);
	const totalTokens = integerValue(response.totalTokens);
	if (!totalTokens && Object.keys(usage).length === 0) {
		return undefined;
	}
	return dropUndefined({
		provider: "claude-code",
		model: stringValue(record(input.tool_input).model),
		input_tokens: integerValue(usage.input_tokens),
		output_tokens: integerValue(usage.output_tokens),
		total_tokens: totalTokens,
		latency_ms: integerValue(response.totalDurationMs),
		stop_reason: stringValue(response.status),
	});
}

export function summarizeValue(value, includeBody = false, depth = 0) {
	if (value === null || value === undefined) {
		return value;
	}
	if (typeof value === "string") {
		return includeBody ? value : { type: "string", length: value.length };
	}
	if (typeof value === "number" || typeof value === "boolean") {
		return value;
	}
	if (Array.isArray(value)) {
		return {
			type: "array",
			length: value.length,
			item_types: histogram(value.map((item) => itemType(item))),
		};
	}
	if (typeof value !== "object") {
		return { type: typeof value };
	}
	if (depth >= 4) {
		return { type: "object", keys: Object.keys(value).sort() };
	}
	const output = {};
	for (const [key, item] of Object.entries(value)) {
		output[key] = isSensitiveKey(key)
			? summarizeSensitive(item, includeBody)
			: summarizeValue(item, includeBody, depth + 1);
	}
	return output;
}

async function authorizeToolHook(config, input, hook) {
	let decision;
	try {
		decision = await authorize(config, buildAuthorizeRequest(config, input));
	} catch (error) {
		decision = syntheticDecision(config.failMode, error);
	}
	if (decision.reservation?.id) {
		await writeReservation(config, input, decision);
	}
	await postEvent(config, buildTraceEvent(input, "claude_code.tool_authorize", {
		decision_id: decision.decision_id,
		reservation_id: decision.reservation?.id,
		outcome: decision.outcome,
		action: decision.action,
		explanations: summarizeExplanations(decision),
	}));
	if (decision.outcome === "deny") {
		return denyOutput(hook, decision);
	}
	return undefined;
}

async function observeToolResult(config, input, outcome) {
	const stored = await readReservation(config, input);
	const usage = extractAgentUsage(input);
	await postEvent(config, buildTraceEvent(input, `claude_code.tool_${outcome}`, {
		decision_id: stored?.decision_id,
		reservation_id: stored?.reservation_id,
		tool_response: summarizeValue(input.tool_response || input.error, config.includeBody),
		usage,
	}));
	if (stored?.reservation_id) {
		await finalizeReservation(config, stored.reservation_id, input, outcome, usage);
		await removeReservation(config, input);
	}
}

async function authorize(config, request) {
	const response = await fetchWithTimeout(config, `${config.noetherUrl}/v1/authorize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify(request),
	});
	if (!response.ok) {
		throw new Error(`Noether authorize returned ${response.status}`);
	}
	return response.json();
}

async function finalizeReservation(config, reservationId, input, outcome, usage) {
	await fetchWithTimeout(config, `${config.noetherUrl}/v1/reservations/${encodeURIComponent(reservationId)}/finalize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({
			reservation_id: reservationId,
			outcome: outcome === "success" ? "success" : "failure",
			actual_cost_usd: 0,
			usage,
			metadata: dropUndefined({
				source: SOURCE,
				outcome,
				session_id: input.session_id,
				tool_name: input.tool_name,
				tool_use_id: input.tool_use_id,
			}),
		}),
	});
}

async function postEvent(config, event) {
	try {
		const response = await fetchWithTimeout(config, `${config.noetherUrl}/v1/events`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(dropUndefined(event)),
		});
		if (!response.ok) {
			throw new Error(`Noether event returned ${response.status}`);
		}
	} catch (error) {
		// Event delivery is best effort; authorization/finalization decisions are handled separately.
	}
}

async function fetchWithTimeout(config, url, init) {
	const controller = new AbortController();
	const timeout = setTimeout(() => controller.abort(new Error(`Noether timed out after ${config.timeoutMs}ms`)), config.timeoutMs);
	try {
		return await fetch(url, { ...init, signal: controller.signal });
	} finally {
		clearTimeout(timeout);
	}
}

function buildTraceEvent(input, kind, payload) {
	return {
		trace_id: input.session_id,
		kind,
		payload: dropUndefined({
			source: SOURCE,
			harness: "claude-code",
			session_id: input.session_id,
			tool_name: input.tool_name,
			tool_use_id: input.tool_use_id,
			...payload,
		}),
	};
}

async function writeReservation(config, input, decision) {
	await mkdir(config.stateDir, { recursive: true });
	await writeFile(reservationPath(config, input), JSON.stringify({
		decision_id: decision.decision_id,
		reservation_id: decision.reservation.id,
	}), "utf8");
}

async function readReservation(config, input) {
	try {
		return JSON.parse(await readFile(reservationPath(config, input), "utf8"));
	} catch {
		return undefined;
	}
}

async function removeReservation(config, input) {
	await rm(reservationPath(config, input), { force: true });
}

function reservationPath(config, input) {
	const key = `${input.session_id || "session"}-${input.tool_use_id || input.tool_name || "tool"}`.replace(/[^A-Za-z0-9_.-]/g, "_");
	return join(config.stateDir, `${key}.json`);
}

function syntheticDecision(failMode, error) {
	const allow = failMode === "fail_open";
	return {
		decision_id: `cc-${failMode}`,
		outcome: allow ? "allow" : "deny",
		action: allow ? "allow" : "block",
		explanations: [{
			rule_id: "integration.sidecar_unavailable",
			reason: `Noether unavailable; applying ${failMode}: ${error instanceof Error ? error.message : String(error)}`,
			severity: allow ? "warn" : "deny",
		}],
		created_at: new Date().toISOString(),
	};
}

function decisionReason(decision) {
	const explanations = Array.isArray(decision.explanations) ? decision.explanations : [];
	return explanations
		.map((explanation) => explanation?.reason)
		.filter(Boolean)
		.join("; ") || "Noether denied this Claude Code action";
}

function summarizeExplanations(decision) {
	return Array.isArray(decision.explanations)
		? decision.explanations.map((explanation) => ({
				rule_id: explanation?.rule_id,
				reason: explanation?.reason,
				severity: explanation?.severity,
			}))
		: undefined;
}

function stripTrailingSlash(value) {
	return value.endsWith("/") ? value.slice(0, -1) : value;
}

function emptyToUndefined(value) {
	return value && value.trim() ? value : undefined;
}

function normalizeFailMode(value) {
	return value === "fail_closed" || value === "fail_open" ? value : undefined;
}

function positiveInteger(value) {
	if (!value) {
		return undefined;
	}
	const parsed = Number.parseInt(value, 10);
	return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined;
}

function stringValue(value) {
	return typeof value === "string" && value ? value : undefined;
}

function integerValue(value) {
	return typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : undefined;
}

function record(value) {
	return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function normalizeSubject(value) {
	return value.includes(":") ? value : `user:${value}`;
}

function unique(values) {
	const output = [];
	for (const value of values) {
		if (value && !output.includes(value)) {
			output.push(value);
		}
	}
	return output.length > 0 ? output : undefined;
}

function summarizeSensitive(value, includeBody) {
	if (includeBody) {
		return value;
	}
	if (typeof value === "string") {
		return { type: "string", length: value.length };
	}
	if (Array.isArray(value)) {
		return { type: "array", length: value.length, item_types: histogram(value.map((item) => itemType(item))) };
	}
	if (value && typeof value === "object") {
		return { type: "object", keys: Object.keys(value).sort() };
	}
	return value;
}

function itemType(value) {
	if (value && typeof value === "object" && !Array.isArray(value)) {
		return stringValue(value.type) || stringValue(value.role) || "object";
	}
	if (Array.isArray(value)) {
		return "array";
	}
	return typeof value;
}

function histogram(values) {
	const counts = {};
	for (const value of values) {
		counts[value] = (counts[value] || 0) + 1;
	}
	return counts;
}

function isSensitiveKey(key) {
	return /content|message|prompt|text|body|command|args|argument|result|output|plan/i.test(key);
}

function dropUndefined(value) {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined));
}

async function readStdin() {
	let input = "";
	for await (const chunk of process.stdin) {
		input += chunk;
	}
	return input.trim() ? JSON.parse(input) : {};
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
	try {
		const output = await handleHook(await readStdin());
		if (output) {
			process.stdout.write(`${JSON.stringify(output)}\n`);
		}
	} catch (error) {
		process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
		process.exitCode = 2;
	}
}
