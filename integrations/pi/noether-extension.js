const DEFAULT_NOETHER_URL = "http://127.0.0.1:4040";
const EXTENSION_NAME = "noether-pi";
const DEFAULT_FAIL_MODE = "fail_open";

function extensionConfig(env = process.env) {
	return {
		noetherUrl: stripTrailingSlash(env.NOET_URL || DEFAULT_NOETHER_URL),
		project: emptyToUndefined(env.NOET_PI_PROJECT),
		subject: emptyToUndefined(env.NOET_PI_SUBJECT),
		failMode: env.NOET_PI_FAIL_MODE === "fail_closed" ? "fail_closed" : DEFAULT_FAIL_MODE,
		includeBody: env.NOET_PI_INCLUDE_BODY === "1" || env.NOET_PI_INCLUDE_BODY === "true",
		version: env.NOET_PI_EXTENSION_VERSION || "dev",
	};
}

function stripTrailingSlash(value) {
	return value.endsWith("/") ? value.slice(0, -1) : value;
}

function emptyToUndefined(value) {
	return value && value.trim() ? value : undefined;
}

function buildAuthorizeRequest(event, ctx, config = extensionConfig()) {
	const model = ctx.model || {};
	const payload = isRecord(event.payload) ? event.payload : {};
	const contextUsage = typeof ctx.getContextUsage === "function" ? ctx.getContextUsage() : undefined;
	const provider = stringValue(model.provider) || stringValue(payload.provider);
	const modelId = stringValue(model.id) || stringValue(model.model) || stringValue(payload.model);
	const metadata = {
		harness: "pi",
		extension: EXTENSION_NAME,
		extension_version: config.version,
		cwd: ctx.cwd,
		model_api: stringValue(model.api),
		payload_kind: Array.isArray(event.payload) ? "array" : typeof event.payload,
		payload_keys: Object.keys(payload).sort(),
		payload_summary: summarizePayload(payload, config.includeBody),
		context_window: numberValue(contextUsage && contextUsage.contextWindow),
		context_usage_percent: numberValue(contextUsage && contextUsage.percent),
	};

	return {
		subject: config.subject,
		project: config.project,
		provider,
		model: modelId,
		estimated_tokens: integerValue(contextUsage && contextUsage.tokens),
		metadata: dropUndefined(metadata),
	};
}

function summarizePayload(payload, includeBody) {
	const summary = {};
	for (const [key, value] of Object.entries(payload)) {
		if (!includeBody && isPromptLikeKey(key)) {
			summary[key] = summarizeValue(value);
			continue;
		}
		if (includeBody) {
			summary[key] = sanitizeForMetadata(value, 2);
			continue;
		}
		summary[key] = summarizeValue(value);
	}
	return summary;
}

function isPromptLikeKey(key) {
	return [
		"input",
		"instructions",
		"messages",
		"prompt",
		"system",
	].includes(key);
}

function summarizeValue(value) {
	if (Array.isArray(value)) {
		return { type: "array", length: value.length };
	}
	if (isRecord(value)) {
		return { type: "object", keys: Object.keys(value).sort() };
	}
	if (typeof value === "string") {
		return { type: "string", length: value.length };
	}
	if (typeof value === "number" || typeof value === "boolean" || value === null) {
		return value;
	}
	return { type: typeof value };
}

function sanitizeForMetadata(value, depth) {
	if (depth <= 0) {
		return summarizeValue(value);
	}
	if (Array.isArray(value)) {
		return value.slice(0, 20).map((item) => sanitizeForMetadata(item, depth - 1));
	}
	if (!isRecord(value)) {
		return summarizeValue(value);
	}
	const output = {};
	for (const [key, nested] of Object.entries(value)) {
		if (isPromptLikeKey(key) || isSensitiveKey(key)) {
			output[key] = summarizeValue(nested);
		} else {
			output[key] = sanitizeForMetadata(nested, depth - 1);
		}
	}
	return output;
}

function isSensitiveKey(key) {
	return /token|secret|key|authorization|cookie|credential/i.test(key);
}

function isRecord(value) {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function stringValue(value) {
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function numberValue(value) {
	return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function integerValue(value) {
	return Number.isInteger(value) && value >= 0 ? value : undefined;
}

function dropUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, nested]) => nested !== undefined));
}

