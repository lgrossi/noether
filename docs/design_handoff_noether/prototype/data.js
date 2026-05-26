/* Realistic Noether dataset.
   ~120 runs across 7 days, 3 agents, 5 projects.
   Each run has a decision driven by the current policy rules.
   We model: cost, retries, tools_per_turn, model, tool_calls (with names).
   Then we let policy.evaluate(run) return {decision, rule, reason}. */

(function () {
  // ---------- agents / projects / models ----------
  const AGENTS = [
    { id: "pi", name: "pi" },
    { id: "claude-code", name: "claude-code" },
    { id: "codex", name: "codex" },
  ];
  const PROJECTS = ["api", "search", "editor", "incident", "labs"];
  const MODELS = [
    { id: "claude-sonnet-4", family: "anthropic/sonnet-4", cost: 1.0 },
    { id: "claude-opus-4", family: "anthropic/opus-4", cost: 3.0 },
    { id: "gpt-4.1", family: "openai/gpt-4.1", cost: 2.2 },
    { id: "gpt-3.5", family: "openai/gpt-3.5", cost: 0.2 },
  ];

  // ---------- deterministic RNG so the data is stable ----------
  function mulberry32(seed) {
    let a = seed >>> 0;
    return function () {
      a |= 0; a = (a + 0x6D2B79F5) | 0;
      let t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  const rnd = mulberry32(42);

  // ---------- generate runs ----------
  // 7 days back from "now" = Fri May 22 2026 12:30
  const now = new Date(2026, 4, 22, 12, 30, 0);
  function daysBack(n) {
    return new Date(now.getTime() - n * 24 * 3600 * 1000);
  }

  function pick(arr) { return arr[Math.floor(rnd() * arr.length)]; }
  function range(min, max) { return min + rnd() * (max - min); }
  function intRange(min, max) { return Math.floor(range(min, max + 1)); }

  const PURPOSES = {
    "api": [
      "refactor auth middleware",
      "security triage spike",
      "vendor sdk integration",
      "test sweep",
      "session token rotation",
      "schema migration check",
      "review changes",
    ],
    "search": [
      "index migration plan",
      "search runup",
      "ranker tuning",
      "embeddings rebuild",
    ],
    "editor": [
      "release notes draft",
      "dashboard polish",
      "doc edit",
      "cmdk redesign sketch",
    ],
    "incident": [
      "agent runaway loop",
      "shell.exec on incident",
      "rollback dry-run",
      "log triage",
    ],
    "labs": [
      "long context experiment",
      "tool-loop sandbox",
      "eval grid",
    ],
  };

  function makeRuns() {
    const runs = [];
    let id = 0x1a00;
    // generate weighted by day — most activity recent
    const dayCounts = [22, 19, 18, 16, 14, 13, 12]; // today→7d ago
    for (let d = 0; d < 7; d++) {
      const day = daysBack(d);
      const n = dayCounts[d];
      for (let i = 0; i < n; i++) {
        const hour = intRange(8, 22);
        const minute = intRange(0, 59);
        const dt = new Date(day);
        dt.setHours(hour, minute, intRange(0, 59), 0);
        // skip a few in the future if today
        if (d === 0 && dt > now) continue;

        const agent = pick(AGENTS).id;
        const project = pick(PROJECTS);
        const purpose = pick(PURPOSES[project]);
        let model = pick(MODELS);
        // bias models: pi tends to sonnet-4
        if (agent === "pi" && rnd() < 0.7) model = MODELS[0];
        if (agent === "codex" && rnd() < 0.6) model = MODELS[2];

        const toolsPerTurn = Math.max(1, Math.round(range(1, 18)));
        const retries = rnd() < 0.85 ? intRange(0, 2) : intRange(3, 5);
        const toolCount = intRange(3, 1500);
        const tokens = intRange(20000, 2_500_000);
        const baseCost = (tokens / 1e6) * model.cost * range(0.25, 0.55);
        let cost = Math.round(baseCost * 100) / 100;
        // occasional runaway — most of the time keep it small
        let runaway = false;
        if (rnd() < 0.035) { cost = Math.round(range(6, 14) * 100) / 100; runaway = true; }

        // synthesize tool call types — keep ask-worthy tools rare so the tail mostly allows
        const toolCalls = [];
        if (project === "incident" && rnd() < 0.18) toolCalls.push("shell.exec");
        if (rnd() < 0.025) toolCalls.push("fs.delete");
        if (rnd() < 0.04) toolCalls.push("net.fetch");
        toolCalls.push("read", "write", "search");

        runs.push({
          id: "req-" + (id++).toString(16),
          dt,
          agent,
          project,
          purpose,
          model: model.id,
          tools: toolCount,
          tokens,
          toolsPerTurn,
          retries,
          cost,
          runaway,
          toolCalls,
          // requested but not yet decided?
          status: "settled",
        });
      }
    }
    // sort newest first
    runs.sort((a, b) => b.dt - a.dt);

    // pin one "pending ask" at the very top — pi shell.exec on incident
    runs.unshift({
      id: "req-1a4b",
      dt: new Date(now.getTime() - 26 * 60 * 1000),
      agent: "pi",
      project: "incident",
      purpose: 'shell.exec("rm -rf node_modules && pnpm i") — waiting on you',
      model: "claude-sonnet-4",
      tools: 18,
      tokens: 312_000,
      toolsPerTurn: 4,
      retries: 4,
      cost: 0.93,
      runaway: false,
      toolCalls: ["shell.exec"],
      status: "pending",
    });

    // pin one famous denied runaway 6h ago
    runs.splice(8, 0, {
      id: "req-1a49",
      dt: new Date(now.getTime() - 18 * 3600 * 1000),
      agent: "pi",
      project: "search",
      purpose: "search runup — tool loop, prompt grew 2.4×",
      model: "gpt-4.1",
      tools: 2714,
      tokens: 93_200_000,
      toolsPerTurn: 9,
      retries: 11,
      cost: 58.35,
      runaway: true,
      toolCalls: ["read", "search", "write", "net.fetch"],
      status: "settled",
    });

    return runs;
  }

  // ---------- policy ----------
  const DEFAULT_POLICY = {
    defaults: { decision_mode: "warn", attribute_to: "project" },
    models: {
      allow: ["anthropic/*", "openai/gpt-4*"],
      deny: ["*/gpt-3.5*"],
    },
    budgets: [
      { id: "personal-local", window: "30d", cap_usd: 100, on_exhaust: "block" },
    ],
    limits: {
      request_cost_usd: 3.0,
      tools_per_turn: 12,
      retries: 3,
    },
    tools: { ask_for: ["shell.exec", "fs.delete", "net.*"] },
    enforced: false,
  };

  function matchGlob(glob, str) {
    const re = new RegExp("^" + glob.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*") + "$");
    return re.test(str);
  }
  function matchAny(globs, str) { return globs.some(g => matchGlob(g, str)); }

  // Evaluate a single run against a policy.
  // Returns { decision: 'allow'|'warn'|'deny'|'ask', rule, reason, blockedCost }
  function evaluateRun(run, policy, runningSpend) {
    // model deny
    const modelFamily = (window.NOETHER_MODEL_FAMILIES || {})[run.model] || run.model;
    if (matchAny(policy.models.deny || [], modelFamily)) {
      return { decision: "deny", rule: "models.deny", line: 8, reason: `model ${run.model} is denied`, blockedCost: run.cost };
    }
    if (!matchAny(policy.models.allow || [], modelFamily)) {
      return { decision: "warn", rule: "models.allow", line: 7, reason: `model ${run.model} not on allow list`, blockedCost: 0 };
    }

    // tools.ask_for
    if (run.toolCalls && policy.tools && policy.tools.ask_for) {
      const askMatch = run.toolCalls.find(t => matchAny(policy.tools.ask_for, t));
      if (askMatch) {
        return { decision: "ask", rule: "tools.ask_for", line: 22, reason: `tool ${askMatch} requires approval`, blockedCost: 0 };
      }
    }

    // budget cap
    const budget = (policy.budgets || [])[0];
    if (budget && runningSpend + run.cost > budget.cap_usd && budget.on_exhaust === "block") {
      return { decision: "deny", rule: `budgets.${budget.id}.cap_usd`, line: 13, reason: `budget ${budget.id} would exceed cap ($${budget.cap_usd})`, blockedCost: run.cost };
    }

    // retries
    if (policy.limits && run.retries > policy.limits.retries) {
      return { decision: "deny", rule: "limits.retries", line: 19, reason: `retries=${run.retries} over ${policy.limits.retries}`, blockedCost: run.cost };
    }

    // request_cost_usd
    if (policy.limits && run.cost > policy.limits.request_cost_usd) {
      return { decision: "warn", rule: "limits.request_cost_usd", line: 17, reason: `cost $${run.cost.toFixed(2)} > $${policy.limits.request_cost_usd}`, blockedCost: 0 };
    }
    // tools per turn
    if (policy.limits && run.toolsPerTurn > policy.limits.tools_per_turn) {
      return { decision: "warn", rule: "limits.tools_per_turn", line: 18, reason: `tools/turn=${run.toolsPerTurn} > ${policy.limits.tools_per_turn}`, blockedCost: 0 };
    }
    return { decision: "allow", rule: null, line: null, reason: "ok", blockedCost: 0 };
  }

  // Evaluate all runs against a policy. Returns array of {run, eval} and totals.
  function evaluateAll(runs, policy) {
    let spend30d = 0;
    // last 30d for budget; we approximate with all current runs
    const sorted = [...runs].sort((a, b) => a.dt - b.dt);
    const results = new Map();
    for (const run of sorted) {
      const e = evaluateRun(run, policy, spend30d);
      if (e.decision === "allow" || e.decision === "warn") spend30d += run.cost;
      results.set(run.id, e);
    }
    // tally by rule
    const tally = {};
    let totalSpend = 0, prevented = 0, denied = 0, warned = 0, asked = 0, allowed = 0;
    for (const run of runs) {
      const e = results.get(run.id);
      if (e.rule) {
        const k = e.rule;
        tally[k] = tally[k] || { allow: 0, warn: 0, deny: 0, ask: 0, blockedCost: 0 };
        tally[k][e.decision] = (tally[k][e.decision] || 0) + 1;
        tally[k].blockedCost += e.blockedCost;
      }
      if (e.decision === "deny") { denied++; prevented += e.blockedCost; }
      else if (e.decision === "warn") { warned++; totalSpend += run.cost; }
      else if (e.decision === "ask")  { asked++; }
      else { allowed++; totalSpend += run.cost; }
    }
    return { results, tally, totals: { totalSpend, prevented, denied, warned, asked, allowed, spend30d } };
  }

  // ---------- export ----------
  const RUNS = makeRuns();
  const POLICY = DEFAULT_POLICY;

  window.NOETHER_MODEL_FAMILIES = {
    "claude-sonnet-4": "anthropic/sonnet-4",
    "claude-opus-4":   "anthropic/opus-4",
    "gpt-4.1":         "openai/gpt-4.1",
    "gpt-3.5":         "openai/gpt-3.5",
  };

  window.NOETHER_DATA = { RUNS, AGENTS, PROJECTS, MODELS, DEFAULT_POLICY: POLICY };
  window.NOETHER_EVAL = { evaluateRun, evaluateAll, matchGlob, matchAny };
  window.NOETHER_NOW = now;
})();
