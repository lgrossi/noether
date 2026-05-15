import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "../../..");
const extension = await import(resolve(repoRoot, ".noet/build/pi-noether/index.js"));
const proofDir = resolve(repoRoot, ".noet/proofs/pi-extension-deny");

await mkdir(proofDir, { recursive: true });

const providerRequests = [];
const provider = createServer((req, res) => {
	providerRequests.push({ method: req.method, url: req.url });
	req.resume();
	res.writeHead(200, { "content-type": "application/json" });
	res.end(JSON.stringify({ ok: true }));
});
await listen(provider);

const authorizations = [];
const events = [];
const noether = createServer((req, res) => {
	if (req.method === "GET" && req.url === "/health") {
		res.writeHead(200, { "content-type": "text/plain" });
		res.end("ok");
		return;
	}
	if (req.method === "POST" && req.url === "/v1/authorize") {
		readJson(req).then((body) => {
			authorizations.push(body);
			res.writeHead(200, { "content-type": "application/json" });
			res.end(JSON.stringify({
				decision_id: "proof-deny",
				outcome: "deny",
				explanations: [],
				created_at: new Date().toISOString(),
			}));
		});
		return;
	}
	if (req.method === "POST" && req.url === "/v1/events") {
		readJson(req).then((body) => {
			events.push(body);
			res.writeHead(202, { "content-type": "application/json" });
			res.end(JSON.stringify({ accepted: true }));
		});
		return;
	}
	req.resume();
	res.writeHead(404);
	res.end();
});
await listen(noether);

try {
	const providerUrl = `http://127.0.0.1:${provider.address().port}/v1/chat/completions`;
	const noetherUrl = `http://127.0.0.1:${noether.address().port}`;
	const handlers = new Map();
	let aborted = false;

	extension.default(
		{
			on(event, handler) {
				handlers.set(event, handler);
			},
		},
		{
			noetherUrl,
			project: "noether-proof",
			subject: "local-proof",
			failMode: "fail_closed",
			includeBody: false,
			version: "proof",
		},
	);

	const payload = {
		model: "noether-proof-model",
		messages: [{ role: "user", content: "local deny proof; this prompt must not reach Noether" }],
		stream: true,
	};
	const ctx = {
		cwd: repoRoot,
		model: {
			provider: "noether-proof",
			id: "noether-proof-model",
			api: "openai-completions",
		},
		getContextUsage() {
			return { tokens: 42, contextWindow: 4096, percent: 1.03 };
		},
		get signal() {
			return undefined;
		},
		abort() {
			aborted = true;
		},
	};

	await handlers.get("before_provider_request")({ payload }, ctx);
	await waitFor(() => events.some((event) => event.kind === "pi.authorize"), "pi.authorize event");

	if (!aborted) {
		await fetch(providerUrl, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(payload),
		});
	}

	await writeFile(
		resolve(proofDir, "summary.json"),
		JSON.stringify({
			authorizations: authorizations.length,
			events: events.map((event) => event.kind),
			providerRequests: providerRequests.length,
			aborted,
		}, null, 2),
	);

	assert.equal(aborted, true);
	assert.equal(authorizations.length, 1);
	assert.equal(providerRequests.length, 0);
	assert.equal(events.some((event) => event.kind === "pi.authorize"), true);
	assert.equal(JSON.stringify(authorizations).includes("local deny proof"), false);

	console.log("proof ok: deny decision aborted before mock provider request");
} finally {
	provider.close();
	noether.close();
}

function listen(server) {
	return new Promise((resolveListen, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", () => {
			server.off("error", reject);
			resolveListen();
		});
	});
}

function readJson(req) {
	return new Promise((resolveRead, reject) => {
		const chunks = [];
		req.on("data", (chunk) => chunks.push(chunk));
		req.on("end", () => {
			try {
				resolveRead(JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}"));
			} catch (error) {
				reject(error);
			}
		});
		req.on("error", reject);
	});
}

async function waitFor(predicate, label) {
	for (let attempt = 0; attempt < 20; attempt += 1) {
		if (predicate()) {
			return;
		}
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
	assert.fail(`timed out waiting for ${label}`);
}
