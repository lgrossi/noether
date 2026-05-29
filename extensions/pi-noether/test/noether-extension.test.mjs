import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const extension = await import(resolve(__dirname, "../../../.noet/build/pi-noether/index.js"));

function fakeContext(overrides = {}) {
	return {
		cwd: "/repo",
		hasUI: true,
		model: {
			provider: "openai-codex",
			id: "gpt-5.5",
			api: "openai-codex-responses",
		},
		ui: {
			notify() {},
			setStatus() {},
			async confirm() {
				return false;
			},
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

function captureUiSignals() {
	const notifications = [];
	const statuses = [];
	const confirms = [];
	return {
		confirms,
		notifications,
		statuses,
		ui: {
			notify(message, type) {
				notifications.push({ message, type });
			},
			setStatus(key, text) {
				statuses.push({ key, text });
			},
			async confirm(title, message, options) {
				confirms.push({ title, message, options });
				return false;
			},
		},
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

function deferred() {
	let resolve;
	const promise = new Promise((innerResolve) => {
		resolve = innerResolve;
	});
	return { promise, resolve };
}

{
	const queue = extension.createDeliveryQueue(2);
	const release = deferred();
	let running = 0;
	let maxRunning = 0;
	let completed = 0;

	for (let index = 0; index < 4; index += 1) {
		queue.enqueue(
			3,
			async () => {
				running += 1;
				maxRunning = Math.max(maxRunning, running);
				await release.promise;
				running -= 1;
				completed += 1;
			},
			`item-${index}`,
		);
	}

	await sleep(10);
	assert.equal(maxRunning, 2);
	release.resolve();
	await waitFor(() => completed === 4, "bounded queue completion");
	assert.equal(maxRunning, 2);
}

{
	const calls = [];
	const blockedEvent = deferred();
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		const body = init.body && JSON.parse(init.body);
		calls.push({ url: String(url), body });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_drop",
				outcome: "allow",
				reservation: { id: "res_drop" },
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		if (String(url).endsWith("/v1/events") && body.kind === "pi.delivery_drop") {
			return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
		}
		if (String(url).endsWith("/v1/events")) {
			return blockedEvent.promise;
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
				queueMaxItems: 1,
			},
		);

		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext());
		await handlers.get("tool_call")({ toolName: "bash", toolCallId: "tool_drop" }, fakeContext());
		await handlers.get("tool_result")({ toolName: "bash", toolCallId: "tool_drop", isError: false }, fakeContext());

		await waitFor(
			() => calls.some((call) => call.body?.kind === "pi.delivery_drop"),
			"delivery drop event",
		);
		const drop = calls.find((call) => call.body?.kind === "pi.delivery_drop");
		assert(["replaced", "rejected"].includes(drop.body.payload.reason));
		assert.equal(typeof drop.body.payload.dropped_kind, "string");
		assert.equal(typeof drop.body.payload.enqueued_kind, "string");
	} finally {
		blockedEvent.resolve(new Response("{}", { status: 202, headers: { "content-type": "application/json" } }));
		globalThis.fetch = originalFetch;
	}
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
	assert.deepEqual(request.entities, ["project:noether", "user:user@example.test"]);
	assert.equal(request.provider, "openai-codex");
	assert.equal(request.model, "gpt-5.5");
	assert.equal(request.estimated_tokens, 1234);
	assert.equal(request.metadata.request_surface, "responses");
	assert.equal(request.metadata.trace_id, undefined);
	assert.deepEqual(request.metadata.payload_summary.input, { type: "array", length: 1, item_types: { user: 1 } });
	assert.deepEqual(request.metadata.payload_summary.instructions, { type: "string", length: 21 });
	assert.equal(JSON.stringify(request).includes("secret prompt"), false);
	assert.equal(JSON.stringify(request).includes("private system prompt"), false);
}

{
	const request = extension.buildAuthorizeRequest(
		{
			payload: {
				model: "gpt-5.5",
				input: [{ role: "user", content: "private" }],
			},
		},
		fakeContext({
			model: {
				provider: "openai-codex",
				id: "gpt-5.5",
				api: "openai-codex-responses",
			},
		}),
		{
			subject: "user:alice",
			project: "noether",
			failMode: "fail_open",
			noetherUrl: "http://127.0.0.1:4051",
			version: "test",
			includeBody: false,
		},
		{
			traceId: "trace-openapi",
			sessionId: "session-openapi",
			agentRunId: "run-openapi",
			requestId: "request-openapi",
			providerCallId: "provider-call-openapi",
		},
	);

	assert.equal(request.metadata.harness, "pi");
	assert.equal(request.metadata.extension, "noether-pi");
	assert.equal(request.metadata.trace_id, "trace-openapi");
	assert.equal(request.metadata.session_id, "session-openapi");
	assert.equal(request.metadata.agent_run_id, "run-openapi");
	assert.equal(request.metadata.request_id, "request-openapi");
	assert.equal(request.metadata.provider_call_id, "provider-call-openapi");
	assert.equal(request.provider, "openai-codex");
	assert.equal(request.model, "gpt-5.5");
	assert.equal(request.metadata.model_api, "openai-codex-responses");
	assert.equal(request.metadata.request_surface, "responses");
	assert.equal(request.metadata.harness, "pi");
	assert.notEqual(request.provider, "pi");
}

{
	const originalUser = process.env.USER;
	const originalLogname = process.env.LOGNAME;
	process.env.USER = "lgrossi";
	delete process.env.LOGNAME;

	try {
		const request = extension.buildAuthorizeRequest(
			{
				payload: {
					provider: "anthropic",
					model: "claude-sonnet",
					messages: [{ role: "user", content: "hello" }],
				},
			},
			fakeContext({
				model: {
					provider: "anthropic",
					id: "claude-sonnet",
					api: "anthropic-messages",
				},
			}),
			{
				failMode: "fail_open",
				noetherUrl: "http://127.0.0.1:4040",
				version: "test",
				includeBody: false,
			},
		);

		assert.equal(request.subject, "user:lgrossi");
		assert(request.entities.includes("user:lgrossi"));
		assert.equal(request.metadata.request_surface, "messages");
	} finally {
		if (originalUser === undefined) {
			delete process.env.USER;
		} else {
			process.env.USER = originalUser;
		}
		if (originalLogname === undefined) {
			delete process.env.LOGNAME;
		} else {
			process.env.LOGNAME = originalLogname;
		}
	}
}

{
	const config = extension.extensionConfig({
		NOET_URL: "http://127.0.0.1:4040/",
		NOET_PI_PROJECT: "noether",
		NOET_PI_SUBJECT: "user:alice",
		NOET_PI_BUDGET_ID: "project-noether",
		NOET_PI_ENTITIES: " project:noether, user:alice, ,",
	}, { loadFiles: false });
	const request = extension.buildAuthorizeRequest({ payload: { model: "local" } }, fakeContext(), config);

	assert.equal(config.noetherUrl, "http://127.0.0.1:4040");
	assert.equal(request.budget_id, "project-noether");
	assert.deepEqual(request.entities, ["project:noether", "user:alice"]);
}

{
	const configRoot = await mkdtemp(resolve(tmpdir(), "pi-noether-config-"));

	try {
		await mkdir(resolve(configRoot, ".pi"), { recursive: true });
		await writeFile(
			resolve(configRoot, ".pi/noether.json"),
			JSON.stringify({
				noetherUrl: "http://127.0.0.1:4051",
			}),
			"utf8",
		);

		const defaults = extension.extensionConfig({}, { loadFiles: false });
		const persisted = extension.extensionConfig({}, { cwd: configRoot });

		assert.equal(defaults.failMode, "fail_open");
		assert.equal(defaults.autoStartLocal, false);
		assert.equal(persisted.noetherUrl, "http://127.0.0.1:4051");
		assert.equal(persisted.autoStartLocal, true);
	} finally {
		await rm(configRoot, { recursive: true, force: true });
	}
}

{
	const config = extension.extensionConfig({
		NOET_URL: "http://127.0.0.1:4051",
		NOET_PI_LOCAL_BIN: "/repo/target/debug/noet",
	}, { loadFiles: false });

	assert.equal(config.autoStartLocal, true);
	assert.equal(config.localBin, "/repo/target/debug/noet");
}

{
	const calls = [];
	let healthy = false;
	let starts = 0;
	const localRoot = await mkdtemp(resolve(tmpdir(), "pi-noether-sidecar-"));
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init?.body && JSON.parse(init.body) });
		if (String(url).endsWith("/health")) {
			return new Response(healthy ? "ok" : "down", { status: healthy ? 200 : 503 });
		}
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_boot",
				outcome: "allow",
				reservation: { id: "res_boot" },
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
				noetherUrl: "http://127.0.0.1:4051",
				failMode: "fail_open",
				includeBody: false,
				version: "test",
				autoStartLocal: true,
				localRoot,
				localStartTimeoutMs: 1000,
				startLocalNoether: async () => {
					starts += 1;
					healthy = true;
					return process.pid;
				},
			},
		);

		await handlers.get("session_start")({ reason: "startup" }, fakeContext({ cwd: localRoot }));
		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext());

		assert.equal(starts, 1);
		assert.equal(calls.some((call) => call.url.endsWith("/health")), true);
		assert.equal(calls.some((call) => call.url.endsWith("/v1/authorize")), true);
	} finally {
		globalThis.fetch = originalFetch;
		await rm(localRoot, { recursive: true, force: true });
	}
}

