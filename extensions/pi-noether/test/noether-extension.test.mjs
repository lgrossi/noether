import assert from "node:assert/strict";
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
		assert.equal(JSON.stringify(calls).includes("do not send"), false);
	} finally {
		globalThis.fetch = originalFetch;
	}
}

console.log("pi-noether extension tests ok");
