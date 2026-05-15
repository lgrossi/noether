// @ts-ignore: Pi runs extensions in Node; this package intentionally avoids a local @types/node dependency.
import { appendFile, mkdir } from "node:fs/promises";

declare const process: {
	env: Record<string, string | undefined>;
};

const DEFAULT_NOETHER_URL = "http://127.0.0.1:4040";
const EXTENSION_NAME = "noether-pi";
const DEFAULT_FAIL_MODE = "fail_open";
const DEFAULT_AUTHORIZE_TIMEOUT_MS = 1_000;
const DEFAULT_QUEUE_MAX_ITEMS = 100;

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
	authorizeTimeoutMs: number;
	queueMaxItems: number;
	debugHooks: boolean;
	debugHookLogDir?: string;
};

type ActiveRequest = {
	traceId: string;
	sessionId: string;
	agentRunId: string;
	requestId: string;
	providerCallId: string;
	startedAt: number;
	decisionId?: string;
	reservationId?: string;
	outcome?: string;
	turnId?: string;
	responseId?: string;
	streamSummary: StreamSummary;
};

type AttributionStatus = "exact" | "fallback" | "unmatched";

type AttributedProviderCall = {
	span?: ActiveRequest;
	status: AttributionStatus;
};

type StreamSummary = {
	counts: Record<string, number>;
	first_at?: string;
	last_at?: string;
	tool_calls: Record<string, { name?: string }>;
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
	cache_read_tokens?: number;
	cache_write_tokens?: number;
	input_cost_usd?: number;
	output_cost_usd?: number;
	cache_read_cost_usd?: number;
	cache_write_cost_usd?: number;
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
		authorizeTimeoutMs: positiveInteger(env.NOET_PI_AUTHORIZE_TIMEOUT_MS) || DEFAULT_AUTHORIZE_TIMEOUT_MS,
		queueMaxItems: positiveInteger(env.NOET_PI_QUEUE_MAX_ITEMS) || DEFAULT_QUEUE_MAX_ITEMS,
		debugHooks: env.NOET_PI_DEBUG_HOOKS === "raw",
		debugHookLogDir: emptyToUndefined(env.NOET_PI_DEBUG_HOOK_LOG_DIR),
	};
}

function stripTrailingSlash(value: string): string {
	return value.endsWith("/") ? value.slice(0, -1) : value;
}

function emptyToUndefined(value: string | undefined): string | undefined {
	return value && value.trim() ? value : undefined;
}

function positiveInteger(value: string | undefined): number | undefined {
	if (!value) {
		return undefined;
	}
	const parsed = Number.parseInt(value, 10);
	return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined;
}

export function buildAuthorizeRequest(
	event: { payload?: unknown },
	ctx: ExtensionContext,
	config: NoetherConfig = extensionConfig(),
	correlation: { traceId?: string; sessionId?: string; agentRunId?: string; requestId?: string; providerCallId?: string } = {},
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
		session_id: correlation.sessionId,
		agent_run_id: correlation.agentRunId,
		request_id: correlation.requestId,
		provider_call_id: correlation.providerCallId,
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
			summary[key] = summarizePayloadField(key, value);
			continue;
		}
		if (includeBody) {
			summary[key] = sanitizeForMetadata(value, 2);
			continue;
		}
		summary[key] = summarizePayloadField(key, value);
	}
	return summary;
}

function summarizePayloadField(key: string, value: unknown): unknown {
	if (key === "prompt_cache_key") {
		return { present: value !== undefined && value !== null };
	}
	if (key === "input" && Array.isArray(value)) {
		return {
			type: "array",
			length: value.length,
			item_types: histogram(value.map(inputItemType)),
		};
	}
	if (key === "tools" && Array.isArray(value)) {
		return { type: "array", length: value.length };
	}
	if ((key === "reasoning" || key === "text") && isRecord(value)) {
		return dropUndefined({
			type: "object",
			keys: Object.keys(value).sort(),
			effort: key === "reasoning" ? stringValue(value.effort) : undefined,
			verbosity: key === "text" ? stringValue(value.verbosity) : undefined,
		});
	}
	return summarizeValue(value);
}

function inputItemType(value: unknown): string {
	if (!isRecord(value)) {
		return typeof value;
	}
	return stringValue(value.type) || stringValue(value.role) || "object";
}

