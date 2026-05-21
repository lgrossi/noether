// @ts-ignore: Pi runs extensions in Node; this package intentionally avoids a local @types/node dependency.
import { appendFile, mkdir } from "node:fs/promises";
import { existsSync, readFileSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { basename, join } from "node:path";

declare const process: {
	env: Record<string, string | undefined>;
	cwd: () => string;
};

const DEFAULT_NOETHER_URL = "http://127.0.0.1:4040";
const EXTENSION_NAME = "noether-pi";
const DEFAULT_FAIL_MODE = "fail_open";
const DEFAULT_AUTHORIZE_TIMEOUT_MS = 1_000;
const DEFAULT_QUEUE_MAX_ITEMS = 100;
const DEFAULT_DELIVERY_TIMEOUT_MS = 1_000;
const DEFAULT_DELIVERY_MAX_ATTEMPTS = 3;
const DEFAULT_WORKFLOWS = ["coding", "review", "research", "ops", "enablement", "incident"];
const DEFAULT_SURFACES = ["editor", "terminal", "automation"];

type FailMode = "fail_open" | "fail_closed";
type DecisionAction = "allow" | "warn" | "block" | "ask";
type AppliedPolicyAction = DecisionAction | "approved";
type UserApproval = "approved" | "rejected" | "unavailable";

type ExtensionUIContext = {
	notify?: (message: string, type?: "info" | "warning" | "error") => void;
	setStatus?: (key: string, text: string | undefined) => void;
	confirm?: (title: string, message: string, options?: { timeout?: number; signal?: AbortSignal }) => Promise<boolean>;
};

type ExtensionAPI = {
	on(event: string, handler: (event: unknown, ctx: ExtensionContext) => unknown | Promise<unknown>): void;
};

type ExtensionContext = {
	cwd?: string;
	model?: Record<string, unknown>;
	signal?: AbortSignal;
	hasUI?: boolean;
	ui?: ExtensionUIContext;
	getContextUsage?: () => Record<string, unknown> | undefined;
	abort?: () => void;
};

type NoetherConfig = {
	noetherUrl: string;
	project?: string;
	projectFromCwd: boolean;
	subject?: string;
	budgetId?: string;
	entities?: string[];
	synthetic?: SyntheticPopulationConfig;
	failMode: FailMode;
	includeBody: boolean;
	version: string;
	authorizeTimeoutMs: number;
	queueMaxItems: number;
	debugHooks: boolean;
	debugHookLogDir?: string;
};

type SyntheticPopulationConfig = {
	enabled: boolean;
	users: number;
	teams: number;
	companies: number;
	workflows: string[];
	surfaces: string[];
};

type PersistedNoetherConfig = Partial<Omit<NoetherConfig, "synthetic">> & {
	synthetic?: Partial<SyntheticPopulationConfig>;
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

type DecisionExplanation = {
	rule_id?: string;
	reason?: string;
	severity?: string;
};

type DecisionRouting = {
	selected_budget_id?: string;
	matched_entity?: string;
	selection_reason?: string;
	rejected_budget_id?: string;
	rejected_budget_reason?: string;
	model_check?: string;
	remaining_budget_usd?: number;
};

type AuthorizeDecision = {
	decision_id?: string;
	outcome?: string;
	action?: string;
	reservation?: {
		id?: string;
	};
	explanations?: DecisionExplanation[];
	routing?: DecisionRouting;
	metadata?: Record<string, unknown>;
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

export function extensionConfig(
	env: Record<string, string | undefined> = process.env,
	options: { cwd?: string; loadFiles?: boolean } = {},
): NoetherConfig {
	const cwd = options.cwd || process.cwd();
	const fileConfig = options.loadFiles === false ? {} : loadPersistedConfig(cwd);
	const synthetic = normalizeSyntheticConfig({
		...(fileConfig.synthetic || {}),
		enabled: booleanOverride(env.NOET_PI_SYNTHETIC_ENABLED, fileConfig.synthetic?.enabled),
		users: positiveInteger(env.NOET_PI_SYNTHETIC_USERS) || fileConfig.synthetic?.users,
		teams: positiveInteger(env.NOET_PI_SYNTHETIC_TEAMS) || fileConfig.synthetic?.teams,
		companies: positiveInteger(env.NOET_PI_SYNTHETIC_COMPANIES) || fileConfig.synthetic?.companies,
		workflows: parseEntities(env.NOET_PI_SYNTHETIC_WORKFLOWS) || fileConfig.synthetic?.workflows,
		surfaces: parseEntities(env.NOET_PI_SYNTHETIC_SURFACES) || fileConfig.synthetic?.surfaces,
	});
	return {
		noetherUrl: stripTrailingSlash(env.NOET_URL || fileConfig.noetherUrl || DEFAULT_NOETHER_URL),
		project: emptyToUndefined(env.NOET_PI_PROJECT) || fileConfig.project,
		projectFromCwd: booleanOverride(env.NOET_PI_PROJECT_FROM_CWD, fileConfig.projectFromCwd) ?? true,
		subject: emptyToUndefined(env.NOET_PI_SUBJECT) || fileConfig.subject,
		budgetId: emptyToUndefined(env.NOET_PI_BUDGET_ID) || fileConfig.budgetId,
		entities: parseEntities(env.NOET_PI_ENTITIES) || fileConfig.entities,
		synthetic,
		failMode:
			normalizeFailMode(env.NOET_PI_FAIL_MODE) || normalizeFailMode(fileConfig.failMode) || DEFAULT_FAIL_MODE,
		includeBody:
			booleanOverride(env.NOET_PI_INCLUDE_BODY, fileConfig.includeBody) || false,
		version: env.NOET_PI_EXTENSION_VERSION || fileConfig.version || "dev",
		authorizeTimeoutMs:
			positiveInteger(env.NOET_PI_AUTHORIZE_TIMEOUT_MS) || fileConfig.authorizeTimeoutMs || DEFAULT_AUTHORIZE_TIMEOUT_MS,
		queueMaxItems: positiveInteger(env.NOET_PI_QUEUE_MAX_ITEMS) || fileConfig.queueMaxItems || DEFAULT_QUEUE_MAX_ITEMS,
		debugHooks: env.NOET_PI_DEBUG_HOOKS === "raw" || fileConfig.debugHooks || false,
		debugHookLogDir: emptyToUndefined(env.NOET_PI_DEBUG_HOOK_LOG_DIR) || fileConfig.debugHookLogDir,
	};
}

function stripTrailingSlash(value: string): string {
	return value.endsWith("/") ? value.slice(0, -1) : value;
}

function emptyToUndefined(value: string | undefined): string | undefined {
	return value && value.trim() ? value : undefined;
}

function booleanOverride(value: string | undefined, fallback?: boolean): boolean | undefined {
	if (value === "1" || value === "true") {
		return true;
	}
	if (value === "0" || value === "false") {
		return false;
	}
	return fallback;
}

function positiveInteger(value: string | undefined): number | undefined {
	if (!value) {
		return undefined;
	}
	const parsed = Number.parseInt(value, 10);
	return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined;
}

function normalizeFailMode(value: unknown): FailMode | undefined {
	return value === "fail_closed" || value === "fail_open" ? value : undefined;
}

function parseEntities(value: string | undefined): string[] | undefined {
	if (!value) {
		return undefined;
	}
	const entities = value
		.split(",")
		.map((entity) => entity.trim())
		.filter((entity) => entity.length > 0);
	return entities.length > 0 ? entities : undefined;
}

function normalizeSyntheticConfig(
	value: Partial<SyntheticPopulationConfig> | undefined,
): SyntheticPopulationConfig | undefined {
	if (!value?.enabled) {
		return undefined;
	}
	return {
		enabled: true,
		users: value.users || 50,
		teams: value.teams || 6,
		companies: value.companies || 3,
		workflows: value.workflows && value.workflows.length > 0 ? value.workflows : DEFAULT_WORKFLOWS,
		surfaces: value.surfaces && value.surfaces.length > 0 ? value.surfaces : DEFAULT_SURFACES,
	};
}

function loadPersistedConfig(cwd: string): PersistedNoetherConfig {
	return mergePersistedConfigs(
		readPersistedConfig(join(homedir(), ".pi/agent/noether.json")),
		readPersistedConfig(join(cwd, ".pi/noether.json")),
	);
}

function readPersistedConfig(path: string): PersistedNoetherConfig {
	if (!existsSync(path)) {
		return {};
	}
	try {
		const value = JSON.parse(readFileSync(path, "utf8"));
		return isRecord(value) ? (value as PersistedNoetherConfig) : {};
	} catch (error) {
		console.error(
			`[noether-pi] failed to parse config ${path}: ${error instanceof Error ? error.message : String(error)}`,
		);
		return {};
	}
}

function mergePersistedConfigs(...configs: PersistedNoetherConfig[]): PersistedNoetherConfig {
	return configs.reduce<PersistedNoetherConfig>(
		(acc, config) => ({
			...acc,
			...config,
			synthetic: {
				...(acc.synthetic || {}),
				...(config.synthetic || {}),
			},
		}),
		{},
	);
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
	const synthetic = syntheticAttribution(config, ctx, correlation);
	const project = config.project || deriveProjectFromCwd(ctx.cwd, config.projectFromCwd) || synthetic.project;
	const subject = config.subject || synthetic.subject || deriveSubjectFromOs();
	const modelApi = stringValue(model.api);
	const entities = uniqueEntities([
		...(config.entities || []),
		...(project ? [`project:${project}`] : []),
		...(subject ? [normalizeSubject(subject)] : []),
		...synthetic.entities,
	]);
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
		model_api: modelApi,
		request_surface: deriveRequestSurface(provider, modelApi, payload),
		payload_kind: Array.isArray(event.payload) ? "array" : typeof event.payload,
		payload_keys: Object.keys(payload).sort(),
		payload_summary: summarizePayload(payload, config.includeBody),
		context_window: numberValue(contextUsage && contextUsage.contextWindow),
		context_usage_percent: numberValue(contextUsage && contextUsage.percent),
		project_source: !config.project && project ? "cwd" : undefined,
		synthetic_attribution: synthetic.metadata,
	};

	return {
		budget_id: config.budgetId,
		entities: entities.length > 0 ? entities : undefined,
		subject,
		project,
		provider,
		model: modelId,
		estimated_tokens: integerValue(contextUsage && contextUsage.tokens),
		metadata: dropUndefined(metadata),
	};
}

function deriveProjectFromCwd(cwd: string | undefined, enabled: boolean): string | undefined {
	if (!enabled) {
		return undefined;
	}
	const source = cwd || process.cwd();
	const segment = basename(source);
	const normalized = segment.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
	return normalized || undefined;
}

function deriveSubjectFromOs(): string | undefined {
	const envUser = emptyToUndefined(process.env.USER) || emptyToUndefined(process.env.LOGNAME);
	if (envUser) {
		return normalizeSubject(envUser);
	}
	try {
		return normalizeSubject(userInfo().username);
	} catch {
		return undefined;
	}
}

function normalizeSubject(subject: string): string {
	return subject.includes(":") ? subject : `user:${subject}`;
}

function deriveRequestSurface(
	provider: string | undefined,
	modelApi: string | undefined,
	payload: Record<string, unknown>,
): string | undefined {
	const normalizedApi = modelApi?.toLowerCase();
	if (normalizedApi?.includes("responses")) {
		return "responses";
	}
	if (normalizedApi?.includes("chat")) {
		return "chat";
	}
	if (normalizedApi?.includes("messages")) {
		return "messages";
	}
	if (Array.isArray(payload.messages)) {
		return provider?.toLowerCase().includes("anthropic") ? "messages" : "chat";
	}
	if ("input" in payload || "instructions" in payload) {
		return "responses";
	}
	return undefined;
}

function uniqueEntities(values: string[]): string[] {
	const seen = new Set<string>();
	const output: string[] = [];
	for (const value of values) {
		const trimmed = value.trim();
		if (!trimmed || seen.has(trimmed)) {
			continue;
		}
		seen.add(trimmed);
		output.push(trimmed);
	}
	return output;
}

function syntheticAttribution(
	config: NoetherConfig,
	ctx: ExtensionContext,
	correlation: { providerCallId?: string; requestId?: string },
): { project?: string; subject?: string; entities: string[]; metadata?: Record<string, unknown> } {
	const synthetic = config.synthetic;
	if (!synthetic?.enabled) {
		return { entities: [] };
	}
	const seed = `${correlation.providerCallId || correlation.requestId || makeTraceId()}|${ctx.cwd || ""}`;
	const userIndex = seededIndex(seed, synthetic.users);
	const teamIndex = userIndex % synthetic.teams;
	const companyIndex = teamIndex % synthetic.companies;
	const workflow = synthetic.workflows[seededIndex(`${seed}|workflow`, synthetic.workflows.length)];
	const surface = synthetic.surfaces[seededIndex(`${seed}|surface`, synthetic.surfaces.length)];
	const subject = `user:user-${String(userIndex + 1).padStart(2, "0")}`;
	return {
		subject,
		entities: [
			`org:company-${String(companyIndex + 1).padStart(2, "0")}`,
			`team:team-${String(teamIndex + 1).padStart(2, "0")}`,
			`workflow:${workflow}`,
			`surface:${surface}`,
		],
		metadata: {
			mode: "synthetic_population",
			user: subject,
			team: `team-${String(teamIndex + 1).padStart(2, "0")}`,
			company: `company-${String(companyIndex + 1).padStart(2, "0")}`,
			workflow,
			surface,
		},
	};
}

function seededIndex(seed: string, size: number): number {
	if (size <= 1) {
		return 0;
	}
	let hash = 0;
	for (let index = 0; index < seed.length; index += 1) {
		hash = (hash * 31 + seed.charCodeAt(index)) >>> 0;
	}
	return hash % size;
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

function normalizeDecisionAction(value: unknown): DecisionAction | undefined {
	return value === "allow" || value === "warn" || value === "block" || value === "ask" ? value : undefined;
}

export function decisionAction(decision: AuthorizeDecision | undefined): DecisionAction {
	const explicit = normalizeDecisionAction(decision?.action);
	if (explicit) {
		return explicit;
	}
	if (decision?.outcome === "warn") {
		return "warn";
	}
	if (decision?.outcome === "deny") {
		return "block";
	}
	return "allow";
}

export function shouldAbortForDecision(decision: AuthorizeDecision | undefined): boolean {
	const action = decisionAction(decision);
	return action === "block" || action === "ask";
}

function decisionExplanations(decision: AuthorizeDecision | undefined): DecisionExplanation[] {
	if (!decision || !Array.isArray(decision.explanations)) {
		return [];
	}
	return decision.explanations.filter((explanation) => isRecord(explanation));
}

function summarizeDecisionExplanations(
	decision: AuthorizeDecision | undefined,
): Array<{ rule_id?: string; reason?: string; severity?: string }> | undefined {
	const explanations = decisionExplanations(decision).map((explanation) =>
		dropUndefined({
			rule_id: stringValue(explanation.rule_id),
			reason: stringValue(explanation.reason),
			severity: stringValue(explanation.severity),
		}),
	);
	return explanations.length > 0 ? explanations : undefined;
}

function extractDecisionRouting(decision: AuthorizeDecision | undefined): DecisionRouting | undefined {
	if (!decision) {
		return undefined;
	}
	const candidate =
		(isRecord(decision.routing) && decision.routing) ||
		(isRecord(decision.metadata) && isRecord(decision.metadata.routing) ? decision.metadata.routing : undefined);
	if (!candidate) {
		return undefined;
	}
	const routing = dropUndefined({
		selected_budget_id: stringValue(candidate.selected_budget_id),
		matched_entity: stringValue(candidate.matched_entity),
		selection_reason: stringValue(candidate.selection_reason),
		rejected_budget_id: stringValue(candidate.rejected_budget_id),
		rejected_budget_reason: stringValue(candidate.rejected_budget_reason),
		model_check: stringValue(candidate.model_check),
		remaining_budget_usd: numberValue(candidate.remaining_budget_usd),
	});
	return Object.keys(routing).length > 0 ? routing : undefined;
}

function formatDecisionExplanation(explanation: DecisionExplanation): string | undefined {
	const reason = stringValue(explanation.reason);
	const ruleId = stringValue(explanation.rule_id);
	if (reason && ruleId) {
		return `${reason} (${ruleId})`;
	}
	if (reason) {
		return reason;
	}
	if (ruleId) {
		return `matched rule ${ruleId}`;
	}
	return undefined;
}

function uniqueStrings(values: Array<string | undefined>): string[] {
	const seen = new Set<string>();
	const output: string[] = [];
	for (const value of values) {
		const normalized = value?.trim();
		if (!normalized || seen.has(normalized)) {
			continue;
		}
		seen.add(normalized);
		output.push(normalized);
	}
	return output;
}

type AttemptedModelContext = {
	provider?: string;
	model?: string;
};

function attemptedModelLabel(context: AttemptedModelContext | undefined): string | undefined {
	if (!context) {
		return undefined;
	}
	if (context.provider && context.model) {
		return `${context.provider}/${context.model}`;
	}
	return context.model || context.provider;
}

function decisionHasReason(decision: AuthorizeDecision | undefined, fragment: string): boolean {
	return decisionExplanations(decision).some((explanation) => stringValue(explanation.reason)?.includes(fragment));
}

function decisionBudgetId(decision: AuthorizeDecision | undefined): string | undefined {
	const routing = extractDecisionRouting(decision);
	return (
		routing?.rejected_budget_id ||
		routing?.selected_budget_id ||
		decisionExplanations(decision)
			.map((explanation) => stringValue(explanation.rule_id))
			.find((ruleId) => ruleId && !["no_fallback_budget", "no_budget_match"].includes(ruleId))
	);
}

function describeDecisionReason(
	decision: AuthorizeDecision | undefined,
	attemptedModel?: AttemptedModelContext,
): string {
	const routing = extractDecisionRouting(decision);
	if (decisionHasReason(decision, "provider/model is not allowed")) {
		const model = attemptedModelLabel(attemptedModel) || "the requested model";
		const budgetId = decisionBudgetId(decision);
		const noFallback = decisionHasReason(decision, "no fallback budget can satisfy the request");
		if (budgetId && noFallback) {
			return `${model} is not allowed on budget ${budgetId}, and no fallback budget can satisfy the request`;
		}
		if (budgetId) {
			return `${model} is not allowed on budget ${budgetId}`;
		}
		if (noFallback) {
			return `${model} is not allowed by the available budgets, and no fallback budget can satisfy the request`;
		}
		return `${model} is not allowed by the active budget policy`;
	}
	const primaryExplanation = decisionExplanations(decision).find((explanation) => {
		const reason = stringValue(explanation.reason);
		if (!reason) {
			return false;
		}
		return (
			!reason.startsWith("selected requested budget") &&
			!reason.startsWith("selected fallback budget") &&
			reason !== "requested budget does not exist"
		);
	});
	if (primaryExplanation) {
		return formatDecisionExplanation(primaryExplanation) || "Noether returned deny without an explanation";
	}
	const fragments = uniqueStrings([
		routing?.selection_reason,
		routing?.rejected_budget_reason
			? routing.rejected_budget_id
				? `budget ${routing.rejected_budget_id} rejected: ${routing.rejected_budget_reason}`
				: routing.rejected_budget_reason
			: undefined,
		routing?.matched_entity ? `matched entity ${routing.matched_entity}` : undefined,
		routing?.model_check ? `model check ${routing.model_check}` : undefined,
		typeof routing?.remaining_budget_usd === "number"
			? `remaining budget USD ${routing.remaining_budget_usd.toFixed(6)}`
			: undefined,
	]);
	return fragments.length > 0 ? fragments.join("; ") : "Noether returned deny without an explanation";
}

function appliedPolicyAction(
	decision: AuthorizeDecision | undefined,
	userApproval?: UserApproval,
): AppliedPolicyAction {
	const action = decisionAction(decision);
	if (action !== "ask") {
		return action;
	}
	return userApproval === "approved" ? "approved" : "block";
}

function messageWithDecisionId(message: string, decisionId: string | undefined): string {
	return decisionId ? `${message} [decision ${decisionId}]` : message;
}

function buildUserApprovalPrompt(
	decision: AuthorizeDecision,
	attemptedModel?: AttemptedModelContext,
): { title: string; message: string } {
	const reason = describeDecisionReason(decision, attemptedModel);
	const header = decision.decision_id ? `Decision ${decision.decision_id}` : "Noether deny decision";
	return {
		title: "Noether requested approval",
		message: `${header}: ${reason}\n\nProceed anyway?`,
	};
}

async function requestUserApproval(
	ctx: ExtensionContext,
	decision: AuthorizeDecision,
	attemptedModel?: AttemptedModelContext,
): Promise<UserApproval> {
	if (!ctx.hasUI || !ctx.ui?.confirm) {
		return "unavailable";
	}
	const prompt = buildUserApprovalPrompt(decision, attemptedModel);
	try {
		return (await ctx.ui.confirm(prompt.title, prompt.message, { signal: ctx.signal })) ? "approved" : "rejected";
	} catch {
		return "unavailable";
	}
}

function buildPolicyDecisionMessage(
	decision: AuthorizeDecision,
	action: DecisionAction,
	userApproval?: UserApproval,
	attemptedModel?: AttemptedModelContext,
): string {
	const reason = describeDecisionReason(decision, attemptedModel);
	if (action === "ask") {
		if (userApproval === "approved") {
			return messageWithDecisionId(
				`Noether requested approval for this request, and you approved proceeding: ${reason}`,
				decision.decision_id,
			);
		}
		if (userApproval === "rejected") {
			return messageWithDecisionId(
				`Noether asked for approval, you rejected proceeding, so the request stayed blocked: ${reason}`,
				decision.decision_id,
			);
		}
		return messageWithDecisionId(
			`Noether would normally ask for approval here, but this Pi run could not show an approval prompt, so the request was blocked: ${reason}`,
			decision.decision_id,
		);
	}
	if (action === "warn") {
		return messageWithDecisionId(`Noether warned on this request: ${reason}`, decision.decision_id);
	}
	return messageWithDecisionId(`Noether blocked this request: ${reason}`, decision.decision_id);
}

function buildAuthorizeFailureMessage(error: unknown, failMode: FailMode): string {
	const detail = error instanceof Error ? error.message : String(error);
	return `Noether authorization failed and failMode=${failMode} blocked this provider request: ${detail}`;
}

function truncateSingleLine(value: string, maxLength = 160): string {
	const normalized = value.replace(/\s+/g, " ").trim();
	if (normalized.length <= maxLength) {
		return normalized;
	}
	return `${normalized.slice(0, Math.max(0, maxLength - 3))}...`;
}

function clearExtensionStatus(ctx: ExtensionContext): void {
	ctx.ui?.setStatus?.(EXTENSION_NAME, undefined);
}

function surfaceExtensionMessage(
	ctx: ExtensionContext,
	message: string,
	type: "info" | "warning" | "error",
): void {
	ctx.ui?.notify?.(message, type);
	ctx.ui?.setStatus?.(EXTENSION_NAME, truncateSingleLine(message));
}

function logPolicyMessage(message: string, level: "info" | "warn" | "error"): void {
	const formatted = `[noether-pi] ${message}`;
	if (level === "error") {
		console.error(formatted);
		return;
	}
	if (level === "warn") {
		console.warn(formatted);
		return;
	}
	console.info(formatted);
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
	await postJsonWithRetry(`${noetherUrl}/v1/events`, event, DEFAULT_DELIVERY_TIMEOUT_MS, DEFAULT_DELIVERY_MAX_ATTEMPTS, signal);
}

async function finalizeReservation(
	noetherUrl: string,
	reservationId: string,
	usage: Usage,
	activeRequest: ActiveRequest,
): Promise<void> {
	await postJsonWithRetry(
		`${noetherUrl}/v1/reservations/${encodeURIComponent(reservationId)}/finalize`,
		{
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
		},
		DEFAULT_DELIVERY_TIMEOUT_MS,
		DEFAULT_DELIVERY_MAX_ATTEMPTS,
	);
}

async function postJsonWithRetry(
	url: string,
	body: unknown,
	timeoutMs: number,
	maxAttempts: number,
	signal?: AbortSignal,
): Promise<void> {
	let lastError: unknown;
	for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
		try {
			await postJson(url, body, timeoutMs, signal);
			return;
		} catch (error) {
			lastError = error;
			if (attempt < maxAttempts) {
				await delay(25 * attempt);
			}
		}
	}
	throw lastError instanceof Error ? lastError : new Error(String(lastError));
}

async function postJson(url: string, body: unknown, timeoutMs: number, signal?: AbortSignal): Promise<void> {
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
		() => controller.abort(new Error(`Noether delivery timed out after ${timeoutMs}ms`)),
		timeoutMs,
	);
	try {
		const response = await fetch(url, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(body),
			signal: controller.signal,
		});
		if (!response.ok) {
			throw new Error(`Noether delivery returned ${response.status}`);
		}
	} finally {
		clearTimeout(timeout);
		if (signal) {
			signal.removeEventListener("abort", abortFromParent);
		}
	}
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

function delay(ms = 0): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

type QueuePriority = 1 | 2 | 3 | 4 | 5 | 6;

type DeliveryQueueItem = {
	kind: string;
	priority: QueuePriority;
	run: () => Promise<void>;
};

type DeliveryQueueDrop = {
	dropped: { kind: string; priority: QueuePriority };
	enqueued: { kind: string; priority: QueuePriority };
	reason: "replaced" | "rejected";
};

type DeliveryQueue = {
	enqueue(priority: QueuePriority, run: () => Promise<void>, kind?: string): void;
};

type DeliveryQueueOptions = {
	onDrop?: (drop: DeliveryQueueDrop) => void;
};

export function createDeliveryQueue(maxItems: number, options: DeliveryQueueOptions = {}): DeliveryQueue {
	const queue: DeliveryQueueItem[] = [];
	let activeCount = 0;

	function schedule(): void {
		while (activeCount < maxItems && queue.length > 0) {
			const item = queue.shift()!;
			activeCount += 1;
			Promise.resolve()
				.then(() => item.run())
				.catch(() => {
					// Delivery failures must never affect Pi's provider behavior.
				})
				.finally(() => {
					activeCount -= 1;
					schedule();
				});
		}
	}

	return {
		enqueue(priority, run, kind = "delivery") {
			const nextItem = { kind, priority, run };
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
					options.onDrop?.({
						dropped: { kind, priority },
						enqueued: { kind, priority },
						reason: "rejected",
					});
					return;
				}
				const [dropped] = queue.splice(lowestIndex, 1);
				options.onDrop?.({
					dropped: { kind: dropped.kind, priority: dropped.priority },
					enqueued: { kind, priority },
					reason: "replaced",
				});
			}
			queue.push(nextItem);
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
		failMode: normalizeFailMode(config.failMode) || DEFAULT_FAIL_MODE,
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
	const delivery = createDeliveryQueue(config.queueMaxItems || DEFAULT_QUEUE_MAX_ITEMS, {
		onDrop(drop) {
			void postEvent(
				config.noetherUrl,
				buildTraceEvent(
					"pi.delivery_drop",
					{
						dropped_kind: drop.dropped.kind,
						dropped_priority: drop.dropped.priority,
						enqueued_kind: drop.enqueued.kind,
						enqueued_priority: drop.enqueued.priority,
						reason: drop.reason,
					},
					latestProviderCall ? { span: latestProviderCall, status: "fallback" } : { status: "unmatched" },
				),
			).catch(() => {
				// Best-effort surfacing only.
			});
		},
	});

	function enqueueEvent(
		kind: string,
		payload: Record<string, unknown>,
		attribution: AttributedProviderCall,
		priority: QueuePriority = 3,
	): void {
		attributionCounts[attribution.status] += 1;
		const event = buildTraceEvent(kind, payload, attribution);
		delivery.enqueue(priority, async () => {
			try {
				await postEvent(config.noetherUrl, event);
			} catch (error) {
				if (kind === "pi.delivery_error") {
					return;
				}
				try {
					await postEvent(
						config.noetherUrl,
						buildTraceEvent(
							"pi.delivery_error",
							{
								failed_kind: kind,
								error: error instanceof Error ? error.message : String(error),
							},
							attribution,
						),
					);
				} catch {
					// Best-effort surfacing only.
				}
			}
		}, kind);
	}

	function enqueueHookLog(
		hook: "before_provider_request" | "message_update" | "message_end" | "turn_end" | "agent_end",
		payload: Record<string, unknown>,
	): void {
		delivery.enqueue(1, () => safeWriteHookLog(config, hook, payload), `hook.${hook}`);
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
		clearExtensionStatus(ctx);
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
			const action = decisionAction(decision);
			const decisionNeedsHandling = action !== "allow";
			const attemptedModel = { provider: request.provider, model: request.model };
			const decisionReason = decisionNeedsHandling ? describeDecisionReason(decision, attemptedModel) : undefined;
			const userApproval = action === "ask" ? await requestUserApproval(ctx, decision, attemptedModel) : undefined;
			const policyAction = appliedPolicyAction(decision, userApproval);

			if (decisionNeedsHandling) {
				const message = buildPolicyDecisionMessage(decision, action, userApproval, attemptedModel);
				if (policyAction === "warn" || policyAction === "approved") {
					surfaceExtensionMessage(ctx, message, "warning");
					logPolicyMessage(message, "warn");
				} else {
					surfaceExtensionMessage(ctx, message, "error");
					logPolicyMessage(message, "error");
				}
			}
			if (policyAction === "block") {
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
					context_tokens: request.estimated_tokens,
					payload_keys: request.metadata?.payload_keys,
					payload_summary: request.metadata?.payload_summary,
					context_window: request.metadata?.context_window,
					context_usage_percent: request.metadata?.context_usage_percent,
				},
				{ span, status: "exact" },
				3,
			);
			enqueueEvent(
				"pi.authorize",
				{
					request,
					outcome: decision.outcome,
					decision_action: action,
					policy_action: policyAction,
					user_approval: userApproval,
					decision_reason: decisionReason,
					explanations: summarizeDecisionExplanations(decision),
					routing: extractDecisionRouting(decision),
				},
				{ span, status: "exact" },
				3,
			);
		} catch (error) {
			if (config.failMode === "fail_closed") {
				const message = buildAuthorizeFailureMessage(error, config.failMode);
				surfaceExtensionMessage(ctx, message, "error");
				logPolicyMessage(message, "error");
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
		}, "pi.finalize_reservation");
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