{
	const calls = [];
	let healthy = false;
	let starts = 0;
	let stops = 0;
	const localRoot = await mkdtemp(resolve(tmpdir(), "pi-noether-sidecar-shutdown-"));
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init?.body && JSON.parse(init.body) });
		if (String(url).endsWith("/health")) {
			return new Response(healthy ? "ok" : "down", { status: healthy ? 200 : 503 });
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const handlersA = new Map();
		const handlersB = new Map();
		const config = {
			noetherUrl: "http://127.0.0.1:4051",
			failMode: "fail_open",
			includeBody: false,
			version: "test",
			autoStartLocal: true,
			localRoot,
			localStartTimeoutMs: 1000,
			startLocalNoether: async () => {
				starts += 1;
				healthy = true;
				return process.pid;
			},
			stopLocalNoether: async () => {
				stops += 1;
				healthy = false;
			},
		};

		extension.default(
			{ on(event, handler) { handlersA.set(event, handler); } },
			config,
		);
		extension.default(
			{ on(event, handler) { handlersB.set(event, handler); } },
			config,
		);

		await handlersA.get("session_start")({ reason: "startup" }, fakeContext({ cwd: localRoot }));
		await handlersB.get("session_start")({ reason: "startup" }, fakeContext({ cwd: localRoot }));
		assert.equal(starts, 1);

		await handlersA.get("session_shutdown")({ reason: "quit" }, fakeContext({ cwd: localRoot }));
		assert.equal(stops, 0);

		await handlersB.get("session_shutdown")({ reason: "quit" }, fakeContext({ cwd: localRoot }));
		assert.equal(stops, 1);
	} finally {
		globalThis.fetch = originalFetch;
		await rm(localRoot, { recursive: true, force: true });
	}
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
	const request = extension.buildAuthorizeRequest(
		{ payload: { model: "gpt-5.5" } },
		fakeContext({ cwd: "/repo/customer-search" }),
		{
			noetherUrl: "http://127.0.0.1:4040",
			failMode: "fail_open",
			includeBody: false,
			version: "test",
			authorizeTimeoutMs: 1000,
			queueMaxItems: 10,
			debugHooks: false,
			projectFromCwd: true,
		},
		{ providerCallId: "call-1" },
	);

	assert.equal(request.project, "customer-search");
	assert(request.entities.includes("project:customer-search"));
}

