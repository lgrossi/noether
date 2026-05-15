declare const process: {
	env: Record<string, string | undefined>;
};

const DEFAULT_NOETHER_URL = "http://127.0.0.1:4040";
const EXTENSION_NAME = "noether-pi";
const DEFAULT_FAIL_MODE = "fail_open";

type FailMode = "fail_open" | "fail_closed";

type ExtensionAPI = {
	on(event: string, handler: (event: unknown, ctx: ExtensionContext) => unknown | Promise<unknown>): void;
};

type ExtensionContext = {
	cwd?: string;
	model?: Record<string, unknown>;
	signal?: AbortSignal;
	getContextUsage?: () => Record<string, unknown> | undefined;
	abort?: () => void;
};

type NoetherConfig = {
	noetherUrl: string;
	project?: string;
	subject?: string;
	failMode: FailMode;
	includeBody: boolean;
	version: string;
};

type ActiveRequest = {
	traceId: string;
	startedAt: number;
	decisionId?: string;
	reservationId?: string;
	outcome?: string;
};

type AuthorizeDecision = {
	decision_id?: string;
	outcome?: string;
	reservation?: {
		id?: string;
	};
};

type Usage = {
	provider?: string;
	model?: string;
	input_tokens?: number;
	output_tokens?: number;
	total_tokens?: number;
	cost_usd?: number;
	stop_reason?: string;
};

export function extensionConfig(env: Record<string, string | undefined> = process.env): NoetherConfig {
	return {
		noetherUrl: stripTrailingSlash(env.NOET_URL || DEFAULT_NOETHER_URL),
		project: emptyToUndefined(env.NOET_PI_PROJECT),
		subject: emptyToUndefined(env.NOET_PI_SUBJECT),
		failMode: env.NOET_PI_FAIL_MODE === "fail_closed" ? "fail_closed" : DEFAULT_FAIL_MODE,
		includeBody: env.NOET_PI_INCLUDE_BODY === "1" || env.NOET_PI_INCLUDE_BODY === "true",
		version: env.NOET_PI_EXTENSION_VERSION || "dev",
	};
}

function stripTrailingSlash(value: string): string {
	return value.endsWith("/") ? value.slice(0, -1) : value;
}

function emptyToUndefined(value: string | undefined): string | undefined {
	return value && value.trim() ? value : undefined;
}

