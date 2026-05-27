const DEFAULT_NOETHER_URL = "http://127.0.0.1:4051";
const DEFAULT_TIMEOUT_MS = 1000;
const SOURCE = "noether-opencode";

export const NoetherOpenCode = async (ctx) => {
	const config = opencodeConfig(ctx);
	const postEvent = (kind, payload) => postNoetherEvent(config, {
		trace_id: payload.trace_id,
		kind,
		payload: {
			source: SOURCE,
			project: config.project,
			subject: config.subject,
			...payload,
		},
	});

	return {
		event: async ({ event }) => {
			await swallowDelivery(() => postEvent(`opencode.${event?.type || "event"}`, {
				event_type: event?.type,
				event: summarizeValue(event, config.includeBody),
			}));
		},
		"tool.execute.before": async (input, output) => {
			await swallowDelivery(() => postEvent("opencode.tool_execute_before", {
				tool: input?.tool,
				session_id: input?.sessionID || input?.session_id,
				call_id: input?.callID || input?.call_id,
				args: summarizeValue(output?.args, config.includeBody),
			}));
		},
		"tool.execute.after": async (input, output) => {
			await swallowDelivery(() => postEvent("opencode.tool_execute_after", {
				tool: input?.tool,
				session_id: input?.sessionID || input?.session_id,
				call_id: input?.callID || input?.call_id,
				error: output?.error ? summarizeValue(output.error, false) : undefined,
				result: summarizeValue(output?.result, config.includeBody),
			}));
		},
	};
};

export default NoetherOpenCode;

export function opencodeConfig(ctx = {}, env = process.env) {
	const directory = typeof ctx.directory === "string" ? ctx.directory : undefined;
	const project = record(ctx.project);
	return {
		noetherUrl: stripTrailingSlash(env.NOET_OPENCODE_URL || DEFAULT_NOETHER_URL),
		timeoutMs: positiveInteger(env.NOET_OPENCODE_TIMEOUT_MS) || DEFAULT_TIMEOUT_MS,
		project:
			emptyToUndefined(env.NOET_OPENCODE_PROJECT) ||
			stringValue(project.name) ||
			(directory ? basename(directory) : undefined),
		subject: emptyToUndefined(env.NOET_OPENCODE_SUBJECT),
		includeBody: env.NOET_OPENCODE_INCLUDE_BODY === "1" || env.NOET_OPENCODE_INCLUDE_BODY === "true",
	};
}

export async function postNoetherEvent(config, event) {
	const controller = new AbortController();
	const timeout = setTimeout(
		() => controller.abort(new Error(`Noether event timed out after ${config.timeoutMs}ms`)),
		config.timeoutMs,
	);
	try {
		const response = await fetch(`${config.noetherUrl}/v1/events`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(dropUndefined(event)),
			signal: controller.signal,
		});
		if (!response.ok) {
			throw new Error(`Noether event returned ${response.status}`);
		}
	} finally {
		clearTimeout(timeout);
	}
}

export function summarizeValue(value, includeBody = false, depth = 0) {
	if (value === null || value === undefined) {
		return value;
	}
	if (typeof value === "string") {
		return includeBody ? value : { type: "string", length: value.length };
	}
	if (typeof value === "number" || typeof value === "boolean") {
		return value;
	}
	if (Array.isArray(value)) {
		return {
			type: "array",
			length: value.length,
			item_types: histogram(value.map((item) => itemType(item))),
		};
	}
	if (typeof value !== "object") {
		return { type: typeof value };
	}
	if (depth >= 4) {
		return { type: "object", keys: Object.keys(value).sort() };
	}
	const output = {};
	for (const [key, item] of Object.entries(value)) {
		if (isSensitiveKey(key)) {
			output[key] = summarizeSensitive(item, includeBody);
		} else {
			output[key] = summarizeValue(item, includeBody, depth + 1);
		}
	}
	return output;
}

function summarizeSensitive(value, includeBody) {
	if (includeBody) {
		return value;
	}
	if (typeof value === "string") {
		return { type: "string", length: value.length };
	}
	if (Array.isArray(value)) {
		return { type: "array", length: value.length, item_types: histogram(value.map((item) => itemType(item))) };
	}
	if (value && typeof value === "object") {
		return { type: "object", keys: Object.keys(value).sort() };
	}
	return value;
}

async function swallowDelivery(run) {
	try {
		await run();
	} catch {
		// OpenCode provider/tool behavior must not depend on best-effort Noether event delivery.
	}
}

function stripTrailingSlash(value) {
	return value.endsWith("/") ? value.slice(0, -1) : value;
}

function emptyToUndefined(value) {
	return value && value.trim() ? value : undefined;
}

function positiveInteger(value) {
	if (!value) {
		return undefined;
	}
	const parsed = Number.parseInt(value, 10);
	return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined;
}

function basename(path) {
	const parts = path.split(/[\\/]/).filter(Boolean);
	return parts.at(-1);
}

function stringValue(value) {
	return typeof value === "string" && value ? value : undefined;
}

function record(value) {
	return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function itemType(value) {
	if (value && typeof value === "object" && !Array.isArray(value)) {
		return stringValue(value.type) || stringValue(value.role) || "object";
	}
	if (Array.isArray(value)) {
		return "array";
	}
	return typeof value;
}

function histogram(values) {
	const counts = {};
	for (const value of values) {
		counts[value] = (counts[value] || 0) + 1;
	}
	return counts;
}

function isSensitiveKey(key) {
	return /content|message|prompt|text|body|command|args|argument|result|output/i.test(key);
}

function dropUndefined(value) {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined));
}
