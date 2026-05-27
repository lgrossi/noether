import assert from "node:assert/strict";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import test from "node:test";

import {
	claudeCodeConfig,
	buildAuthorizeRequest,
	denyOutput,
	extractAgentUsage,
	handleHook,
	summarizeValue,
} from "../noether-claude-code.mjs";

test("authorize request maps Claude Code tool hook without prompt content", () => {
	const config = claudeCodeConfig(
		{ cwd: "/repo/noether" },
		{ NOET_CC_SUBJECT: "alice", NOET_CC_TIMEOUT_MS: "250" },
	);
	const request = buildAuthorizeRequest(config, {
		hook_event_name: "PreToolUse",
		session_id: "session-1",
		tool_name: "Bash",
		tool_use_id: "tool-1",
		tool_input: { command: "echo secret", description: "private", model: "sonnet" },
		cwd: "/repo/noether",
		permission_mode: "default",
	});

	assert.equal(request.provider, "claude-code");
	assert.equal(request.model, "sonnet");
	assert.equal(request.project, "noether");
	assert.equal(request.subject, "alice");
	assert.deepEqual(request.entities, ["project:noether", "user:alice", "tool:Bash"]);
	assert.equal(request.metadata.harness, "claude-code");
	assert.equal(request.metadata.integration, "noether-claude-code");
	assert.equal(request.metadata.tool_name, "Bash");
	assert.equal(JSON.stringify(request).includes("echo secret"), false);
});

test("deny output uses Claude Code event-specific schemas", () => {
	const decision = {
		outcome: "deny",
		explanations: [{ reason: "tool budget exceeded" }],
	};

	assert.deepEqual(denyOutput("PreToolUse", decision), {
		hookSpecificOutput: {
			hookEventName: "PreToolUse",
			permissionDecision: "deny",
			permissionDecisionReason: "tool budget exceeded",
		},
	});
	assert.deepEqual(denyOutput("PermissionRequest", decision), {
		hookSpecificOutput: {
			hookEventName: "PermissionRequest",
			decision: {
				behavior: "deny",
				message: "tool budget exceeded",
				interrupt: false,
			},
		},
	});
});

test("pre tool hook denies from Noether decision and records event", async () => {
	const calls = [];
	const originalFetch = globalThis.fetch;
	const stateDir = await mkdtemp(resolve(tmpdir(), "noether-cc-"));
	globalThis.fetch = async (url, init) => {
		const body = init.body && JSON.parse(init.body);
		calls.push({ url: String(url), body });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "decision-1",
				outcome: "deny",
				action: "block",
				explanations: [{ reason: "blocked by policy" }],
				created_at: "2026-05-27T00:00:00Z",
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};
	try {
		const output = await handleHook(
			{
				hook_event_name: "PreToolUse",
				session_id: "session-1",
				tool_name: "Bash",
				tool_use_id: "tool-1",
				tool_input: { command: "rm -rf private" },
				cwd: "/repo/noether",
			},
			{ NOET_CC_STATE_DIR: stateDir },
		);

		assert.equal(output.hookSpecificOutput.permissionDecision, "deny");
		assert.equal(output.hookSpecificOutput.permissionDecisionReason, "blocked by policy");
		assert(calls.some((call) => call.url.endsWith("/v1/authorize")));
		assert(calls.some((call) => call.body?.kind === "claude_code.tool_authorize"));
		assert.equal(JSON.stringify(calls).includes("rm -rf private"), false);
	} finally {
		globalThis.fetch = originalFetch;
	}
});

test("allowed tool reservation is finalized after Agent tool usage", async () => {
	const calls = [];
	const originalFetch = globalThis.fetch;
	const stateDir = await mkdtemp(resolve(tmpdir(), "noether-cc-"));
	globalThis.fetch = async (url, init) => {
		const body = init.body && JSON.parse(init.body);
		calls.push({ url: String(url), body });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "decision-1",
				outcome: "allow",
				action: "allow",
				reservation: { id: "reservation-1" },
				explanations: [],
				created_at: "2026-05-27T00:00:00Z",
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};
	try {
		const common = {
			session_id: "session-1",
			tool_name: "Agent",
			tool_use_id: "tool-1",
			cwd: "/repo/noether",
		};
		await handleHook(
			{
				...common,
				hook_event_name: "PreToolUse",
				tool_input: { prompt: "private", model: "sonnet" },
			},
			{ NOET_CC_STATE_DIR: stateDir },
		);
		await readFile(resolve(stateDir, "session-1-tool-1.json"), "utf8");

		await handleHook(
			{
				...common,
				hook_event_name: "PostToolUse",
				tool_input: { prompt: "private", model: "sonnet" },
				tool_response: {
					status: "completed",
					totalTokens: 30,
					totalDurationMs: 2000,
					usage: { input_tokens: 10, output_tokens: 20 },
					content: [{ type: "text", text: "private answer" }],
				},
			},
			{ NOET_CC_STATE_DIR: stateDir },
		);

		const finalize = calls.find((call) => call.url.includes("/v1/reservations/reservation-1/finalize"));
		assert.equal(finalize.body.actual_cost_usd, 0);
		assert.equal(finalize.body.usage.input_tokens, 10);
		assert.equal(finalize.body.usage.output_tokens, 20);
		assert.equal(finalize.body.usage.total_tokens, 30);
		assert.equal(finalize.body.metadata.outcome, "success");
		assert.equal(JSON.stringify(calls).includes("private answer"), false);
	} finally {
		globalThis.fetch = originalFetch;
	}
});

test("fail_closed sidecar outage denies pre tool use", async () => {
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async () => {
		throw new Error("sidecar unavailable");
	};
	try {
		const output = await handleHook(
			{
				hook_event_name: "PreToolUse",
				session_id: "session-1",
				tool_name: "Bash",
				tool_input: { command: "npm test" },
				cwd: "/repo/noether",
			},
			{ NOET_CC_FAIL_MODE: "fail_closed" },
		);

		assert.equal(output.hookSpecificOutput.permissionDecision, "deny");
		assert.match(output.hookSpecificOutput.permissionDecisionReason, /fail_closed/);
	} finally {
		globalThis.fetch = originalFetch;
	}
});

test("extracts Agent subtask usage only when Claude Code exposes it", () => {
	assert.deepEqual(extractAgentUsage({
		tool_name: "Agent",
		tool_input: { model: "sonnet" },
		tool_response: {
			status: "completed",
			totalTokens: 30,
			totalDurationMs: 2000,
			usage: { input_tokens: 10, output_tokens: 20 },
		},
	}), {
		provider: "claude-code",
		model: "sonnet",
		input_tokens: 10,
		output_tokens: 20,
		total_tokens: 30,
		latency_ms: 2000,
		stop_reason: "completed",
	});
	assert.equal(extractAgentUsage({ tool_name: "Bash", tool_response: {} }), undefined);
});

test("summarizer redacts sensitive fields by default", () => {
	const summary = summarizeValue({
		command: "secret command",
		file_path: "/repo/file",
		content: "secret content",
		ok: true,
	});

	assert.deepEqual(summary.command, { type: "string", length: 14 });
	assert.deepEqual(summary.content, { type: "string", length: 14 });
	assert.deepEqual(summary.file_path, { type: "string", length: 10 });
	assert.equal(summary.ok, true);
});
