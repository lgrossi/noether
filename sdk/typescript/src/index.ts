export type FailMode = "fail_open" | "fail_closed";
export type DecisionOutcome = "allow" | "warn" | "deny";
export type PolicyAction = "allow" | "warn" | "ask" | "block";
export type DecisionSeverity = "info" | "warn" | "deny";
export type ReservationStatus = "active" | "finalized";
export type FinalizeOutcome = "success" | "failure" | "cancelled";

export type NoetherClientOptions = {
	url?: string;
	timeoutMs?: number;
	failMode?: FailMode;
	apiKey?: string;
	fetch?: typeof fetch;
};

export type AuthorizeRequest = {
	budget_id?: string;
	entities?: string[];
	subject?: string;
	project?: string;
	provider?: string;
	model?: string;
	estimated_tokens?: number;
	estimated_cost_usd?: number;
	metadata?: Record<string, unknown>;
};

export type DecisionExplanation = {
	rule_id: string;
	reason: string;
	severity: DecisionSeverity;
};

export type Reservation = {
	id: string;
	amount_usd: number;
	currency: string;
	status: ReservationStatus;
	created_at: string;
	expires_at: string;
};

export type AuthorizeDecision = {
	decision_id: string;
	outcome: DecisionOutcome;
	action: PolicyAction;
	reservation?: Reservation;
	explanations: DecisionExplanation[];
	metadata?: AuthorizeDecisionMetadata;
	created_at: string;
};

export type AuthorizeDecisionMetadata = {
	message_hints?: AuthorizeMessageHintMetadata[];
	notifications?: AuthorizeNotificationMetadata[];
	[key: string]: unknown;
};

export type AuthorizeMessageHintMetadata = {
	kind: string;
	rule_id: string;
	severity: DecisionSeverity;
	recommendation: "show" | "hide" | string;
	limit_type?: string;
	window_id?: string;
	window_label?: string;
	window_mode?: string;
	window_ends_at?: string;
	projected_spend_usd?: number;
	max_usd?: number;
	threshold_usd?: number;
	threshold_percent?: number;
};

export type AuthorizeNotificationMetadata = {
	kind: string;
	key: string;
	severity: DecisionSeverity;
	title: string;
	body: string;
};

export type UsageObservation = {
	provider?: string;
	model?: string;
	input_tokens?: number;
	output_tokens?: number;
	total_tokens?: number;
	cost_usd?: number;
	latency_ms?: number;
	stop_reason?: string;
};

export type FinalizeReservation = {
	reservation_id?: string;
	outcome?: FinalizeOutcome;
	usage?: UsageObservation;
	actual_cost_usd?: number;
	metadata?: Record<string, unknown>;
};

export type TraceEvent = {
	id?: string;
	trace_id?: string;
	occurred_at?: string;
	kind: string;
	payload?: unknown;
};

export type HealthResponse = {
	status: "ok";
	decision_mode: "dry_run" | "enforce";
	policy_loaded: boolean;
	upstream_configured: boolean;
	route_count: number;
	ledger_backend: string;
	auth_configured: boolean;
	request_body_limit_bytes: number;
	replay_jobs: number;
	replay_job_capacity: number;
	postgres_async_finalize_failures?: number | null;
};

export type WithDecisionContext = {
	decision: AuthorizeDecision;
	client: NoetherClient;
};

export class NoetherError extends Error {
	constructor(
		message: string,
		readonly cause?: unknown,
	) {
		super(message);
		this.name = "NoetherError";
	}
}

export class NoetherHttpError extends NoetherError {
	constructor(
		readonly status: number,
		readonly body: string,
	) {
		super(`Noether request failed with HTTP ${status}: ${body}`);
		this.name = "NoetherHttpError";
	}
}

export class NoetherDeniedError extends NoetherError {
	constructor(readonly decision: AuthorizeDecision) {
		super(`Noether denied request: ${decision.explanations.map((item) => item.reason).join("; ")}`);
		this.name = "NoetherDeniedError";
	}
}

