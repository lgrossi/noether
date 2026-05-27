import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import test from "node:test";

import {
	buildAuthorizeRequest,
	codexConfig,
	codexEventKind,
	codexExecArgs,
	extractUsage,
	modelFromArgs,
	runCodex,
} from "../noether-codex.mjs";

test("config and authorize request model Codex as harness not provider", () => {
	const config = codexConfig(
		["--model", "gpt-5.5", "fix tests"],
		{
			NOET_CODEX_URL: "http://127.0.0.1:4051/",
			NOET_CODEX_PROVIDER: "openai",
			NOET_CODEX_SUBJECT: "alice",
		},
		"/repo/noether",
	);
	const request = buildAuthorizeRequest(config, ["--model", "gpt-5.5", "fix tests"]);

	assert.equal(config.noetherUrl, "http://127.0.0.1:4051");
	assert.equal(request.provider, "openai");
	assert.equal(request.model, "gpt-5.5");
	assert.equal(request.metadata.harness, "codex");
	assert.equal(request.metadata.integration, "noether-codex");
	assert.equal(request.metadata.provider_known, true);
	assert.notEqual(request.provider, "codex");
	assert.deepEqual(request.entities, ["project:noether", "user:alice"]);
});

test("wrapper normalizes arguments to codex exec json", () => {
	assert.deepEqual(codexExecArgs(["--model", "gpt-5.5", "hello"]), ["exec", "--json", "--model", "gpt-5.5", "hello"]);
	assert.deepEqual(codexExecArgs(["exec", "--json", "hello"]), ["exec", "--json", "hello"]);
	assert.equal(modelFromArgs(["-m", "gpt-5.5"]), "gpt-5.5");
	assert.equal(modelFromArgs(["--model=gpt-5.5"]), "gpt-5.5");
});

test("extracts usage only when Codex event exposes usage", () => {
	assert.deepEqual(extractUsage({
		type: "turn.completed",
		model: "gpt-5.5",
		usage: { input_tokens: 10, output_tokens: 20, total_tokens: 30 },
		cost_usd: 0.12,
		stop_reason: "stop",
	}), {
		model: "gpt-5.5",
		input_tokens: 10,
		output_tokens: 20,
		total_tokens: 30,
		cost_usd: 0.12,
		stop_reason: "stop",
	});
	assert.equal(extractUsage({ type: "started" }), undefined);
	assert.equal(codexEventKind({ type: "turn.completed" }), "codex.turn.completed");
});

test("denied authorization does not spawn codex", async () => {
	const calls = [];
	const originalFetch = globalThis.fetch;
	globalThis.fetch = async (url, init) => {
		calls.push({ url: String(url), body: init.body && JSON.parse(init.body) });
		return Response.json({
			decision_id: "decision-1",
			outcome: "deny",
			action: "block",
			explanations: [{ reason: "budget exceeded" }],
			created_at: "2026-05-27T00:00:00Z",
		});
	};
	try {
		let stderr = "";
		const result = await runCodex(
			["--model", "gpt-5.5", "hello"],
			{ NOET_CODEX_PROVIDER: "openai" },
			{ stdout: { write() {} }, stderr: { write(chunk) { stderr += chunk; } } },
		);

		assert.equal(result.spawned, false);
		assert.equal(result.exitCode, 3);
		assert.match(stderr, /budget exceeded/);
		assert(calls[0].url.endsWith("/v1/authorize"));
	} finally {
		globalThis.fetch = originalFetch;
	}
});

test("allowed run spawns codex, forwards events, and finalizes observed usage", async () => {
	const tempdir = await mkdtemp(resolve(tmpdir(), "noether-codex-"));
	const fakeCodex = resolve(tempdir, "codex");
	await writeFile(
		fakeCodex,
		`#!/usr/bin/env node
console.log(JSON.stringify({type:"run.started"}));
console.log(JSON.stringify({type:"turn.completed",model:"gpt-5.5",usage:{input_tokens:1,output_tokens:2,total_tokens:3},cost_usd:0.01}));
`,
		{ mode: 0o755 },
	);
	const calls = [];
	const originalFetch = globalThis.fetch;
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
		let stdout = "";
		const result = await runCodex(
			["--model", "gpt-5.5", "hello"],
			{ NOET_CODEX_BIN: fakeCodex, NOET_CODEX_PROVIDER: "openai" },
			{ stdout: { write(chunk) { stdout += chunk; } }, stderr: { write() {} } },
		);

		assert.equal(result.spawned, true);
		assert.equal(result.exitCode, 0);
		assert.match(stdout, /turn.completed/);
		assert(calls.some((call) => call.url.endsWith("/v1/events") && call.body.kind === "codex.run.started"));
		assert(calls.some((call) => call.url.endsWith("/v1/events") && call.body.kind === "codex.turn.completed"));
		const finalize = calls.find((call) => call.url.includes("/v1/reservations/reservation-1/finalize"));
		assert.equal(finalize.body.actual_cost_usd, 0.01);
		assert.equal(finalize.body.usage.input_tokens, 1);
		assert.equal(finalize.body.metadata.harness, "codex");
	} finally {
		globalThis.fetch = originalFetch;
	}
});
