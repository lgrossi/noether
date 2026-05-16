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

function sleep(ms = 0) {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitFor(predicate, label) {
	for (let attempt = 0; attempt < 20; attempt += 1) {
		if (await predicate()) {
			return;
		}
		await sleep(5);
	}
	assert.fail(`timed out waiting for ${label}`);
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
	assert.equal(request.budget_id, undefined);
	assert.equal(request.entities, undefined);
	assert.equal(request.provider, "openai-codex");
	assert.equal(request.model, "gpt-5.5");
	assert.equal(request.estimated_tokens, 1234);
	assert.equal(request.metadata.trace_id, undefined);
	assert.deepEqual(request.metadata.payload_summary.input, { type: "array", length: 1, item_types: { user: 1 } });
	assert.deepEqual(request.metadata.payload_summary.instructions, { type: "string", length: 21 });
	assert.equal(JSON.stringify(request).includes("secret prompt"), false);
	assert.equal(JSON.stringify(request).includes("private system prompt"), false);
}

{
	const config = extension.extensionConfig({
		NOET_URL: "http://127.0.0.1:4040/",
		NOET_PI_PROJECT: "noether",
		NOET_PI_SUBJECT: "user:alice",
		NOET_PI_BUDGET_ID: "project-noether",
		NOET_PI_ENTITIES: " project:noether, user:alice, ,",
	});
	const request = extension.buildAuthorizeRequest({ payload: { model: "local" } }, fakeContext(), config);

	assert.equal(config.noetherUrl, "http://127.0.0.1:4040");
	assert.equal(request.budget_id, "project-noether");
	assert.deepEqual(request.entities, ["project:noether", "user:alice"]);
}

{
	const request = extension.buildAuthorizeRequest(
		{
			payload: {
				model: "gpt-5.5",
				input: [
					{ type: "message", role: "user", content: "private" },
					{ type: "reasoning", text: "private thought" },
					{ type: "function_call", name: "bash", arguments: "{\"cmd\":\"secret\"}" },
					{ type: "function_call_output", output: "secret output" },
				],
				instructions: "private instructions",
				tools: [{ type: "function", name: "bash" }, { type: "function", name: "ask_user" }],
				tool_choice: "auto",
				reasoning: { effort: "high", summary: "auto" },
				text: { verbosity: "low" },
				include: ["reasoning.encrypted_content"],
				parallel_tool_calls: true,
				prompt_cache_key: "private-cache-key",
				store: false,
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

	assert.deepEqual(request.metadata.payload_summary.input.item_types, {
		message: 1,
		reasoning: 1,
		function_call: 1,
		function_call_output: 1,
	});
	assert.deepEqual(request.metadata.payload_summary.tools, { type: "array", length: 2 });
	assert.equal(request.metadata.payload_summary.reasoning.effort, "high");
	assert.equal(request.metadata.payload_summary.text.verbosity, "low");
	assert.deepEqual(request.metadata.payload_summary.prompt_cache_key, { present: true });
	assert.equal(JSON.stringify(request).includes("private-cache-key"), false);
	assert.equal(JSON.stringify(request).includes("secret output"), false);
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
			cacheRead: 4,
			cacheWrite: 5,
			cost: { input: 0.0001, output: 0.0002, cacheRead: 0.00003, cacheWrite: 0.00004, total: 0.001 },
		},
	});

	assert.deepEqual(usage, {
		provider: "anthropic",
		model: "claude",
		input_tokens: 10,
		output_tokens: 20,
		total_tokens: 30,
		cache_read_tokens: 4,
		cache_write_tokens: 5,
		input_cost_usd: 0.0001,
		output_cost_usd: 0.0002,
		cache_read_cost_usd: 0.00003,
		cache_write_cost_usd: 0.00004,
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
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		if (String(url).endsWith("/v1/authorize")) {
			return new Promise((_, reject) => {
				init.signal.addEventListener("abort", () => reject(init.signal.reason), { once: true });
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
				authorizeTimeoutMs: 5,
			},
		);

		let aborted = false;
		const returned = await Promise.race([
			handlers
				.get("before_provider_request")(
					{ payload: { model: "local" } },
					fakeContext({ abort: () => { aborted = true; } }),
				)
				.then(() => true),
			sleep(50).then(() => false),
		]);

		assert.equal(returned, true);
		assert.equal(aborted, false);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		if (String(url).endsWith("/v1/authorize")) {
			return new Promise((_, reject) => {
				init.signal.addEventListener("abort", () => reject(init.signal.reason), { once: true });
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
				failMode: "fail_closed",
				includeBody: false,
				version: "test",
				authorizeTimeoutMs: 5,
			},
		);

		let aborted = false;
		const returned = await Promise.race([
			handlers
				.get("before_provider_request")(
					{ payload: { model: "local" } },
					fakeContext({ abort: () => { aborted = true; } }),
				)
				.then(() => true),
			sleep(50).then(() => false),
		]);

		assert.equal(returned, true);
		assert.equal(aborted, true);
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

		await waitFor(
			() => calls.some((call) => call.url.includes("/v1/reservations/res_2/finalize")),
			"reservation finalization",
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
				decision_id: "dec_slow",
				outcome: "allow",
				reservation: { id: "res_slow" },
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		return new Promise(() => {});
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
		const messageReturned = await Promise.race([
			Promise.resolve(
				handlers.get("message_end")(
					{
						message: {
							role: "assistant",
							provider: "openai",
							model: "local",
							usage: { input: 1, output: 2, totalTokens: 3, cost: { total: 0.0001 } },
						},
					},
					fakeContext(),
				),
			).then(() => true),
			sleep(20).then(() => false),
		]);
		const turnReturned = await Promise.race([
			Promise.resolve(handlers.get("turn_end")({ turnIndex: 1 }, fakeContext())).then(() => true),
			sleep(20).then(() => false),
		]);
		const agentReturned = await Promise.race([
			Promise.resolve(handlers.get("agent_end")({ messages: [] }, fakeContext())).then(() => true),
			sleep(20).then(() => false),
		]);

		assert.equal(messageReturned, true);
		assert.equal(turnReturned, true);
		assert.equal(agentReturned, true);
		await waitFor(
			() => calls.some((call) => call.url.includes("/v1/reservations/res_slow/finalize")),
			"slow finalization started",
		);
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

		await waitFor(() => calls.some((call) => call.body.kind === "pi.agent_context"), "agent context event");
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

		await waitFor(() => calls.some((call) => call.body.kind === "tool.observed"), "tool observation event");
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
	const calls = [];
	let authorizeCount = 0;
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init.body && JSON.parse(init.body) });
		if (String(url).endsWith("/v1/authorize")) {
			authorizeCount += 1;
			return Response.json({
				decision_id: `dec_${authorizeCount}`,
				outcome: "allow",
				reservation: { id: `res_${authorizeCount}` },
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

		await handlers.get("before_provider_request")({ payload: { model: "a" } }, fakeContext());
		const firstProviderCallId = calls.find((call) => call.url.endsWith("/v1/authorize")).body.metadata.provider_call_id;
		await handlers.get("message_update")(
			{ responseId: "resp_a", type: "toolcall_start", toolCallId: "tool_a", toolName: "ask_user" },
			fakeContext(),
		);
		await handlers.get("before_provider_request")({ payload: { model: "b" } }, fakeContext());
		const secondProviderCallId = calls
			.filter((call) => call.url.endsWith("/v1/authorize"))
			.at(-1).body.metadata.provider_call_id;
		assert.notEqual(firstProviderCallId, secondProviderCallId);

		await handlers.get("tool_result")(
			{ toolName: "ask_user", toolCallId: "tool_a", content: "private answer", isError: false },
			fakeContext(),
		);
		await handlers.get("message_end")(
			{
				message: {
					id: "resp_a",
					role: "assistant",
					provider: "openai",
					model: "a",
					stopReason: "toolUse",
					toolCalls: [{ id: "tool_a", name: "ask_user", arguments: "private args" }],
					usage: { input: 1, output: 2, totalTokens: 3, cost: { total: 0.0001 } },
				},
			},
			fakeContext(),
		);

		await waitFor(() => calls.some((call) => call.url.includes("/v1/reservations/res_1/finalize")), "first reservation finalization");
		const observed = calls.find((call) => call.body?.kind === "tool.observed");
		const messageEnd = calls.find((call) => call.body?.kind === "pi.message_end");
		const streamSummary = calls.find((call) => call.body?.kind === "pi.stream_summary");
		const finalize = calls.find((call) => call.url.includes("/v1/reservations/res_1/finalize"));
		assert.equal(observed.body.payload.provider_call_id, firstProviderCallId);
		assert.equal(observed.body.payload.attribution_status, "exact");
		assert.equal(messageEnd.body.payload.provider_call_id, firstProviderCallId);
		assert.equal(messageEnd.body.payload.message.tool_calls[0].tool_call_id, "tool_a");
		assert.equal(JSON.stringify(messageEnd).includes("private args"), false);
		assert.equal(streamSummary.body.payload.counts.toolcall_start, 1);
		assert.equal(finalize.body.metadata.provider_call_id, firstProviderCallId);
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

		await handlers.get("tool_result")({ toolName: "bash", toolCallId: "orphan", isError: true }, fakeContext());

		await waitFor(() => calls.some((call) => call.body.kind === "tool.observed"), "unmatched tool observation");
		const observed = calls.find((call) => call.body.kind === "tool.observed");
		assert.equal(observed.body.payload.attribution_status, "unmatched");
		assert.equal(observed.body.payload.provider_call_id, undefined);
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
				debugHooks: true,
				debugHookLogDir: hookLogDir,
			},
		);
		assert.equal(handlers.has("after_provider_response"), false);

		await handlers.get("before_provider_request")(
			{ payload: { model: "local", messages: [{ role: "user", content: "log me raw" }] } },
			fakeContext(),
		);

		await waitFor(async () => {
			try {
				await readFile(resolve(hookLogDir, "before_provider_request.raw.jsonl"), "utf8");
				return true;
			} catch {
				return false;
			}
		}, "raw before hook log");
		const beforeLines = (await readFile(resolve(hookLogDir, "before_provider_request.raw.jsonl"), "utf8"))
			.trim()
			.split("\n")
			.map((line) => JSON.parse(line));
		const before = beforeLines.find((line) => line.payload.event);
		assert.equal(before.hook, "before_provider_request");
		assert.equal(before.payload.event.payload.messages[0].content, "log me raw");
		assert.equal(before.payload.noether_authorize_request.metadata.payload_summary.messages.length, 1);
	} finally {
		globalThis.fetch = originalFetch;
		await rm(hookLogDir, { recursive: true, force: true });
	}
}

{
	const hookLogDir = await mkdtemp(resolve(tmpdir(), "pi-noether-hooks-default-"));
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_default_no_hook",
				outcome: "allow",
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
				debugHookLogDir: hookLogDir,
			},
		);

		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext());
		await assert.rejects(readFile(resolve(hookLogDir, "before_provider_request.raw.jsonl"), "utf8"));
	} finally {
		globalThis.fetch = originalFetch;
		await rm(hookLogDir, { recursive: true, force: true });
	}
}

{
	const hookLogDir = await mkdtemp(resolve(tmpdir(), "pi-noether-hooks-disabled-"));
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_no_hook",
				outcome: "allow",
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
				debugHooks: false,
				debugHookLogDir: hookLogDir,
			},
		);

		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext());
		await assert.rejects(readFile(resolve(hookLogDir, "before_provider_request.raw.jsonl"), "utf8"));
	} finally {
		globalThis.fetch = originalFetch;
		await rm(hookLogDir, { recursive: true, force: true });
	}
}

console.log("pi-noether extension tests ok");