function isNoetherAuthFailure(error: unknown): boolean {
	return error instanceof NoetherHttpError && (error.status === 401 || error.status === 403);
}

export class NoetherClient {
	readonly url: string;
	readonly timeoutMs: number;
	readonly failMode: FailMode;
	readonly apiKey?: string;
	private readonly fetchImpl: typeof fetch;

	constructor(options: NoetherClientOptions = {}) {
		this.url = stripTrailingSlash(options.url ?? "http://127.0.0.1:4051");
		this.timeoutMs = options.timeoutMs ?? 1_000;
		this.failMode = options.failMode ?? "fail_closed";
		this.apiKey = options.apiKey;
		this.fetchImpl = options.fetch ?? globalThis.fetch;
		if (!this.fetchImpl) {
			throw new NoetherError("No fetch implementation available");
		}
	}

	async authorize(request: AuthorizeRequest): Promise<AuthorizeDecision> {
		try {
			return await this.postJson<AuthorizeDecision>("/v1/authorize", request);
		} catch (error) {
			if (isNoetherAuthFailure(error)) {
				throw error;
			}
			return syntheticDecision(this.failMode, error);
		}
	}

	async requireAuthorization(request: AuthorizeRequest): Promise<AuthorizeDecision> {
		const decision = await this.authorize(request);
		if (decision.outcome === "deny") {
			throw new NoetherDeniedError(decision);
		}
		return decision;
	}

	async finalize(reservationId: string, payload: FinalizeReservation): Promise<Reservation> {
		return this.postJson<Reservation>(`/v1/reservations/${encodeURIComponent(reservationId)}/finalize`, payload);
	}

	async event(event: TraceEvent): Promise<{ accepted: true }> {
		return this.postJson<{ accepted: true }>("/v1/events", event);
	}

	async health(): Promise<HealthResponse> {
		return this.getJson<HealthResponse>("/health");
	}

	async withDecision<T>(
		request: AuthorizeRequest,
		run: (context: WithDecisionContext) => Promise<T> | T,
	): Promise<T> {
		const decision = await this.requireAuthorization(request);
		return run({ decision, client: this });
	}

	private async getJson<T>(path: string): Promise<T> {
		const response = await this.request(path, { method: "GET" });
		return decodeJson<T>(response);
	}

	private async postJson<T>(path: string, body: unknown): Promise<T> {
		const response = await this.request(path, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(body),
		});
		return decodeJson<T>(response);
	}

	private async request(path: string, init: RequestInit): Promise<Response> {
		const controller = new AbortController();
		const timeout = setTimeout(() => controller.abort(), this.timeoutMs);
		const headers = new Headers(init.headers);
		if (this.apiKey) {
			headers.set("Authorization", `Bearer ${this.apiKey}`);
		}
		try {
			const response = await this.fetchImpl(`${this.url}${path}`, {
				...init,
				headers,
				signal: controller.signal,
			});
			if (!response.ok) {
				throw new NoetherHttpError(response.status, await response.text());
			}
			return response;
		} catch (error) {
			if (error instanceof NoetherError) {
				throw error;
			}
			throw new NoetherError("Noether request failed", error);
		} finally {
			clearTimeout(timeout);
		}
	}
}

async function decodeJson<T>(response: Response): Promise<T> {
	return (await response.json()) as T;
}

function stripTrailingSlash(value: string): string {
	return value.endsWith("/") ? value.slice(0, -1) : value;
}

function syntheticDecision(failMode: FailMode, error: unknown): AuthorizeDecision {
	const outcome: DecisionOutcome = failMode === "fail_open" ? "allow" : "deny";
	const action: PolicyAction = failMode === "fail_open" ? "allow" : "block";
	const severity: DecisionSeverity = failMode === "fail_open" ? "warn" : "deny";
	return {
		decision_id: `sdk-${failMode}`,
		outcome,
		action,
		explanations: [
			{
				rule_id: "sdk.sidecar_unavailable",
				reason: `Noether sidecar unavailable; applying ${failMode}`,
				severity,
			},
		],
		created_at: new Date().toISOString(),
	};
}