{
	const request = extension.buildAuthorizeRequest(
		{ payload: { model: "gpt-5.5" } },
		fakeContext({ cwd: "/repo/noether" }),
		{
			noetherUrl: "http://127.0.0.1:4040",
			failMode: "fail_open",
			includeBody: false,
			version: "test",
			authorizeTimeoutMs: 1000,
			queueMaxItems: 10,
			debugHooks: false,
			projectFromCwd: true,
			synthetic: {
				enabled: true,
				users: 50,
				teams: 6,
				companies: 3,
				workflows: ["coding", "review"],
				surfaces: ["editor", "terminal"],
			},
		},
		{ providerCallId: "call-2" },
	);

	assert.equal(request.project, "noether");
	assert.match(request.subject, /^user:user-\d{2}$/);
	assert(request.entities.includes("project:noether"));
	assert(request.entities.some((value) => value.startsWith("org:company-")));
	assert(request.entities.some((value) => value.startsWith("team:team-")));
	assert(request.entities.some((value) => value.startsWith("workflow:")));
	assert(request.entities.some((value) => value.startsWith("surface:")));
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
				action: "block",
				explanations: [
					{ rule_id: "daily-cap", reason: "daily budget exceeded", severity: "deny" },
				],
				routing: {
					rejected_budget_id: "project-noether",
					rejected_budget_reason: "remaining budget is 0",
				},
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const ui = captureUiSignals();
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
			fakeContext({
				ui: ui.ui,
				abort: () => {
					aborted = true;
				},
			}),
		);

		assert.equal(aborted, true);
		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "error");
		assert.equal(ui.notifications[0].message, "Daily budget exceeded.");
		assert.doesNotMatch(ui.notifications[0].message, /decision dec_1/);
		assert(ui.statuses.some((status) => status.text === "Daily budget exceeded."));
		assert.equal(calls.some((call) => call.url.endsWith("/v1/authorize")), true);
		const authorizeCall = calls.find((call) => call.url.endsWith("/v1/authorize"));
		assert.equal(typeof authorizeCall.body.metadata.trace_id, "string");
		assert.equal(typeof authorizeCall.body.metadata.request_id, "string");
		assert.equal(JSON.stringify(calls).includes("do not send"), false);
		await waitFor(() => calls.some((call) => call.body?.kind === "pi.authorize"), "authorize deny event");
		const authorizeEvent = calls.find((call) => call.body?.kind === "pi.authorize");
		assert.equal(authorizeEvent.body.payload.decision_action, "block");
		assert.equal(authorizeEvent.body.payload.policy_action, "block");
		assert.match(authorizeEvent.body.payload.decision_reason, /daily budget exceeded/);
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
				decision_id: "dec_warn",
				outcome: "warn",
				action: "warn",
				explanations: [
					{ rule_id: "restricted-model", reason: "model access requires a different budget", severity: "deny" },
				],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const ui = captureUiSignals();
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
			{ payload: { model: "local" } },
			fakeContext({
				ui: ui.ui,
				abort: () => {
					aborted = true;
				},
			}),
		);

		assert.equal(aborted, false);
		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "warning");
		assert.equal(ui.notifications[0].message, "Model access requires a different budget.");
		await waitFor(() => calls.some((call) => call.body?.kind === "pi.authorize"), "authorize warn-mode event");
		const authorizeEvent = calls.find((call) => call.body?.kind === "pi.authorize");
		assert.equal(authorizeEvent.body.payload.decision_action, "warn");
		assert.equal(authorizeEvent.body.payload.policy_action, "warn");
		assert.match(authorizeEvent.body.payload.decision_reason, /different budget/);
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
				decision_id: "dec_spend_warn",
				outcome: "warn",
				action: "warn",
				explanations: [
					{
						rule_id: "personal-local.spend_window.monthly-cap",
						reason: "projected spend $800.230526 reaches warning threshold $800.000000 for 30d window",
						severity: "warn",
					},
				],
				metadata: {
					message_hints: [
						{
							kind: "spend_threshold",
							rule_id: "personal-local.spend_window.monthly-cap",
							severity: "warn",
							limit_type: "spend",
							window_id: "monthly-cap",
							window_label: "30d",
							window_mode: "tumbling",
							window_ends_at: "2026-06-01T09:00:00.000Z",
							threshold_percent: 80,
						},
					],
				},
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const ui = captureUiSignals();
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
			{ payload: { model: "local" } },
			fakeContext({ ui: ui.ui, abort: () => { aborted = true; } }),
		);

		assert.equal(aborted, false);
		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "warning");
		assert.match(ui.notifications[0].message, /^Monthly budget 80% reached\./);
		assert.match(ui.notifications[0].message, /Resets/);
		assert.match(ui.notifications[0].message, /Jun 1/);
		assert.match(ui.notifications[0].message, /Consider a cheaper model\./);
		assert.doesNotMatch(ui.notifications[0].message, /projected spend/);
		assert.doesNotMatch(ui.notifications[0].message, /decision dec_spend_warn/);
		await waitFor(() => calls.some((call) => call.body?.kind === "pi.authorize"), "authorize spend warning event");
		const authorizeEvent = calls.find((call) => call.body?.kind === "pi.authorize");
		assert.match(authorizeEvent.body.payload.decision_reason, /projected spend/);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_context_and_spend_warn",
				outcome: "warn",
				action: "warn",
				explanations: [
					{
						rule_id: "personal-local.context_tokens",
						reason: "estimated context tokens 120000 exceeds context limit max 100000",
						severity: "warn",
					},
					{
						rule_id: "personal-local.spend_window.monthly-cap",
						reason: "projected spend $800.230526 reaches warning threshold $800.000000 for 30d window",
						severity: "warn",
					},
				],
				metadata: {
					message_hints: [
						{
							kind: "context_tokens",
							rule_id: "personal-local.context_tokens",
							severity: "warn",
							limit_type: "context_tokens",
						},
						{
							kind: "spend_threshold",
							rule_id: "personal-local.spend_window.monthly-cap",
							severity: "warn",
							limit_type: "spend",
							window_id: "monthly-cap",
							window_label: "30d",
							window_mode: "tumbling",
							window_ends_at: "2026-06-01T09:00:00.000Z",
							threshold_percent: 80,
						},
					],
				},
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const ui = captureUiSignals();
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

		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext({ ui: ui.ui }));

		assert.equal(ui.notifications.length, 1);
		assert.match(ui.notifications[0].message, /Large context/);
		assert.match(ui.notifications[0].message, /Monthly budget 80% reached/);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_hidden_context_warn",
				outcome: "warn",
				action: "warn",
				explanations: [
					{
						rule_id: "personal-local.context_tokens",
						reason: "estimated context tokens 120000 exceeds context limit max 100000",
						severity: "warn",
					},
				],
				metadata: {
					message_hints: [
						{
							kind: "context_tokens",
							rule_id: "personal-local.context_tokens",
							severity: "warn",
							recommendation: "hide",
							limit_type: "context_tokens",
						},
					],
				},
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const ui = captureUiSignals();
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

		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext({ ui: ui.ui }));

		assert.equal(ui.notifications.length, 0);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_enablement_tip",
				outcome: "allow",
				action: "allow",
				reservation: { id: "res_tip" },
				explanations: [],
				metadata: {
					notifications: [
						{
							kind: "enablement_tip",
							key: "workflow.codify_repeated_process",
							severity: "info",
							title: "AI budget headroom available",
							body: "Try codifying a repeated team workflow into a reusable skill or checklist.",
						},
					],
				},
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		const ui = captureUiSignals();
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

		await handlers.get("before_provider_request")({ payload: { model: "local" } }, fakeContext({ ui: ui.ui }));

		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "info");
		assert.match(ui.notifications[0].message, /AI budget headroom available/);
		assert.match(ui.notifications[0].message, /reusable skill or checklist/);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_model_block",
				outcome: "deny",
				action: "block",
				explanations: [
					{
						rule_id: "personal-local",
						reason: "requested provider/model is not allowed by requested budget",
						severity: "deny",
					},
					{
						rule_id: "no_fallback_budget",
						reason: "no fallback budget can satisfy the request",
						severity: "deny",
					},
					{
						rule_id: "personal-local",
						reason: "requested provider/model is not allowed by budget",
						severity: "deny",
					},
				],
				routing: {
					rejected_budget_id: "personal-local",
					rejected_budget_reason: "requested provider/model is not allowed by requested budget",
					model_check: "denied",
				},
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const ui = captureUiSignals();
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
			{ payload: { model: "gpt-5.4-mini" } },
			fakeContext({
				model: {
					provider: "openai-codex",
					id: "gpt-5.4-mini",
					api: "openai-codex-responses",
				},
				ui: ui.ui,
				abort: () => {
					aborted = true;
				},
			}),
		);

		assert.equal(aborted, true);
		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "error");
		assert.equal(ui.notifications[0].message, "Model not available on this budget. Choose another model or budget.");
		assert.doesNotMatch(ui.notifications[0].message, /requested provider\/model is not allowed by budget/);
		assert.doesNotMatch(ui.notifications[0].message, /requested provider\/model is not allowed by requested budget/);
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
				decision_id: "dec_user_approved",
				outcome: "deny",
				action: "ask",
				explanations: [
					{ rule_id: "restricted-tool", reason: "tool access requires explicit approval", severity: "deny" },
				],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const ui = captureUiSignals();
		ui.ui.confirm = async (title, message, options) => {
			ui.confirms.push({ title, message, options });
			return true;
		};
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
			{ payload: { model: "local" } },
			fakeContext({
				ui: ui.ui,
				abort: () => {
					aborted = true;
				},
			}),
		);

		assert.equal(aborted, false);
		assert.equal(ui.confirms.length, 1);
		assert.equal(ui.confirms[0].title, "Continue anyway?");
		assert.match(ui.confirms[0].message, /Tool access requires explicit approval/);
		assert.match(ui.confirms[0].message, /Continue this request\?/);
		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "warning");
		assert.equal(ui.notifications[0].message, "Continuing by request.");
		await waitFor(() => calls.some((call) => call.body?.kind === "pi.authorize"), "authorize user-approved approval event");
		const authorizeEvent = calls.find((call) => call.body?.kind === "pi.authorize");
		assert.equal(authorizeEvent.body.payload.decision_action, "ask");
		assert.equal(authorizeEvent.body.payload.policy_action, "approved");
		assert.equal(authorizeEvent.body.payload.user_approval, "approved");
		assert.match(authorizeEvent.body.payload.decision_reason, /explicit approval/);
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
				decision_id: "dec_user_rejected",
				outcome: "deny",
				action: "ask",
				explanations: [
					{ rule_id: "after-hours", reason: "after-hours policy requires manual override", severity: "deny" },
				],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const ui = captureUiSignals();
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
			{ payload: { model: "local" } },
			fakeContext({
				ui: ui.ui,
				abort: () => {
					aborted = true;
				},
			}),
		);

		assert.equal(aborted, true);
		assert.equal(ui.confirms.length, 1);
		assert.match(ui.confirms[0].message, /After-hours policy requires manual override/);
		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "error");
		assert.equal(ui.notifications[0].message, "Request canceled.");
		await waitFor(() => calls.some((call) => call.body?.kind === "pi.authorize"), "authorize user-approved rejection event");
		const authorizeEvent = calls.find((call) => call.body?.kind === "pi.authorize");
		assert.equal(authorizeEvent.body.payload.decision_action, "ask");
		assert.equal(authorizeEvent.body.payload.policy_action, "block");
		assert.equal(authorizeEvent.body.payload.user_approval, "rejected");
		assert.match(authorizeEvent.body.payload.decision_reason, /manual override/);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_user_unavailable",
				outcome: "deny",
				action: "ask",
				explanations: [
					{ rule_id: "daily-cap", reason: "daily budget exceeded", severity: "deny" },
				],
				created_at: new Date().toISOString(),
			});
		}
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};

	try {
		let aborted = false;
		const ui = captureUiSignals();
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
			{ payload: { model: "local" } },
			fakeContext({
				hasUI: false,
				ui: ui.ui,
				abort: () => {
					aborted = true;
				},
			}),
		);

		assert.equal(aborted, true);
		assert.equal(ui.notifications.length, 1);
		assert.equal(ui.notifications[0].type, "error");
		assert.equal(ui.notifications[0].message, "Could not ask whether to continue. Request blocked.");
		assert.doesNotMatch(ui.notifications[0].message, /policyMode=user_approved could not collect approval/);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url) => {
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_allow_matrix",
				outcome: "allow",
				reservation: { id: "res_allow_matrix" },
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
			{ payload: { model: "local" } },
			fakeContext({ abort: () => { aborted = true; } }),
		);

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
			throw new Error("sidecar unavailable");
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

		let aborted = false;
		await handlers.get("before_provider_request")(
			{ payload: { model: "local" } },
			fakeContext({ abort: () => { aborted = true; } }),
		);

		assert.equal(aborted, false);
		await waitFor(() => calls.some((call) => call.body.kind === "pi.authorize_error"), "authorize error event");
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
			throw new Error("sidecar unavailable");
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
			},
		);

		let aborted = false;
		await handlers.get("before_provider_request")(
			{ payload: { model: "local" } },
			fakeContext({ abort: () => { aborted = true; } }),
		);

		assert.equal(aborted, true);
		await waitFor(() => calls.some((call) => call.body.kind === "pi.authorize_error"), "authorize error event");
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const calls = [];
	let authorizeCount = 0;
	let authorizeEventAttempts = 0;
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		const body = init.body && JSON.parse(init.body);
		calls.push({ url: String(url), body });
		if (String(url).endsWith("/v1/authorize")) {
			authorizeCount += 1;
			return Response.json({
				decision_id: `dec_retry_${authorizeCount}`,
				outcome: "allow",
				reservation: { id: `res_retry_${authorizeCount}` },
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		if (String(url).endsWith("/v1/events") && body.kind === "pi.authorize") {
			authorizeEventAttempts += 1;
			if (authorizeEventAttempts < 3) {
				return new Response("{}", { status: 503, headers: { "content-type": "application/json" } });
			}
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

		let aborted = false;
		await handlers.get("before_provider_request")(
			{ payload: { model: "local" } },
			fakeContext({ abort: () => { aborted = true; } }),
		);

		assert.equal(aborted, false);
		await waitFor(() => authorizeEventAttempts === 3, "bounded authorize event retries");
	} finally {
		globalThis.fetch = originalFetch;
	}
}

{
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		const body = init.body && JSON.parse(init.body);
		calls.push({ url: String(url), body });
		if (String(url).endsWith("/v1/authorize")) {
			return Response.json({
				decision_id: "dec_delivery_error",
				outcome: "allow",
				reservation: { id: "res_delivery_error" },
				explanations: [],
				created_at: new Date().toISOString(),
			});
		}
		if (String(url).endsWith("/v1/events") && body.kind === "pi.authorize") {
			return new Response("{}", { status: 503, headers: { "content-type": "application/json" } });
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

		let aborted = false;
		await handlers.get("before_provider_request")(
			{ payload: { model: "local" } },
			fakeContext({ abort: () => { aborted = true; } }),
		);

		assert.equal(aborted, false);
		await waitFor(() => calls.some((call) => call.body.kind === "pi.delivery_error"), "delivery error event");
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