export function buildAuthorizeRequest(
	event: { payload?: unknown },
	ctx: ExtensionContext,
	config: NoetherConfig = extensionConfig(),
) {
	const model = isRecord(ctx.model) ? ctx.model : {};
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

export function summarizePayload(payload: Record<string, unknown>, includeBody: boolean): Record<string, unknown> {
	const summary: Record<string, unknown> = {};
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

function isPromptLikeKey(key: string): boolean {
	return ["input", "instructions", "messages", "prompt", "system"].includes(key);
}

function summarizeValue(value: unknown): unknown {
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

function sanitizeForMetadata(value: unknown, depth: number): unknown {
	if (depth <= 0) {
		return summarizeValue(value);
	}
	if (Array.isArray(value)) {
		return value.slice(0, 20).map((item) => sanitizeForMetadata(item, depth - 1));
	}
	if (!isRecord(value)) {
		return summarizeValue(value);
	}
	const output: Record<string, unknown> = {};
	for (const [key, nested] of Object.entries(value)) {
		if (isPromptLikeKey(key) || isSensitiveKey(key)) {
			output[key] = summarizeValue(nested);
		} else {
			output[key] = sanitizeForMetadata(nested, depth - 1);
		}
	}
	return output;
}

function isSensitiveKey(key: string): boolean {
	return /token|secret|key|authorization|cookie|credential/i.test(key);
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function stringValue(value: unknown): string | undefined {
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
	return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function integerValue(value: unknown): number | undefined {
	return Number.isInteger(value) && Number(value) >= 0 ? Number(value) : undefined;
}

function dropUndefined<T extends Record<string, unknown>>(value: T): Partial<T> {
	return Object.fromEntries(Object.entries(value).filter(([, nested]) => nested !== undefined)) as Partial<T>;
}

async function authorize(noetherUrl: string, request: unknown, signal?: AbortSignal): Promise<AuthorizeDecision> {
	const response = await fetch(`${noetherUrl}/v1/authorize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify(request),
		signal,
	});
	if (!response.ok) {
		throw new Error(`Noether authorize returned ${response.status}`);
	}
	return (await response.json()) as AuthorizeDecision;
}

export function shouldAbortForDecision(decision: AuthorizeDecision | undefined): boolean {
	return decision?.outcome === "deny";
}

function buildTraceEvent(kind: string, payload: Record<string, unknown>, activeRequest: ActiveRequest | undefined) {
	return {
		trace_id: activeRequest?.traceId,
		kind,
		payload: dropUndefined({
			source: EXTENSION_NAME,
			decision_id: activeRequest?.decisionId,
			reservation_id: activeRequest?.reservationId,
			...payload,
		}),
	};
}

async function postEvent(noetherUrl: string, event: unknown, signal?: AbortSignal): Promise<void> {
	await fetch(`${noetherUrl}/v1/events`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify(event),
		signal,
	});
}

async function finalizeReservation(
	noetherUrl: string,
	reservationId: string,
	usage: Usage,
	signal?: AbortSignal,
): Promise<void> {
	await fetch(`${noetherUrl}/v1/reservations/${encodeURIComponent(reservationId)}/finalize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({
			reservation_id: reservationId,
			usage,
			actual_cost_usd: usage.cost_usd,
		}),
		signal,
	});
}

export function extractUsage(message: unknown): Usage | undefined {
	if (!isRecord(message) || message.role !== "assistant" || !isRecord(message.usage)) {
		return undefined;
	}
	const usage = message.usage;
	return dropUndefined({
		provider: stringValue(message.provider),
		model: stringValue(message.model),
		input_tokens: integerValue(usage.input),
		output_tokens: integerValue(usage.output),
		total_tokens: integerValue(usage.totalTokens),
		cost_usd: isRecord(usage.cost) ? numberValue(usage.cost.total) : undefined,
		stop_reason: stringValue(message.stopReason),
	});
}

function makeTraceId(): string {
	if (globalThis.crypto && typeof globalThis.crypto.randomUUID === "function") {
		return globalThis.crypto.randomUUID();
	}
	return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export default function registerNoetherExtension(pi: ExtensionAPI, config: NoetherConfig = extensionConfig()): void {
	let activeRequest: ActiveRequest | undefined;
	const completedReservations = new Set<string>();

	async function safePostEvent(kind: string, payload: Record<string, unknown>, ctx: ExtensionContext): Promise<void> {
		try {
			await postEvent(config.noetherUrl, buildTraceEvent(kind, payload, activeRequest), ctx.signal);
		} catch {
			// Event reporting must not change Pi's provider behavior.
		}
	}

	pi.on("before_provider_request", async (event, ctx) => {
		const request = buildAuthorizeRequest(isRecord(event) ? event : {}, ctx, config);
		const traceId = makeTraceId();
		activeRequest = {
			traceId,
			startedAt: Date.now(),
		};

		try {
			const decision = await authorize(config.noetherUrl, request, ctx.signal);
			activeRequest.decisionId = decision.decision_id;
			activeRequest.reservationId = decision.reservation?.id;
			activeRequest.outcome = decision.outcome;
			if (shouldAbortForDecision(decision)) {
				ctx.abort?.();
			}
			await safePostEvent("pi.authorize", { request, outcome: decision.outcome }, ctx);
		} catch (error) {
			if (config.failMode === "fail_closed") {
				ctx.abort?.();
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
				status: isRecord(event) ? event.status : undefined,
				headers: isRecord(event) ? sanitizeHeaders(event.headers) : undefined,
				latency_ms: activeRequest ? Date.now() - activeRequest.startedAt : undefined,
			},
			ctx,
		);
	});

	pi.on("message_end", async (event, ctx) => {
		const usage = extractUsage(isRecord(event) ? event.message : undefined);
		if (!usage) {
			return;
		}
		await safePostEvent("pi.message_end", { usage }, ctx);
		if (!activeRequest?.reservationId) {
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
			{
				turn_index: isRecord(event) ? event.turnIndex : undefined,
				usage: extractUsage(isRecord(event) ? event.message : undefined),
			},
			ctx,
		);
	});

	pi.on("agent_end", async (event, ctx) => {
		const messages = isRecord(event) && Array.isArray(event.messages) ? event.messages : [];
		await safePostEvent("pi.agent_end", { message_count: messages.length }, ctx);
	});
}

function sanitizeHeaders(headers: unknown): Record<string, unknown> {
	const output: Record<string, unknown> = {};
	if (!isRecord(headers)) {
		return output;
	}
	for (const [key, value] of Object.entries(headers)) {
		output[key] = isSensitiveKey(key) ? "[redacted]" : value;
	}
	return output;
}