function histogram(values: string[]): Record<string, number> {
	const counts: Record<string, number> = {};
	for (const value of values) {
		counts[value] = (counts[value] || 0) + 1;
	}
	return counts;
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
		| "before_provider_request"
		| "message_update"
		| "message_end"
		| "turn_end"
		| "agent_end",
): string | undefined {
	if (!config.debugHooks || !config.debugHookLogDir) {
		return undefined;
	}
	return `${config.debugHookLogDir.replace(/\/+$/, "")}/${hook}.raw.jsonl`;
}

async function writeHookLog(
	config: NoetherConfig,
	hook:
		| "before_provider_request"
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
	await mkdir(config.debugHookLogDir!, { recursive: true });
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
		| "before_provider_request"
		| "message_update"
		| "message_end"
		| "turn_end"
		| "agent_end",
	payload: Record<string, unknown>,
): Promise<void> {
	try {
		await writeHookLog(config, hook, payload);
	} catch (error) {
		if (config.debugHooks && config.debugHookLogDir) {
			console.error(
				`[noether-pi] failed to write ${hook} hook log to ${config.debugHookLogDir}: ${
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

async function authorize(
	noetherUrl: string,
	request: unknown,
	timeoutMs: number,
	signal?: AbortSignal,
): Promise<AuthorizeDecision> {
	const controller = new AbortController();
	const abortFromParent = () => controller.abort(signal?.reason);
	if (signal) {
		if (signal.aborted) {
			controller.abort(signal.reason);
		} else {
			signal.addEventListener("abort", abortFromParent, { once: true });
		}
	}
	const timeout = setTimeout(
		() => controller.abort(new Error(`Noether authorize timed out after ${timeoutMs}ms`)),
		timeoutMs,
	);
	try {
		const response = await fetch(`${noetherUrl}/v1/authorize`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(request),
			signal: controller.signal,
		});
		if (!response.ok) {
			throw new Error(`Noether authorize returned ${response.status}`);
		}
		return (await response.json()) as AuthorizeDecision;
	} finally {
		clearTimeout(timeout);
		if (signal) {
			signal.removeEventListener("abort", abortFromParent);
		}
	}
}

export function shouldAbortForDecision(decision: AuthorizeDecision | undefined): boolean {
	return decision?.outcome === "deny";
}

function buildTraceEvent(kind: string, payload: Record<string, unknown>, attribution: AttributedProviderCall) {
	const activeRequest = attribution.span;
	return {
		trace_id: activeRequest?.traceId,
		kind,
		payload: dropUndefined({
			source: EXTENSION_NAME,
			decision_id: activeRequest?.decisionId,
			reservation_id: activeRequest?.reservationId,
			session_id: activeRequest?.sessionId,
			agent_run_id: activeRequest?.agentRunId,
			...payload,
			request_id: activeRequest?.requestId,
			provider_call_id: activeRequest?.providerCallId,
			turn_id: activeRequest?.turnId,
			attribution_status: attribution.status,
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
				session_id: activeRequest.sessionId,
				agent_run_id: activeRequest.agentRunId,
				request_id: activeRequest.requestId,
				provider_call_id: activeRequest.providerCallId,
				source: EXTENSION_NAME,
				usage_details: dropUndefined({
					cache_read_tokens: usage.cache_read_tokens,
					cache_write_tokens: usage.cache_write_tokens,
					input_cost_usd: usage.input_cost_usd,
					output_cost_usd: usage.output_cost_usd,
					cache_read_cost_usd: usage.cache_read_cost_usd,
					cache_write_cost_usd: usage.cache_write_cost_usd,
				}),
			},
		}),
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
		cache_read_tokens: integerValue(usage.cacheRead) || integerValue(usage.cacheReadTokens),
		cache_write_tokens: integerValue(usage.cacheWrite) || integerValue(usage.cacheWriteTokens),
		input_cost_usd: isRecord(usage.cost) ? numberValue(usage.cost.input) : undefined,
		output_cost_usd: isRecord(usage.cost) ? numberValue(usage.cost.output) : undefined,
		cache_read_cost_usd: isRecord(usage.cost) ? numberValue(usage.cost.cacheRead) : undefined,
		cache_write_cost_usd: isRecord(usage.cost) ? numberValue(usage.cost.cacheWrite) : undefined,
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

type QueuePriority = 1 | 2 | 3 | 4 | 5 | 6;

type DeliveryQueue = {
	enqueue(priority: QueuePriority, run: () => Promise<void>): void;
};

function createDeliveryQueue(maxItems: number): DeliveryQueue {
	const queue: Array<{ priority: QueuePriority; run: () => Promise<void> }> = [];
	let draining = false;

	function schedule(): void {
		if (draining) {
			return;
		}
		draining = true;
		Promise.resolve().then(drain);
	}

	async function drain(): Promise<void> {
		try {
			while (queue.length > 0) {
				const item = queue.shift()!;
				item.run().catch(() => {
					// Delivery failures must never affect Pi's provider behavior.
				});
			}
		} finally {
			draining = false;
			if (queue.length > 0) {
				schedule();
			}
		}
	}

	return {
		enqueue(priority, run) {
			if (queue.length >= maxItems) {
				let lowestIndex = -1;
				let lowestPriority = priority;
				for (const [index, item] of queue.entries()) {
					if (item.priority < lowestPriority) {
						lowestPriority = item.priority;
						lowestIndex = index;
					}
				}
				if (lowestIndex === -1) {
					return;
				}
				queue.splice(lowestIndex, 1);
			}
			queue.push({ priority, run });
			queue.sort((left, right) => right.priority - left.priority);
			schedule();
		},
	};
}

function makeStreamSummary(): StreamSummary {
	return { counts: {}, tool_calls: {} };
}

function eventMessage(event: unknown): Record<string, unknown> | undefined {
	if (!isRecord(event)) {
		return undefined;
	}
	return isRecord(event.message) ? event.message : event;
}

function responseIdFromEvent(event: unknown): string | undefined {
	const record = isRecord(event) ? event : {};
	const message = eventMessage(event) || {};
	return (
		stringValue(record.responseId) ||
		stringValue(record.response_id) ||
		stringValue(record.messageId) ||
		stringValue(message.responseId) ||
		stringValue(message.response_id) ||
		stringValue(message.id)
	);
}

function toolCallIdFromEvent(event: unknown): string | undefined {
	const record = isRecord(event) ? event : {};
	const message = eventMessage(event) || {};
	if (stringValue(record.toolCallId)) {
		return stringValue(record.toolCallId);
	}
	if (isRecord(record.toolCall)) {
		return stringValue(record.toolCall.id) || stringValue(record.toolCall.toolCallId);
	}
	if (Array.isArray(message.toolCalls) && isRecord(message.toolCalls[0])) {
		return stringValue(message.toolCalls[0].id) || stringValue(message.toolCalls[0].toolCallId);
	}
	return undefined;
}

function toolNameFromEvent(event: unknown): string | undefined {
	const record = isRecord(event) ? event : {};
	if (stringValue(record.toolName)) {
		return stringValue(record.toolName);
	}
	if (isRecord(record.toolCall)) {
		return stringValue(record.toolCall.name) || stringValue(record.toolCall.toolName);
	}
	return undefined;
}

function messageSummary(message: unknown): Record<string, unknown> {
	const record = isRecord(message) ? message : {};
	return dropUndefined({
		role: stringValue(record.role),
		provider: stringValue(record.provider),
		model: stringValue(record.model),
		response_id: responseIdFromEvent(record),
		stop_reason: stringValue(record.stopReason),
		content_summary: summarizeValue(record.content),
		tool_calls: summarizeToolCalls(record.toolCalls),
	});
}

function summarizeToolCalls(value: unknown): unknown {
	if (!Array.isArray(value)) {
		return undefined;
	}
	return value.slice(0, 50).map((item) => {
		const record = isRecord(item) ? item : {};
		return dropUndefined({
			tool_call_id: stringValue(record.id) || stringValue(record.toolCallId),
			name: stringValue(record.name) || stringValue(record.toolName),
			arguments_summary: summarizeValue(record.arguments),
		});
	});
}

function streamDeltaType(event: unknown): string {
	const record = isRecord(event) ? event : {};
	return (
		stringValue(record.type) ||
		stringValue(record.eventType) ||
		stringValue(record.deltaType) ||
		stringValue(record.kind) ||
		"unknown"
	);
}

function updateStreamSummary(span: ActiveRequest, event: unknown): void {
	const now = new Date().toISOString();
	const deltaType = streamDeltaType(event);
	span.streamSummary.first_at ||= now;
	span.streamSummary.last_at = now;
	span.streamSummary.counts[deltaType] = (span.streamSummary.counts[deltaType] || 0) + 1;
	const toolCallId = toolCallIdFromEvent(event);
	if (toolCallId) {
		span.streamSummary.tool_calls[toolCallId] = dropUndefined({
			name: toolNameFromEvent(event) || span.streamSummary.tool_calls[toolCallId]?.name,
		});
	}
}

function streamSummaryPayload(span: ActiveRequest): Record<string, unknown> {
	return {
		counts: span.streamSummary.counts,
		first_at: span.streamSummary.first_at,
		last_at: span.streamSummary.last_at,
		tool_calls: Object.entries(span.streamSummary.tool_calls).map(([tool_call_id, value]) =>
			dropUndefined({ tool_call_id, name: value.name }),
		),
	};
}

export default function registerNoetherExtension(pi: ExtensionAPI, config: NoetherConfig = extensionConfig()): void {
	config = {
		...config,
		authorizeTimeoutMs: config.authorizeTimeoutMs || DEFAULT_AUTHORIZE_TIMEOUT_MS,
		queueMaxItems: config.queueMaxItems || DEFAULT_QUEUE_MAX_ITEMS,
		debugHooks: config.debugHooks || false,
	};
	const sessionId = makeTraceId();
	let traceId = makeTraceId();
	let agentRunId = makeTraceId();
	let latestProviderCall: ActiveRequest | undefined;
	let pendingAgentContext: Record<string, unknown> | undefined;
	const completedReservations = new Set<string>();
	const toolStartedAt = new Map<string, number>();
	const providerCallsById = new Map<string, ActiveRequest>();
	const providerCallByResponseId = new Map<string, string>();
	const providerCallByToolCallId = new Map<string, string>();
	const recentProviderCalls: ActiveRequest[] = [];
	const attributionCounts: Record<AttributionStatus, number> = { exact: 0, fallback: 0, unmatched: 0 };
	const delivery = createDeliveryQueue(config.queueMaxItems || DEFAULT_QUEUE_MAX_ITEMS);

	function enqueueEvent(
		kind: string,
		payload: Record<string, unknown>,
		attribution: AttributedProviderCall,
		priority: QueuePriority = 3,
	): void {
		attributionCounts[attribution.status] += 1;
		const event = buildTraceEvent(kind, payload, attribution);
		delivery.enqueue(priority, () => postEvent(config.noetherUrl, event));
	}

	function enqueueHookLog(
		hook: "before_provider_request" | "message_update" | "message_end" | "turn_end" | "agent_end",
		payload: Record<string, unknown>,
	): void {
		delivery.enqueue(1, () => safeWriteHookLog(config, hook, payload));
	}

	function rememberProviderCall(span: ActiveRequest): void {
		providerCallsById.set(span.providerCallId, span);
		recentProviderCalls.push(span);
		while (recentProviderCalls.length > 20) {
			recentProviderCalls.shift();
		}
		latestProviderCall = span;
	}

	function associateEventIds(span: ActiveRequest, event: unknown): void {
		const responseId = responseIdFromEvent(event);
		if (responseId) {
			span.responseId = responseId;
			providerCallByResponseId.set(responseId, span.providerCallId);
		}
		const toolCallId = toolCallIdFromEvent(event);
		if (toolCallId) {
			providerCallByToolCallId.set(toolCallId, span.providerCallId);
		}
	}

	function resolveProviderCall(event?: unknown): AttributedProviderCall {
		const record = isRecord(event) ? event : {};
		const explicitProviderCallId = stringValue(record.provider_call_id) || stringValue(record.providerCallId);
		if (explicitProviderCallId && providerCallsById.has(explicitProviderCallId)) {
			return { span: providerCallsById.get(explicitProviderCallId), status: "exact" };
		}
		const responseId = responseIdFromEvent(event);
		const providerCallIdByResponse = responseId ? providerCallByResponseId.get(responseId) : undefined;
		if (providerCallIdByResponse && providerCallsById.has(providerCallIdByResponse)) {
			return { span: providerCallsById.get(providerCallIdByResponse), status: "exact" };
		}
		const toolCallId = toolCallIdFromEvent(event);
		const providerCallIdByTool = toolCallId ? providerCallByToolCallId.get(toolCallId) : undefined;
		if (providerCallIdByTool && providerCallsById.has(providerCallIdByTool)) {
			return { span: providerCallsById.get(providerCallIdByTool), status: "exact" };
		}
		if (latestProviderCall) {
			return { span: latestProviderCall, status: "fallback" };
		}
		return { status: "unmatched" };
	}

	pi.on("before_agent_start", (event) => {
		agentRunId = makeTraceId();
		pendingAgentContext = summarizeAgentContext(event);
	});

	pi.on("session_start", () => {
		traceId = makeTraceId();
		agentRunId = makeTraceId();
	});

	pi.on("before_provider_request", async (event, ctx) => {
		const providerCallId = makeTraceId();
		const requestId = providerCallId;
		const request = buildAuthorizeRequest(isRecord(event) ? event : {}, ctx, config, {
			traceId,
			sessionId,
			agentRunId,
			requestId,
			providerCallId,
		}) as MutableAuthorizeRequest;
		if (pendingAgentContext && Object.keys(pendingAgentContext).length > 0) {
			request.metadata = {
				...request.metadata,
				agent_context: pendingAgentContext,
			};
		}
		enqueueHookLog("before_provider_request", {
			trace_id: traceId,
			request_id: requestId,
			event,
			ctx,
			noether_authorize_request: request,
		});
		const span: ActiveRequest = {
			traceId,
			sessionId,
			agentRunId,
			requestId,
			providerCallId,
			startedAt: Date.now(),
			streamSummary: makeStreamSummary(),
		};
		rememberProviderCall(span);

		try {
			const decision = await authorize(config.noetherUrl, request, config.authorizeTimeoutMs, ctx.signal);
			span.decisionId = decision.decision_id;
			span.reservationId = decision.reservation?.id;
			span.outcome = decision.outcome;
			if (shouldAbortForDecision(decision)) {
				ctx.abort?.();
			}
			if (pendingAgentContext && Object.keys(pendingAgentContext).length > 0) {
				enqueueEvent("pi.agent_context", pendingAgentContext, { span, status: "exact" }, 3);
			}
			enqueueEvent(
				"pi.provider_call.started",
				{
					provider: request.provider,
					model: request.model,
					payload_keys: request.metadata?.payload_keys,
					payload_summary: request.metadata?.payload_summary,
					context_window: request.metadata?.context_window,
					context_usage_percent: request.metadata?.context_usage_percent,
				},
				{ span, status: "exact" },
				3,
			);
			enqueueEvent("pi.authorize", { request, outcome: decision.outcome }, { span, status: "exact" }, 3);
		} catch (error) {
			if (config.failMode === "fail_closed") {
				ctx.abort?.();
			}
			enqueueEvent(
				"pi.authorize_error",
				{ error: error instanceof Error ? error.message : String(error), fail_mode: config.failMode },
				{ span, status: "exact" },
				3,
			);
		}
		pendingAgentContext = undefined;
	});

	pi.on("tool_call", (event) => {
		const eventRecord = isRecord(event) ? event : {};
		const toolCallId = stringValue(eventRecord.toolCallId);
		if (toolCallId) {
			toolStartedAt.set(toolCallId, Date.now());
		}
		const attribution = resolveProviderCall(event);
		if (toolCallId && attribution.span) {
			providerCallByToolCallId.set(toolCallId, attribution.span.providerCallId);
		}
		enqueueEvent(
			"pi.tool_call",
			{
				tool_name: stringValue(eventRecord.toolName),
				tool_call_id: toolCallId,
				input_summary: summarizeObject(eventRecord.input),
			},
			attribution,
			3,
		);
	});

	pi.on("tool_result", (event) => {
		const eventRecord = isRecord(event) ? event : {};
		const toolCallId = stringValue(eventRecord.toolCallId);
		const startedAt = toolCallId ? toolStartedAt.get(toolCallId) : undefined;
		if (toolCallId) {
			toolStartedAt.delete(toolCallId);
		}
		const attribution = resolveProviderCall(event);
		if (toolCallId && attribution.span) {
			providerCallByToolCallId.set(toolCallId, attribution.span.providerCallId);
		}
		enqueueEvent(
			"tool.observed",
			{
				name: stringValue(eventRecord.toolName) || "unknown",
				duration_ms: startedAt ? Date.now() - startedAt : undefined,
				success: typeof eventRecord.isError === "boolean" ? !eventRecord.isError : undefined,
				metadata: summarizeToolMetadata(event),
			},
			attribution,
			4,
		);
	});

	pi.on("message_update", (event, ctx) => {
		const attribution = resolveProviderCall(event);
		if (attribution.span) {
			associateEventIds(attribution.span, event);
			updateStreamSummary(attribution.span, event);
		}
		enqueueHookLog("message_update", {
			trace_id: attribution.span?.traceId,
			request_id: attribution.span?.requestId,
			event,
			ctx,
			active_request: attribution.span,
			attribution_status: attribution.status,
		});
	});

	pi.on("message_end", (event, ctx) => {
		const attribution = resolveProviderCall(event);
		const request = attribution.span;
		if (request) {
			associateEventIds(request, event);
		}
		enqueueHookLog("message_end", {
			trace_id: request?.traceId,
			request_id: request?.requestId,
			event,
			ctx,
			active_request: request,
			attribution_status: attribution.status,
		});
		const usage = extractUsage(isRecord(event) ? event.message : undefined);
		if (!usage) {
			return;
		}
		enqueueEvent(
			"pi.message_end",
			{
				usage,
				message: messageSummary(isRecord(event) ? event.message : event),
			},
			attribution,
			3,
		);
		if (request && Object.keys(request.streamSummary.counts).length > 0) {
			enqueueEvent("pi.stream_summary", streamSummaryPayload(request), { span: request, status: "exact" }, 2);
		}
		if (!request?.reservationId) {
			return;
		}
		if (completedReservations.has(request.reservationId)) {
			return;
		}
		completedReservations.add(request.reservationId);
		delivery.enqueue(5, async () => {
			try {
				await finalizeReservation(config.noetherUrl, request.reservationId!, usage, request);
			} catch {
				enqueueEvent("pi.reservation_finalize_error", { usage }, { span: request, status: "exact" }, 3);
			}
		});
	});

	pi.on("turn_end", (event, ctx) => {
		const eventRecord = isRecord(event) ? event : {};
		const attribution = resolveProviderCall(event);
		const request = attribution.span;
		const turnIndex = eventRecord.turnIndex;
		if (request && (typeof turnIndex === "number" || typeof turnIndex === "string")) {
			request.turnId = `turn-${turnIndex}`;
		}
		enqueueHookLog("turn_end", {
			trace_id: request?.traceId,
			request_id: request?.requestId,
			event,
			ctx,
			active_request: request,
			attribution_status: attribution.status,
		});
		enqueueEvent(
			"pi.turn_end",
			{
				turn_index: turnIndex,
				usage: extractUsage(eventRecord.message),
			},
			attribution,
			3,
		);
		if (Array.isArray(eventRecord.toolResults)) {
			for (const toolResult of eventRecord.toolResults) {
				const toolAttribution = resolveProviderCall(toolResult);
				enqueueEvent(
					"tool.observed",
					{
						name: isRecord(toolResult) ? stringValue(toolResult.toolName) || "unknown" : "unknown",
						success: isRecord(toolResult) && typeof toolResult.isError === "boolean" ? !toolResult.isError : undefined,
						metadata: summarizeToolMetadata(toolResult),
					},
					toolAttribution,
					4,
				);
			}
		}
	});

	pi.on("agent_end", (event, ctx) => {
		const attribution = resolveProviderCall(event);
		enqueueHookLog("agent_end", {
			trace_id: attribution.span?.traceId,
			request_id: attribution.span?.requestId,
			event,
			ctx,
			active_request: attribution.span,
			attribution_status: attribution.status,
		});
		const messages = isRecord(event) && Array.isArray(event.messages) ? event.messages : [];
		enqueueEvent(
			"pi.agent_end",
			{
				message_count: messages.length,
				provider_call_count: providerCallsById.size,
				attribution_counts: attributionCounts,
			},
			attribution,
			3,
		);
	});
}
