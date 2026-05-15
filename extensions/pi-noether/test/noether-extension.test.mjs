import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const extension = await import(resolve(__dirname, "../../../.noet/build/pi-noether/index.js"));

function fakeContext(overrides = {}) {
	return {
		cwd: "/repo",
		model: {
			provider: "openai-codex",
			id: "gpt-5.5",
			api: "openai-codex-responses",
		},
		getContextUsage() {
			return {
				tokens: 1234,
				contextWindow: 128000,
				percent: 0.96,
			};
		},
		abort() {},
		...overrides,
	};
}

{
	const request = extension.buildAuthorizeRequest(
		{
			payload: {
				model: "gpt-5.5",
				input: [{ role: "user", content: "secret prompt" }],
				instructions: "private system prompt",
				stream: true,
			},
		},
		fakeContext(),
		{
			subject: "user@example.test",
			project: "noether",
			failMode: "fail_open",
			noetherUrl: "http://127.0.0.1:4040",
			version: "test",
			includeBody: false,
		},
	);

	assert.equal(request.subject, "user@example.test");
	assert.equal(request.project, "noether");
	assert.equal(request.provider, "openai-codex");
	assert.equal(request.model, "gpt-5.5");
	assert.equal(request.estimated_tokens, 1234);
	assert.equal(request.metadata.trace_id, undefined);
	assert.deepEqual(request.metadata.payload_summary.input, { type: "array", length: 1 });
	assert.deepEqual(request.metadata.payload_summary.instructions, { type: "string", length: 21 });
	assert.equal(JSON.stringify(request).includes("secret prompt"), false);
	assert.equal(JSON.stringify(request).includes("private system prompt"), false);
}

{
	const usage = extension.extractUsage({
		role: "assistant",
		provider: "anthropic",
		model: "claude",
		stopReason: "stop",
		usage: {
			input: 10,
			output: 20,
			totalTokens: 30,
			cost: { total: 0.001 },
		},
	});

	assert.deepEqual(usage, {
		provider: "anthropic",
		model: "claude",
		input_tokens: 10,
		output_tokens: 20,
		total_tokens: 30,
		cost_usd: 0.001,
		stop_reason: "stop",
	});
}