async function authorize(noetherUrl, request, signal) {
	const response = await fetch(`${noetherUrl}/v1/authorize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify(request),
		signal,
	});
	if (!response.ok) {
		throw new Error(`Noether authorize returned ${response.status}`);
	}
	return response.json();
}

function shouldAbortForDecision(decision) {
	return decision && decision.outcome === "deny";
}

function buildTraceEvent(kind, payload, activeRequest) {
	return {
		trace_id: activeRequest && activeRequest.traceId,
		kind,
		payload: dropUndefined({
			source: EXTENSION_NAME,
			decision_id: activeRequest && activeRequest.decisionId,
			reservation_id: activeRequest && activeRequest.reservationId,
			...payload,
		}),
	};
}

async function postEvent(noetherUrl, event, signal) {
	await fetch(`${noetherUrl}/v1/events`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify(event),
		signal,
	});
}

async function finalizeReservation(noetherUrl, reservationId, usage, signal) {
	await fetch(`${noetherUrl}/v1/reservations/${encodeURIComponent(reservationId)}/finalize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({
			reservation_id: reservationId,
			usage,
			actual_cost_usd: usage && usage.cost_usd,
		}),
		signal,
	});
}

function extractUsage(message) {
	if (!message || message.role !== "assistant" || !message.usage) {
		return undefined;
	}
	const usage = message.usage;
	return dropUndefined({
		provider: stringValue(message.provider),
		model: stringValue(message.model),
		input_tokens: integerValue(usage.input),
		output_tokens: integerValue(usage.output),
		total_tokens: integerValue(usage.totalTokens),
		cost_usd: numberValue(usage.cost && usage.cost.total),
		stop_reason: stringValue(message.stopReason),
	});
}

function makeTraceId() {
	if (globalThis.crypto && typeof globalThis.crypto.randomUUID === "function") {
		return globalThis.crypto.randomUUID();
	}
	return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function registerNoetherExtension(pi, config = extensionConfig()) {
	let activeRequest;
	const completedReservations = new Set();

	async function safePostEvent(kind, payload, ctx) {
		try {
			await postEvent(
				config.noetherUrl,
				buildTraceEvent(kind, payload, activeRequest),
				ctx && ctx.signal,
			);
		} catch {
			// Event reporting must not change Pi's provider behavior.
		}
	}

	pi.on("before_provider_request", async (event, ctx) => {
		const request = buildAuthorizeRequest(event, ctx, config);
		const traceId = makeTraceId();
		activeRequest = {
			traceId,
			startedAt: Date.now(),
			decisionId: undefined,
			reservationId: undefined,
			outcome: undefined,
		};

		try {
			const decision = await authorize(config.noetherUrl, request, ctx.signal);
			activeRequest.decisionId = decision.decision_id;
			activeRequest.reservationId = decision.reservation && decision.reservation.id;
			activeRequest.outcome = decision.outcome;
			if (shouldAbortForDecision(decision)) {
				ctx.abort();
			}
			await safePostEvent("pi.authorize", { request, outcome: decision.outcome }, ctx);
		} catch (error) {
			if (config.failMode === "fail_closed") {
				ctx.abort();
			}
			await safePostEvent(
				"pi.authorize_error",
				{ error: error instanceof Error ? error.message : String(error), fail_mode: config.failMode },
				ctx,
			);
		}
	});

	pi.on("after_provider_response", async (event, ctx) => {
		await safePostEvent(
			"pi.provider_response",
			{
				status: event.status,
				headers: sanitizeHeaders(event.headers),
				latency_ms: activeRequest ? Date.now() - activeRequest.startedAt : undefined,
			},
			ctx,
		);
	});

	pi.on("message_end", async (event, ctx) => {
		const usage = extractUsage(event.message);
		if (!usage) {
			return;
		}
		await safePostEvent("pi.message_end", { usage }, ctx);
		if (!activeRequest || !activeRequest.reservationId) {
			return;
		}
		if (completedReservations.has(activeRequest.reservationId)) {
			return;
		}
		try {
			await finalizeReservation(config.noetherUrl, activeRequest.reservationId, usage, ctx.signal);
			completedReservations.add(activeRequest.reservationId);
		} catch {
			await safePostEvent("pi.reservation_finalize_error", { usage }, ctx);
		}
	});

	pi.on("turn_end", async (event, ctx) => {
		await safePostEvent(
			"pi.turn_end",
			{ turn_index: event.turnIndex, usage: extractUsage(event.message) },
			ctx,
		);
	});

	pi.on("agent_end", async (event, ctx) => {
		await safePostEvent("pi.agent_end", { message_count: event.messages.length }, ctx);
	});
}

function sanitizeHeaders(headers) {
	const output = {};
	for (const [key, value] of Object.entries(headers || {})) {
		output[key] = isSensitiveKey(key) ? "[redacted]" : value;
	}
	return output;
}

module.exports = registerNoetherExtension;
module.exports.buildAuthorizeRequest = buildAuthorizeRequest;
module.exports.extractUsage = extractUsage;
module.exports.extensionConfig = extensionConfig;
module.exports.shouldAbortForDecision = shouldAbortForDecision;
module.exports.summarizePayload = summarizePayload;
