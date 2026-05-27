#!/usr/bin/env node
import { spawn } from "node:child_process";
import { basename } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_NOETHER_URL = "http://127.0.0.1:4051";
const DEFAULT_TIMEOUT_MS = 1000;
const SOURCE = "noether-codex";

export async function runCodex(args, env = process.env, io = defaultIo()) {
	const config = codexConfig(args, env);
	const request = buildAuthorizeRequest(config, args);
	const decision = await safeAuthorize(config, request);
	if (decision.outcome === "deny") {
		io.stderr.write(`${decisionReason(decision)}\n`);
		return { exitCode: 3, spawned: false, decision };
	}
	const codexArgs = codexExecArgs(args);
	const result = await spawnCodex(config, codexArgs, io, decision);
	return { ...result, spawned: true, decision };
}

export function codexConfig(args = [], env = process.env, cwd = process.cwd()) {
	return {
		noetherUrl: stripTrailingSlash(env.NOET_CODEX_URL || DEFAULT_NOETHER_URL),
		failMode: normalizeFailMode(env.NOET_CODEX_FAIL_MODE) || "fail_closed",
		timeoutMs: positiveInteger(env.NOET_CODEX_TIMEOUT_MS) || DEFAULT_TIMEOUT_MS,
		project: emptyToUndefined(env.NOET_CODEX_PROJECT) || basename(cwd),
		subject: emptyToUndefined(env.NOET_CODEX_SUBJECT),
		provider: emptyToUndefined(env.NOET_CODEX_PROVIDER),
		model: modelFromArgs(args) || emptyToUndefined(env.NOET_CODEX_MODEL),
		codexBin: env.NOET_CODEX_BIN || "codex",
		cwd,
	};
}

export function buildAuthorizeRequest(config, args) {
	return dropUndefined({
		entities: unique([
			config.project ? `project:${config.project}` : undefined,
			config.subject ? normalizeSubject(config.subject) : undefined,
		]),
		subject: config.subject,
		project: config.project,
		provider: config.provider,
		model: config.model,
		metadata: dropUndefined({
			harness: "codex",
			integration: SOURCE,
			request_id: `codex-${Date.now()}`,
			cwd: config.cwd,
			codex_args: summarizeArgs(args),
			provider_known: Boolean(config.provider),
		}),
	});
}

export function codexExecArgs(args) {
	const output = [...args];
	if (output[0] !== "exec") {
		output.unshift("exec");
	}
	if (!output.includes("--json")) {
		output.splice(1, 0, "--json");
	}
	return output;
}

export function modelFromArgs(args) {
	for (let index = 0; index < args.length; index += 1) {
		const item = args[index];
		if (item === "--model" || item === "-m") {
			return args[index + 1];
		}
		if (item.startsWith("--model=")) {
			return item.slice("--model=".length);
		}
	}
	return undefined;
}

export function extractUsage(event) {
	const msg = record(event.msg);
	const usage = record(event.usage) || record(event.token_usage) || record(msg?.usage);
	const cost = numberValue(event.cost_usd) ?? numberValue(event.cost) ?? numberValue(msg?.cost_usd);
	if (!usage && cost === undefined) {
		return undefined;
	}
	return dropUndefined({
		provider: stringValue(event.provider),
		model: stringValue(event.model) || stringValue(msg?.model),
		input_tokens: integerValue(usage?.input_tokens ?? usage?.prompt_tokens),
		output_tokens: integerValue(usage?.output_tokens ?? usage?.completion_tokens),
		total_tokens: integerValue(usage?.total_tokens),
		cost_usd: cost,
		stop_reason: stringValue(event.stop_reason) || stringValue(msg?.stop_reason),
	});
}

export function codexEventKind(event) {
	return `codex.${stringValue(event.type) || stringValue(event.event_type) || "event"}`;
}

async function spawnCodex(config, args, io, decision) {
	const child = spawn(config.codexBin, args, {
		cwd: config.cwd,
		env: process.env,
		stdio: ["inherit", "pipe", "pipe"],
	});
	const reservationId = decision.reservation?.id;
	let lastUsage;
	child.stdout.setEncoding("utf8");
	child.stderr.setEncoding("utf8");
	child.stderr.on("data", (chunk) => {
		io.stderr.write(chunk);
	});
	child.stdout.on("data", (chunk) => {
		io.stdout.write(chunk);
		for (const line of String(chunk).split(/\r?\n/)) {
			if (!line.trim()) {
				continue;
			}
			let event;
			try {
				event = JSON.parse(line);
			} catch {
				continue;
			}
			const usage = extractUsage(event);
			if (usage) {
				lastUsage = usage;
			}
			void postEvent(config, {
				trace_id: decision.decision_id,
				kind: codexEventKind(event),
				payload: dropUndefined({
					source: SOURCE,
					harness: "codex",
					decision_id: decision.decision_id,
					reservation_id: reservationId,
					event,
				}),
			});
		}
	});
	const exitCode = await new Promise((resolve) => {
		child.on("close", (code) => resolve(code ?? 1));
	});
	if (reservationId && lastUsage) {
		await finalizeReservation(config, reservationId, decision, lastUsage, exitCode);
	}
	return { exitCode };
}

async function safeAuthorize(config, request) {
	try {
		const response = await fetchWithTimeout(config, `${config.noetherUrl}/v1/authorize`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`Noether authorize returned ${response.status}`);
		}
		return response.json();
	} catch (error) {
		return syntheticDecision(config.failMode, error);
	}
}

async function postEvent(config, event) {
	try {
		await fetchWithTimeout(config, `${config.noetherUrl}/v1/events`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(dropUndefined(event)),
		});
	} catch {
		// Codex execution must not depend on best-effort event delivery.
	}
}

async function finalizeReservation(config, reservationId, decision, usage, exitCode) {
	await fetchWithTimeout(config, `${config.noetherUrl}/v1/reservations/${encodeURIComponent(reservationId)}/finalize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({
			reservation_id: reservationId,
			outcome: "success",
			actual_cost_usd: usage.cost_usd,
			usage,
			metadata: {
				source: SOURCE,
				harness: "codex",
				decision_id: decision.decision_id,
				exit_code: exitCode,
			},
		}),
	});
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

function syntheticDecision(failMode, error) {
	const allow = failMode === "fail_open";
	return {
		decision_id: `codex-${failMode}`,
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

function summarizeArgs(args) {
	return args.map((arg) => {
		if (arg.length > 120) {
			return { type: "string", length: arg.length };
		}
		return arg;
	});
}

function decisionReason(decision) {
	const explanations = Array.isArray(decision.explanations) ? decision.explanations : [];
	return explanations.map((explanation) => explanation?.reason).filter(Boolean).join("; ") || "Noether denied Codex run";
}

function defaultIo() {
	return {
		stdout: process.stdout,
		stderr: process.stderr,
	};
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

function stringValue(value) {
	return typeof value === "string" && value ? value : undefined;
}

function integerValue(value) {
	return typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : undefined;
}

function numberValue(value) {
	return typeof value === "number" && Number.isFinite(value) ? Math.max(0, value) : undefined;
}

function record(value) {
	return value && typeof value === "object" && !Array.isArray(value) ? value : undefined;
}

function dropUndefined(value) {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
	const result = await runCodex(process.argv.slice(2));
	process.exitCode = result.exitCode;
}
