import assert from "node:assert/strict";
import test from "node:test";

import { NoetherOpenCode, opencodeConfig, summarizeValue } from "../noether-opencode.mjs";

test("configuration derives local project and sidecar URL", () => {
	const config = opencodeConfig(
		{ directory: "/repo/noether", project: { name: "repo-project" } },
		{
			NOET_OPENCODE_URL: "http://127.0.0.1:4051/",
			NOET_OPENCODE_TIMEOUT_MS: "250",
			NOET_OPENCODE_SUBJECT: "user:local",
		},
	);

	assert.equal(config.noetherUrl, "http://127.0.0.1:4051");
	assert.equal(config.timeoutMs, 250);
	assert.equal(config.project, "repo-project");
	assert.equal(config.subject, "user:local");
});

test("summarizer keeps prompt and tool data shape-only by default", () => {
	const summary = summarizeValue({
		messages: [{ role: "user", content: "secret prompt" }],
		tool: "bash",
		args: { command: "rm -rf private" },
		ok: true,
	});

	assert.deepEqual(summary.messages, { type: "array", length: 1, item_types: { user: 1 } });
	assert.deepEqual(summary.args, { type: "object", keys: ["command"] });
	assert.equal(summary.ok, true);
	assert.equal(JSON.stringify(summary).includes("secret prompt"), false);
	assert.equal(JSON.stringify(summary).includes("rm -rf private"), false);
});

test("plugin posts generic and tool events without blocking OpenCode", async () => {
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: JSON.parse(init.body) });
		return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
	};
	try {
		const plugin = await NoetherOpenCode({
			directory: "/repo/noether",
			project: { name: "noether" },
		});

		await plugin.event({ event: { type: "session.created", properties: { id: "session-1" } } });
		await plugin["tool.execute.before"](
			{ tool: "bash", sessionID: "session-1", callID: "call-1" },
			{ args: { command: "echo private" } },
		);
		await plugin["tool.execute.after"](
			{ tool: "bash", sessionID: "session-1", callID: "call-1" },
			{ result: "private output" },
		);

		assert.deepEqual(calls.map((call) => call.body.kind), [
			"opencode.session.created",
			"opencode.tool_execute_before",
			"opencode.tool_execute_after",
		]);
		assert(calls.every((call) => call.url.endsWith("/v1/events")));
		assert.equal(calls[1].body.payload.tool, "bash");
		assert.equal(JSON.stringify(calls).includes("echo private"), false);
		assert.equal(JSON.stringify(calls).includes("private output"), false);
	} finally {
		globalThis.fetch = originalFetch;
	}
});

test("delivery failure is swallowed so OpenCode behavior is not changed", async () => {
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async () => new Response("down", { status: 503 });
	try {
		const plugin = await NoetherOpenCode({ directory: "/repo/noether" });
		await plugin.event({ event: { type: "session.idle" } });
	} finally {
		globalThis.fetch = originalFetch;
	}
});
