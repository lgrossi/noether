// @ts-ignore: Pi runs extensions in Node; this package intentionally avoids a local @types/node dependency.
import { appendFile, mkdir } from "node:fs/promises";

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
	hookLogDir?: string;
};

type ActiveRequest = {
	traceId: string;
	requestId: string;
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

type MutableAuthorizeRequest = ReturnType<typeof buildAuthorizeRequest> & {
	metadata?: Record<string, unknown>;
};

export function extensionConfig(env: Record<string, string | undefined> = process.env): NoetherConfig {
	return {
		noetherUrl: stripTrailingSlash(env.NOET_URL || DEFAULT_NOETHER_URL),
		project: emptyToUndefined(env.NOET_PI_PROJECT),
		subject: emptyToUndefined(env.NOET_PI_SUBJECT),
		failMode: env.NOET_PI_FAIL_MODE === "fail_closed" ? "fail_closed" : DEFAULT_FAIL_MODE,
		includeBody: env.NOET_PI_INCLUDE_BODY === "1" || env.NOET_PI_INCLUDE_BODY === "true",
		version: env.NOET_PI_EXTENSION_VERSION || "dev",
		hookLogDir: emptyToUndefined(env.NOET_PI_HOOK_LOG_DIR),
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
	correlation: { traceId?: string; requestId?: string } = {},
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
		trace_id: correlation.traceId,
		request_id: correlation.requestId,
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

function summarizeObject(value: unknown): unknown {
	if (!isRecord(value)) {
		return summarizeValue(value);
	}
	return Object.fromEntries(Object.entries(value).map(([key, nested]) => [key, summarizeValue(nested)]));
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

function summarizeNames(value: unknown): unknown {
	if (!Array.isArray(value)) {
		return undefined;
	}
	return value.slice(0, 50).map((item) => {
		if (typeof item === "string") {
			return item;
		}
		if (isRecord(item)) {
			return stringValue(item.name) || stringValue(item.id) || stringValue(item.path) || summarizeValue(item);
		}
		return summarizeValue(item);
	});
}

function summarizeAgentContext(event: unknown): Record<string, unknown> {
	const eventRecord = isRecord(event) ? event : {};
	const options = isRecord(eventRecord.systemPromptOptions) ? eventRecord.systemPromptOptions : {};
	return dropUndefined({
		prompt: summarizeValue(eventRecord.prompt),
		images: summarizeValue(eventRecord.images),
		selected_tools: summarizeNames(options.selectedTools),
		tool_snippets: summarizeObject(options.toolSnippets),
		skills: summarizeNames(options.skills),
		context_files: summarizeNames(options.contextFiles),
		cwd: stringValue(options.cwd),
	});
}

function summarizeToolMetadata(event: unknown): Record<string, unknown> {
	const record = isRecord(event) ? event : {};
	return dropUndefined({
		tool_call_id: stringValue(record.toolCallId),
		input_summary: summarizeObject(record.input),
		content_summary: summarizeValue(record.content),
		details_summary: summarizeObject(record.details),
	});
}

function hookLogPath(
	config: NoetherConfig,
	hook:
		| "session_start"
		| "before_provider_request"
		| "after_provider_response"
		| "message_update"
		| "message_end"
		| "turn_end"
		| "agent_end",
): string | undefined {
	if (!config.hookLogDir) {
		return undefined;
	}
	if (hook === "message_update" || hook === "message_end" || hook === "turn_end" || hook === "agent_end") {
		return `${config.hookLogDir.replace(/\/+$/, "")}/after_provider_response.jsonl`;
	}
	return `${config.hookLogDir.replace(/\/+$/, "")}/${hook}.jsonl`;
}

async function writeHookLog(
	config: NoetherConfig,
	hook:
		| "session_start"
		| "before_provider_request"
		| "after_provider_response"
		| "message_update"
		| "message_end"
		| "turn_end"
		| "agent_end",
	payload: Record<string, unknown>,
): Promise<void> {
	const path = hookLogPath(config, hook);
	if (!path) {
		return;
	}
	await mkdir(config.hookLogDir!, { recursive: true });
	await appendFile(
		path,
		`${JSON.stringify({
			at: new Date().toISOString(),
			hook,
			payload: safeForHookLog(payload),
		})}\n`,
		"utf8",
	);
}

async function safeWriteHookLog(
	config: NoetherConfig,
	hook:
		| "session_start"
		| "before_provider_request"
		| "after_provider_response"
		| "message_update"
		| "message_end"
		| "turn_end"
		| "agent_end",
	payload: Record<string, unknown>,
): Promise<void> {
	try {
		await writeHookLog(config, hook, payload);
	} catch (error) {
		if (config.hookLogDir) {
			console.error(
				`[noether-pi] failed to write ${hook} hook log to ${config.hookLogDir}: ${
					error instanceof Error ? error.stack || error.message : String(error)
				}`,
			);
		}
	}
}

function safeForHookLog(value: unknown, depth = 8, seen = new WeakSet<object>()): unknown {
	if (depth <= 0) {
		return summarizeValue(value);
	}
	if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
		return value;
	}
	if (typeof value === "undefined") {
		return "[undefined]";
	}
	if (typeof value === "function") {
		return "[function]";
	}
	if (Array.isArray(value)) {
		return value.map((item) => safeForHookLog(item, depth - 1, seen));
	}
	if (!isRecord(value)) {
		return summarizeValue(value);
	}
	if (seen.has(value)) {
		return "[circular]";
	}
	seen.add(value);
	const output: Record<string, unknown> = {};
	for (const [key, nested] of Object.entries(value)) {
		output[key] = safeForHookLog(nested, depth - 1, seen);
	}
	seen.delete(value);
	return output;
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
	activeRequest: ActiveRequest,
	signal?: AbortSignal,
): Promise<void> {
	await fetch(`${noetherUrl}/v1/reservations/${encodeURIComponent(reservationId)}/finalize`, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({
			reservation_id: reservationId,
			usage,
			actual_cost_usd: usage.cost_usd,
			metadata: {
				trace_id: activeRequest.traceId,
				request_id: activeRequest.requestId,
				source: EXTENSION_NAME,
			},
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
	let pendingAgentContext: Record<string, unknown> | undefined;
	const completedReservations = new Set<string>();
	const toolStartedAt = new Map<string, number>();
	safeWriteHookLog(config, "before_provider_request", {
		extension_loaded: true,
		noether_url: config.noetherUrl,
		fail_mode: config.failMode,
		version: config.version,
	});
	safeWriteHookLog(config, "after_provider_response", {
		extension_loaded: true,
		noether_url: config.noetherUrl,
		fail_mode: config.failMode,
		version: config.version,
	});

	async function safePostEvent(kind: string, payload: Record<string, unknown>, ctx: ExtensionContext): Promise<void> {
		try {
			await postEvent(config.noetherUrl, buildTraceEvent(kind, payload, activeRequest), ctx.signal);
		} catch {
			// Event reporting must not change Pi's provider behavior.
		}
	}

	pi.on("before_agent_start", (event) => {
		pendingAgentContext = summarizeAgentContext(event);
	});

	pi.on("session_start", async (event, ctx) => {
		await safeWriteHookLog(config, "session_start", {
			event,
			ctx,
			noether_url: config.noetherUrl,
			fail_mode: config.failMode,
			version: config.version,
			hook_log_dir: config.hookLogDir,
		});
	});

	pi.on("before_provider_request", async (event, ctx) => {
		const traceId = makeTraceId();
		const requestId = makeTraceId();
		const request = buildAuthorizeRequest(isRecord(event) ? event : {}, ctx, config, {
			traceId,
			requestId,
		}) as MutableAuthorizeRequest;
		if (pendingAgentContext && Object.keys(pendingAgentContext).length > 0) {
			request.metadata = {
				...request.metadata,
				agent_context: pendingAgentContext,
			};
		}
		await safeWriteHookLog(config, "before_provider_request", {
			trace_id: traceId,
			request_id: requestId,
			event,
			ctx,
			noether_authorize_request: request,
		});
		activeRequest = {
			traceId,
			requestId,
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
			if (pendingAgentContext && Object.keys(pendingAgentContext).length > 0) {
				await safePostEvent("pi.agent_context", pendingAgentContext, ctx);
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
		pendingAgentContext = undefined;
	});

	pi.on("tool_call", async (event, ctx) => {
		const eventRecord = isRecord(event) ? event : {};
		const toolCallId = stringValue(eventRecord.toolCallId);
		if (toolCallId) {
			toolStartedAt.set(toolCallId, Date.now());
		}
		await safePostEvent(
			"pi.tool_call",
			{
				tool_name: stringValue(eventRecord.toolName),
				tool_call_id: toolCallId,
				input_summary: summarizeObject(eventRecord.input),
			},
			ctx,
		);
	});

	pi.on("tool_result", async (event, ctx) => {
		const eventRecord = isRecord(event) ? event : {};
		const toolCallId = stringValue(eventRecord.toolCallId);
		const startedAt = toolCallId ? toolStartedAt.get(toolCallId) : undefined;
		if (toolCallId) {
			toolStartedAt.delete(toolCallId);
		}
		await safePostEvent(
			"tool.observed",
			{
				name: stringValue(eventRecord.toolName) || "unknown",
				duration_ms: startedAt ? Date.now() - startedAt : undefined,
				success: typeof eventRecord.isError === "boolean" ? !eventRecord.isError : undefined,
				metadata: summarizeToolMetadata(event),
			},
			ctx,
		);
	});

	pi.on("after_provider_response", async (event, ctx) => {
		await safeWriteHookLog(config, "after_provider_response", {
			trace_id: activeRequest?.traceId,
			request_id: activeRequest?.requestId,
			event,
			ctx,
			active_request: activeRequest,
		});
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

	pi.on("message_update", async (event, ctx) => {
		await safeWriteHookLog(config, "message_update", {
			trace_id: activeRequest?.traceId,
			request_id: activeRequest?.requestId,
			event,
			ctx,
			active_request: activeRequest,
		});
	});

	pi.on("message_end", async (event, ctx) => {
		await safeWriteHookLog(config, "message_end", {
			trace_id: activeRequest?.traceId,
			request_id: activeRequest?.requestId,
			event,
			ctx,
			active_request: activeRequest,
		});
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
			await finalizeReservation(config.noetherUrl, activeRequest.reservationId, usage, activeRequest, ctx.signal);
			completedReservations.add(activeRequest.reservationId);
		} catch {
			await safePostEvent("pi.reservation_finalize_error", { usage }, ctx);
		}
	});

	pi.on("turn_end", async (event, ctx) => {
		await safeWriteHookLog(config, "turn_end", {
			trace_id: activeRequest?.traceId,
			request_id: activeRequest?.requestId,
			event,
			ctx,
			active_request: activeRequest,
		});
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
		await safeWriteHookLog(config, "agent_end", {
			trace_id: activeRequest?.traceId,
			request_id: activeRequest?.requestId,
			event,
			ctx,
			active_request: activeRequest,
		});
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