{
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init.body && JSON.parse(init.body) });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_1",
				outcome: "deny",
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const handlers = new Map();
		extension.default(
			{
				on(event, handler) {
					handlers.set(event, handler);
				},
			},
			{
				noetherUrl: "http://127.0.0.1:1",
				failMode: "fail_open",
				includeBody: false,
				version: "test",
			},
		);

		await handlers.get("before_provider_request")(
			{ payload: { model: "local", messages: [{ role: "user", content: "do not send" }] } },
			fakeContext({ abort: () => { aborted = true; } }),
		);

		assert.equal(aborted, true);
		assert.equal(calls.some((call) => call.url.endsWith("/v1/authorize")), true);
		const authorizeCall = calls.find((call) => call.url.endsWith("/v1/authorize"));
		assert.equal(typeof authorizeCall.body.metadata.trace_id, "string");
		assert.equal(typeof authorizeCall.body.metadata.request_id, "string");
		assert.equal(JSON.stringify(calls).includes("do not send"), false);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init.body && JSON.parse(init.body) });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_2",
				outcome: "allow",
				reservation: { id: "res_2" },
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const handlers = new Map();
		extension.default(
			{
				on(event, handler) {
					handlers.set(event, handler);
				},
			},
			{
				noetherUrl: "http://127.0.0.1:1",
				failMode: "fail_open",
				includeBody: false,
				version: "test",
			},
		);

		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext());
		await handlers.get("message_end")(
			{
				message: {
					role: "assistant",
					provider: "openai",
					model: "local",
					usage: { input: 1, output: 2, totalTokens: 3, cost: { total: 0.0001 } },
				},
			},
			fakeContext(),
		);

		const authorizeCall = calls.find((call) => call.url.endsWith("/v1/authorize"));
		const finalizeCall = calls.find((call) => call.url.includes("/v1/reservations/res_2/finalize"));
		assert.equal(finalizeCall.body.metadata.trace_id, authorizeCall.body.metadata.trace_id);
		assert.equal(finalizeCall.body.metadata.request_id, authorizeCall.body.metadata.request_id);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init.body && JSON.parse(init.body) });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_3",
				outcome: "allow",
				reservation: { id: "res_3" },
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const handlers = new Map();
		extension.default(
			{
				on(event, handler) {
					handlers.set(event, handler);
				},
			},
			{
				noetherUrl: "http://127.0.0.1:1",
				failMode: "fail_open",
				includeBody: false,
				version: "test",
			},
		);

		await handlers.get("before_agent_start")(
			{
				prompt: "private user prompt",
				systemPromptOptions: {
					selectedTools: ["read", "bash"],
					skills: [{ name: "diagnose" }],
					contextFiles: [{ path: "AGENTS.md" }],
					cwd: "/repo",
				},
			},
			fakeContext(),
		);
		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext());

		const authorizeCall = calls.find((call) => call.url.endsWith("/v1/authorize"));
		assert.deepEqual(authorizeCall.body.metadata.agent_context.selected_tools, ["read", "bash"]);
		assert.deepEqual(authorizeCall.body.metadata.agent_context.skills, ["diagnose"]);
		assert.deepEqual(authorizeCall.body.metadata.agent_context.context_files, ["AGENTS.md"]);
		assert.equal(JSON.stringify(calls).includes("private user prompt"), false);
		assert.equal(calls.some((call) => call.body.kind === "pi.agent_context"), true);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init.body && JSON.parse(init.body) });
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const handlers = new Map();
		extension.default(
			{
				on(event, handler) {
					handlers.set(event, handler);
				},
			},
			{
				noetherUrl: "http://127.0.0.1:1",
				failMode: "fail_open",
				includeBody: false,
				version: "test",
			},
		);

		await handlers.get("tool_call")(
			{ toolName: "bash", toolCallId: "tool_1", input: { command: "secret command" } },
			fakeContext(),
		);
		await handlers.get("tool_result")(
			{
				toolName: "bash",
				toolCallId: "tool_1",
				input: { command: "secret command" },
				content: [{ type: "text", text: "secret output" }],
				details: { exitCode: 0 },
				isError: false,
			},
			fakeContext(),
		);

		const observed = calls.find((call) => call.body.kind === "tool.observed");
		assert.equal(observed.body.payload.name, "bash");
		assert.equal(observed.body.payload.success, true);
		assert.equal(typeof observed.body.payload.duration_ms, "number");
		assert.deepEqual(observed.body.payload.metadata.input_summary.command, { type: "string", length: 14 });
		assert.equal(JSON.stringify(calls).includes("secret command"), false);
		assert.equal(JSON.stringify(calls).includes("secret output"), false);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const hookLogDir = await mkdtemp(resolve(tmpdir(), "pi-noether-hooks-"));
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init.body && JSON.parse(init.body) });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_hook",
				outcome: "allow",
				reservation: { id: "res_hook" },
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const handlers = new Map();
		extension.default(
			{
				on(event, handler) {
					handlers.set(event, handler);
				},
			},
			{
				noetherUrl: "http://127.0.0.1:1",
				failMode: "fail_open",
				includeBody: false,
				version: "test",
				hookLogDir,
			},
		);

		await handlers.get("before_provider_request")(
			{ payload: { model: "local", messages: [{ role: "user", content: "log me raw" }] } },
			fakeContext(),
		);
		await handlers.get("after_provider_response")(
			{ status: 200, headers: { "content-type": "text/event-stream" } },
			fakeContext(),
		);

		const beforeLines = (await readFile(resolve(hookLogDir, "before_provider_request.jsonl"), "utf8"))
			.trim()
			.split("\n")
			.map((line) => JSON.parse(line));
		const afterLines = (await readFile(resolve(hookLogDir, "after_provider_response.jsonl"), "utf8"))
			.trim()
			.split("\n")
			.map((line) => JSON.parse(line));
		const before = beforeLines.find((line) => line.payload.event);
		const after = afterLines.find((line) => line.payload.event);
		assert.equal(beforeLines[0].payload.extension_loaded, true);
		assert.equal(afterLines[0].payload.extension_loaded, true);
		assert.equal(before.hook, "before_provider_request");
		assert.equal(before.payload.event.payload.messages[0].content, "log me raw");
		assert.equal(before.payload.noether_authorize_request.metadata.payload_summary.messages.length, 1);
		assert.equal(after.hook, "after_provider_response");
		assert.equal(after.payload.event.status, 200);
	} finally {
		globalThis.fetch = originalFetch;
		await rm(hookLogDir, { recursive: true, force: true });
	}
}

console.log("pi-noether extension tests ok");
