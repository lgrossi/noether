import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";

import { NoetherClient, NoetherDeniedError, NoetherHttpError } from "../dist/index.js";

test("client calls authorize, finalize, event, and health endpoints", async () => {
	const seen = [];
	const server = http.createServer(async (request, response) => {
		const body = await readBody(request);
		seen.push({
			method: request.method,
			url: request.url,
			authorization: request.headers.authorization,
			body: body && JSON.parse(body),
		});
		response.setHeader("content-type", "application/json");
		if (request.url === "/v1/authorize") {
			response.end(JSON.stringify({
				decision_id: "decision-1",
				outcome: "allow",
				action: "allow",
				reservation: {
					id: "reservation-1",
					amount_usd: 0.12,
					currency: "USD",
					status: "active",
					created_at: "2026-05-27T00:00:00Z",
					expires_at: "2026-05-27T01:00:00Z",
				},
				explanations: [],
				created_at: "2026-05-27T00:00:00Z",
			}));
			return;
		}
		if (request.url === "/v1/reservations/reservation-1/finalize") {
			response.end(JSON.stringify({
				id: "reservation-1",
				amount_usd: 0.10,
				currency: "USD",
				status: "finalized",
				created_at: "2026-05-27T00:00:00Z",
				expires_at: "2026-05-27T01:00:00Z",
			}));
			return;
		}
		if (request.url === "/v1/events") {
			response.statusCode = 202;
			response.end(JSON.stringify({ accepted: true }));
			return;
		}
		if (request.url === "/health") {
			response.end(JSON.stringify({
				status: "ok",
				decision_mode: "dry_run",
				policy_loaded: true,
				upstream_configured: false,
				route_count: 0,
			}));
			return;
		}
		response.statusCode = 404;
		response.end(JSON.stringify({ error: "not found" }));
	});
	const baseUrl = await listen(server);
	try {
		const client = new NoetherClient({ url: baseUrl, timeoutMs: 500, apiKey: "secret-token" });
		const decision = await client.authorize({
			project: "noether",
			subject: "user:local",
			provider: "openai",
			model: "gpt-4.1",
			metadata: { harness: "test" },
		});
		assert.equal(decision.outcome, "allow");
		assert.equal(decision.reservation?.id, "reservation-1");

		const reservation = await client.finalize("reservation-1", {
			actual_cost_usd: 0.10,
			metadata: { trace_id: "trace-1" },
		});
		assert.equal(reservation.status, "finalized");

		assert.deepEqual(await client.event({ kind: "tool.observed", payload: { name: "bash" } }), { accepted: true });
		assert.equal((await client.health()).status, "ok");
		assert.deepEqual(seen.map((item) => [item.method, item.url]), [
			["POST", "/v1/authorize"],
			["POST", "/v1/reservations/reservation-1/finalize"],
			["POST", "/v1/events"],
			["GET", "/health"],
		]);
		assert.equal(seen.every((item) => item.authorization === "Bearer secret-token"), true);
	} finally {
		await close(server);
	}
});

test("fail_open returns synthetic allow decision when sidecar is unavailable", async () => {
	const client = new NoetherClient({ url: "http://127.0.0.1:9", timeoutMs: 50, failMode: "fail_open" });
	const decision = await client.authorize({ project: "noether" });

	assert.equal(decision.outcome, "allow");
	assert.equal(decision.action, "allow");
	assert.equal(decision.explanations[0].rule_id, "sdk.sidecar_unavailable");
});

test("fail_open does not synthesize allow decisions for auth failures", async () => {
	const server = http.createServer(async (_request, response) => {
		response.statusCode = 401;
		response.setHeader("content-type", "application/json");
		response.end(JSON.stringify({ error: "missing or invalid Noether API key" }));
	});
	const baseUrl = await listen(server);
	try {
		const client = new NoetherClient({
			url: baseUrl,
			timeoutMs: 500,
			failMode: "fail_open",
			apiKey: "wrong-token",
		});

		await assert.rejects(
			() => client.authorize({ project: "noether" }),
			(error) => error instanceof NoetherHttpError && error.status === 401,
		);
	} finally {
		await close(server);
	}
});

test("fail_open does not synthesize allow decisions for sidecar HTTP errors", async () => {
	const server = http.createServer(async (_request, response) => {
		response.statusCode = 500;
		response.setHeader("content-type", "application/json");
		response.end(JSON.stringify({ error: "internal server error" }));
	});
	const baseUrl = await listen(server);
	try {
		const client = new NoetherClient({
			url: baseUrl,
			timeoutMs: 500,
			failMode: "fail_open",
		});

		await assert.rejects(
			() => client.authorize({ project: "noether" }),
			(error) => error instanceof NoetherHttpError && error.status === 500,
		);
	} finally {
		await close(server);
	}
});

test("fail_open does not synthesize allow decisions for malformed success responses", async () => {
	const server = http.createServer(async (_request, response) => {
		response.statusCode = 200;
		response.setHeader("content-type", "application/json");
		response.end("{not-json");
	});
	const baseUrl = await listen(server);
	try {
		const client = new NoetherClient({
			url: baseUrl,
			timeoutMs: 500,
			failMode: "fail_open",
		});

		await assert.rejects(
			() => client.authorize({ project: "noether" }),
			SyntaxError,
		);
	} finally {
		await close(server);
	}
});

test("fail_closed returns synthetic deny decision and withDecision blocks work", async () => {
	const client = new NoetherClient({ url: "http://127.0.0.1:9", timeoutMs: 50, failMode: "fail_closed" });
	const decision = await client.authorize({ project: "noether" });

	assert.equal(decision.outcome, "deny");
	assert.equal(decision.action, "block");
	let called = false;
	await assert.rejects(
		() => client.withDecision({ project: "noether" }, () => {
			called = true;
		}),
		NoetherDeniedError,
	);
	assert.equal(called, false);
});

function readBody(request) {
	return new Promise((resolve, reject) => {
		let body = "";
		request.setEncoding("utf8");
		request.on("data", (chunk) => {
			body += chunk;
		});
		request.on("end", () => resolve(body));
		request.on("error", reject);
	});
}

function listen(server) {
	return new Promise((resolve) => {
		server.listen(0, "127.0.0.1", () => {
			const address = server.address();
			resolve(`http://${address.address}:${address.port}`);
		});
	});
}

function close(server) {
	return new Promise((resolve, reject) => {
		server.close((error) => error ? reject(error) : resolve());
	});
}
