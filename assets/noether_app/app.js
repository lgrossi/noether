const modes = new Set(["policy", "runs", "replay"]);
const state = {
  policy: null,
  runs: null,
  replay: null,
  policySource: "",
  policyEditorDirty: false,
  runsFilter: { decision: "any", rule: "any", q: "" },
};
const $ = (selector) => document.querySelector(selector);
const money = (value) => `$${Number(value || 0).toFixed(2)}`;
const deltaMoney = (value) => `${Number(value || 0) >= 0 ? "+" : "-"}${money(Math.abs(Number(value || 0)))}`;
const cost = (value) => value == null ? "—" : money(value);
const number = (value) => new Intl.NumberFormat().format(Number(value || 0));
const glyphs = { allow: "●", warn: "▲", deny: "✕", ask: "?" };
const classes = { allow: "ok", warn: "warn", deny: "deny", ask: "ask" };
let runFilterTimer = null;

function html(value) {
  return String(value ?? "").replace(/[&<>"']/g, (char) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[char]));
}

async function json(path, options = {}) {
  const response = await fetch(path, {
    headers: { Accept: "application/json", ...(options.headers || {}) },
    ...options,
  });
  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;
  if (!response.ok) throw new Error(payload?.error || `${path} returned ${response.status}`);
  return payload;
}

async function fetchRuns({ reset = false } = {}) {
  const offset = reset || !state.runs ? 0 : state.runs.next_offset;
  const params = new URLSearchParams({
    limit: "80",
    offset: String(offset || 0),
  });
  if (state.runsFilter.decision !== "any") params.set("decision", state.runsFilter.decision);
  if (state.runsFilter.rule !== "any") params.set("rule", state.runsFilter.rule);
  if (state.runsFilter.q.trim()) params.set("q", state.runsFilter.q.trim());

  const page = await json(`/v1/app/runs?${params.toString()}`);
  if (!reset && state.runs) {
    return {
      ...page,
      runs: [...state.runs.runs, ...page.runs],
    };
  }
  return page;
}

function policyEditorSource(policy) {
  return policy?.proposal?.source || policy?.source || "";
}

function storePolicy(policy, { resetEditor = false } = {}) {
  state.policy = policy;
  if (resetEditor || (!state.policyEditorDirty && !state.policySource)) {
    state.policySource = policyEditorSource(policy);
  }
}

function modeFromPath() {
  const segment = location.pathname.replace(/^\/+/, "").split("/")[0];
  return modes.has(segment) ? segment : "policy";
}

function showMode(mode, replace = false) {
  const next = modes.has(mode) ? mode : "policy";
  document.querySelectorAll("[data-surface]").forEach((surface) => {
    surface.classList.toggle("active", surface.dataset.surface === next);
  });
  document.querySelectorAll("[data-mode]").forEach((link) => {
    link.classList.toggle("active", link.dataset.mode === next);
    link.setAttribute("aria-current", link.dataset.mode === next ? "true" : "false");
  });
  const path = next === "policy" ? "/policy" : `/${next}`;
  if (location.pathname !== path) history[replace ? "replaceState" : "pushState"]({ mode: next }, "", path);
  load(next).catch(renderError);
}

async function load(mode, force = false) {
  if (mode === "policy" && (!state.policy || force)) {
    storePolicy(await json("/v1/app/policy"), { resetEditor: true });
    if (!state.runs) state.runs = await fetchRuns({ reset: true });
  }
  if (mode === "runs" && (!state.runs || force)) {
    if (!state.policy) storePolicy(await json("/v1/app/policy"));
    state.runs = await fetchRuns({ reset: true });
  }
  if (mode === "replay" && (!state.replay || force)) {
    if (!state.policy) {
      storePolicy(await json("/v1/app/policy"));
      renderTopStatus();
    }
    state.replay = await json("/v1/app/replay");
  }
  if (mode === "policy" && state.policy && !state.policyEditorDirty && !state.policySource) {
    state.policySource = policyEditorSource(state.policy);
  }
  if (mode === "policy" && state.policy) {
    renderPolicy();
  }
  if (mode === "runs" && state.runs) {
    renderRuns();
  }
  if (mode === "replay" && state.replay) {
    renderReplay();
  }
}

function renderTopStatus() {
  const top = $("[data-top-status]");
  if (!top || !state.policy) return;
  const enforced = state.policy.decision_mode === "enforce";
  const changed = hasDraftChanges(state.policy);
  top.classList.toggle("on", enforced);
  top.innerHTML = `<span class="pip"></span><span>${changed ? "draft pending" : state.policy.decision_mode}</span>`;
}

function renderPolicy() {
  const data = state.policy;
  const totals = state.runs?.totals || { runs: 0, allow: 0, warn: 0, deny: 0, ask: 0 };
  const draftChanged = hasDraftChanges(data);
  const changeCount = draftChanged ? countChangedLines(data.source || "", data.proposal?.source || "") : 0;
  renderTopStatus();
  $("[data-policy-status]").innerHTML = `
    <div class="big">${number(data.rule_stats.length)} rules</div>
    <div class="sub">${draftChanged ? `${number(changeCount)} changed lines · replay before enforce` : `${data.decision_mode} · decisions logged`}</div>
  `;
  $("[data-policy-path]").textContent = data.path || "in-memory policy";
  $("[data-policy-title]").textContent = data.path ? data.path.split("/").pop() : "policy";
  $("[data-policy-draft-badge]").textContent = draftChanged ? `modified · ${number(changeCount)} lines` : "";
  $("[data-policy-source]").value = state.policySource;
  $("[data-policy-state]").innerHTML = draftChanged
    ? `<span class="state-dot draft"></span><span>draft pending</span>`
    : `<span class="state-dot ${data.decision_mode === "enforce" ? "enforce" : "draft"}"></span><span>${html(data.decision_mode)}</span>`;
  const enforceButton = $("[data-policy-enforce]");
  if (enforceButton) {
    enforceButton.disabled = !draftChanged;
    enforceButton.textContent = draftChanged ? "Enforce draft" : "No draft to enforce";
  }
  const discardButton = $("[data-policy-discard]");
  if (discardButton) {
    discardButton.disabled = !data.proposal;
  }
  const revertButton = $("[data-policy-revert]");
  if (revertButton) {
    revertButton.disabled = !state.policyEditorDirty && state.policySource === (data.source || "");
  }
  $("[data-policy-save-state]").textContent = draftChanged
    ? `Draft has ${number(changeCount)} changed lines. Replay before enforcing.`
    : `${data.decision_mode === "enforce" ? "Enforced policy is active" : "Dry-run policy is active"}. Edit and save to create a draft.`;
  $("[data-tail-summary]").innerHTML = `
    <span><b>${number(totals.runs)}</b> in ledger</span>
    <span><b style="color:var(--ok)">${number(totals.allow)}</b> allow</span>
    <span><b style="color:var(--warn)">${number(totals.warn)}</b> warn</span>
    <span><b style="color:var(--deny)">${number(totals.deny)}</b> deny</span>
    <span><b style="color:var(--accent)">${number(totals.ask)}</b> ask</span>
    <span style="margin-left:auto;cursor:pointer" data-link-button="/runs">→ open in Runs</span>
  `;
  renderLiveTail();
  renderRuleStats();
  renderSuggestions();
  renderPolicyHighlight();
  renderRunRuleOptions();
  $("[data-policy-next]").innerHTML = data.proposal
    ? (draftChanged
      ? `Draft saved at <span class="mono">${html(data.proposal.path.split("/").pop())}</span> with ${number(changeCount)} changed lines. Replay before enforcing.`
      : `Draft file matches the enforced policy. Edit policy and save to create a pending draft.`)
    : `No draft yet. Edit policy, save a draft, then replay against ${number(totals.runs)} recorded decisions.`;
}

function hasDraftChanges(policy) {
  return Boolean(policy?.proposal && (policy.proposal.source || "") !== (policy.source || ""));
}

function countChangedLines(active, draft) {
  return policyLineDiffRows(active, draft).filter((row) => row.added || row.removed.length).length;
}

function renderLiveTail() {
  const runs = (state.runs?.runs || []).slice(0, 12);
  $("[data-live-tail]").innerHTML = runs.map((run, index) => {
    const cls = classes[run.decision] || "ok";
    return `
      <div class="tail-row is-${html(run.decision)} ${index === 0 ? "is-new" : ""}">
        <span class="t">${clock(run.occurred_at)}</span>
        <span class="glyph ${cls}">${glyphs[run.decision] || "·"}</span>
        <span class="what">${html(run.model || run.provider || "agent")} · ${html(prettySummary(run))} <span class="ref">· ${html(run.rule || "unattributed")}</span></span>
        <span class="cost">${run.decision === "deny" ? "blocked" : run.decision === "ask" ? "waiting" : cost(run.cost_usd)}</span>
      </div>
    `;
  }).join("") || `<div class="empty">No decisions recorded yet.</div>`;
}

function renderRuleStats() {
  const max = Math.max(1, ...state.policy.rule_stats.map((stat) => stat.allow + stat.warn + stat.deny + stat.ask));
  $("[data-rule-stats]").innerHTML = state.policy.rule_stats.map((stat) => {
    const total = stat.allow + stat.warn + stat.deny + stat.ask;
    const severity = stat.deny ? "deny" : stat.warn || stat.ask || stat.limit_hits ? "warn" : "";
    return `
      <div class="rule-row ${severity}">
        <span class="dot"></span>
        <span class="name">${html(stat.rule)} <span class="loc">${number(total)} hits</span></span>
        <span class="bar">
          <span class="ok" style="width:${(stat.allow / max) * 100}%"></span>
          <span class="warn" style="width:${(stat.warn / max) * 100}%"></span>
          <span class="deny" style="width:${(stat.deny / max) * 100}%"></span>
          <span class="ask" style="width:${(stat.ask / max) * 100}%"></span>
        </span>
        <span class="counts"><b>${number(stat.allow)}</b> allow · <b>${number(stat.deny)}</b> deny · ${number(stat.limit_hits)} limits</span>
      </div>
    `;
  }).join("") || `<div class="empty">No rule evidence yet.</div>`;
}

function renderPolicyHighlight() {
  const target = $("[data-policy-highlight]");
  if (!target) return;
  target.innerHTML = policyLineDiffRows(state.policy?.source || "", state.policySource).map((row, index) => {
    const line = row.line;
    const changed = row.added || row.removed.length > 0;
    const marker = row.added
      ? `<span class="diff-marker plus">+</span>`
      : row.removed.length
        ? `<span class="diff-marker minus">-</span>`
        : "";
    const beforeHint = row.removed.length
      ? `<span class="diff-before">${row.removed.map((removed) => `<span>- ${html(removed || " ")}</span>`).join("")}</span>`
      : "";
    return `<span class="hl-line ${changed ? "changed" : ""}"><span class="gutter">${index + 1}</span><span class="code">${marker}<span class="draft-code">${highlightYaml(line) || " "}</span>${beforeHint}</span></span>`;
  }).join("");
}

function policyLineDiffRows(active, draft) {
  const activeLines = active.split("\n");
  const draftLines = draft.split("\n");
  const lcs = Array.from({ length: activeLines.length + 1 }, () => Array(draftLines.length + 1).fill(0));
  for (let i = activeLines.length - 1; i >= 0; i -= 1) {
    for (let j = draftLines.length - 1; j >= 0; j -= 1) {
      lcs[i][j] = activeLines[i] === draftLines[j]
        ? lcs[i + 1][j + 1] + 1
        : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }

  const rows = [];
  let removed = [];
  let i = 0;
  let j = 0;
  const pushRow = (line, added) => {
    rows.push({ line, added, removed });
    removed = [];
  };
  while (i < activeLines.length || j < draftLines.length) {
    if (i < activeLines.length && j < draftLines.length && activeLines[i] === draftLines[j]) {
      pushRow(draftLines[j], false);
      i += 1;
      j += 1;
    } else if (i < activeLines.length && (j === draftLines.length || lcs[i + 1][j] >= lcs[i][j + 1])) {
      removed.push(activeLines[i]);
      i += 1;
    } else if (j < draftLines.length) {
      pushRow(draftLines[j], true);
      j += 1;
    }
  }
  if (removed.length) {
    if (rows.length) {
      rows[rows.length - 1].removed.push(...removed);
    } else {
      rows.push({ line: "", added: false, removed });
    }
  }
  return rows;
}

function syncPolicyHighlightScroll(editor) {
  const target = $("[data-policy-highlight]");
  if (!target) return;
  target.style.transform = `translate(${-editor.scrollLeft}px, ${-editor.scrollTop}px)`;
}

function highlightYaml(line) {
  const escaped = html(line);
  if (/^\s*#/.test(line)) return `<span class="yc">${escaped}</span>`;
  const key = line.match(/^(\s*-?\s*)([A-Za-z0-9_.-]+)(:)(.*)$/);
  if (key) {
    return `${html(key[1])}<span class="yk">${html(key[2])}</span>${html(key[3])}${highlightYamlValue(key[4])}`;
  }
  return highlightYamlValue(line);
}

function highlightYamlValue(value) {
  const parts = [];
  const pattern = /("[^"]*"|'[^']*'|-?\d+(?:\.\d+)?\b|\b(?:allow|warn|deny|block|ask|true|false|null)\b)/g;
  let last = 0;
  for (const match of value.matchAll(pattern)) {
    parts.push(html(value.slice(last, match.index)));
    const token = match[0];
    const cls = /^["']/.test(token) ? "ys" : /^-?\d/.test(token) ? "yn" : "yflag";
    parts.push(`<span class="${cls}">${html(token)}</span>`);
    last = match.index + token.length;
  }
  parts.push(html(value.slice(last)));
  return parts.join("");
}

function renderSuggestions() {
  $("[data-policy-suggestions]").innerHTML = state.policy.suggestions.map((suggestion) => `
    <div class="suggest">
      <span class="glyph">⌁</span>
      <div class="body">
        <p><b>${html(suggestion.title)}</b> ${html(suggestion.body)}</p>
        ${renderSuggestionEvidence(suggestion)}
      </div>
      <div style="display:flex;flex-direction:column;gap:6px">
        <button class="btn" type="button" data-link-button="/runs">Open runs</button>
        <button class="btn primary" type="button" data-link-button="/replay">View replay</button>
        ${suggestion.apply_label ? `<button class="btn primary" type="button" data-apply-suggestion="${html(suggestion.id)}">${html(suggestion.apply_label)}</button>` : ""}
      </div>
    </div>
  `).join("") || "";
}

function renderSuggestionEvidence(suggestion) {
  const evidence = suggestion.evidence || [];
  if (!evidence.length) return `<div class="sub">Open the runs first; replay only after editing the policy draft.</div>`;
  return `
    <div class="evidence-list">
      ${evidence.map((line) => `<span>${html(line)}</span>`).join("")}
    </div>
  `;
}

async function savePolicy() {
  const status = $("[data-policy-save-state]");
  status.textContent = "Saving draft...";
  try {
    storePolicy(await json("/v1/app/policy/proposal", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ source: state.policySource }),
    }), { resetEditor: true });
    state.policyEditorDirty = false;
    state.replay = null;
    renderPolicy();
    status.textContent = hasDraftChanges(state.policy)
      ? "Draft saved. Replay before enforcing."
      : "No policy changes to save.";
  } catch (error) {
    status.textContent = error.message;
  }
}

async function enforcePolicy() {
  const status = $("[data-policy-save-state]");
  status.textContent = "Enforcing draft...";
  try {
    storePolicy(await json("/v1/app/policy/enforce", { method: "POST" }), { resetEditor: true });
    state.policyEditorDirty = false;
    state.replay = null;
    renderPolicy();
    status.textContent = "Draft enforced.";
  } catch (error) {
    status.textContent = error.message;
  }
}

function revertPolicyEditor() {
  state.policySource = state.policy?.source || "";
  state.policyEditorDirty = false;
  renderPolicy();
}

async function discardPolicyDraft() {
  const status = $("[data-policy-save-state]");
  status.textContent = "Discarding draft...";
  try {
    storePolicy(await json("/v1/app/policy/proposal", { method: "DELETE" }), { resetEditor: true });
    state.policyEditorDirty = false;
    state.replay = null;
    renderPolicy();
    status.textContent = "Saved draft discarded.";
  } catch (error) {
    status.textContent = error.message;
  }
}

async function applySuggestion(id) {
  const response = await json(`/v1/app/policy/suggestions/${encodeURIComponent(id)}/apply`, {
    method: "POST",
  });
  storePolicy(response.policy, { resetEditor: true });
  state.policyEditorDirty = false;
  state.replay = null;
  renderPolicy();
}

function renderRuns() {
  const { totals, runs, filtered_total, next_offset } = state.runs;
  renderRunRuleOptions();
  syncRunFilterControls();
  $("[data-runs-status]").innerHTML = `<div class="big">${money(totals.spend_usd)}</div><div class="sub">${number(filtered_total)} matching · ${number(totals.runs)} total</div>`;
  const groups = groupRunsByDay(runs);
  $("[data-runs-list]").innerHTML = groups.map(([day, dayRuns]) => {
    const daySpend = dayRuns.reduce((sum, run) => sum + Number(run.cost_usd || 0), 0);
    return `
      <div class="runs-day"><span>${html(day)}</span><span class="total">${money(daySpend)} · ${number(dayRuns.length)} runs</span></div>
      ${dayRuns.map(renderRunRow).join("")}
    `;
  }).join("") + `
    <div class="runs-foot">
      <span><b>${number(runs.length)}</b> shown · <b>${number(filtered_total)}</b> matching · <b>${number(totals.tokens)}</b> tokens total</span>
      <span class="grow"></span>
      ${next_offset == null ? "" : `<button class="btn primary" type="button" data-runs-more>Load more</button>`}
      <button class="btn" type="button">Export ledger (soon)</button>
    </div>
  `;
}

function renderRunRow(run) {
  const cls = classes[run.decision] || "ok";
  const project = run.matched_entity || (run.entities || []).find((entity) => entity.startsWith("project:")) || "unattributed";
  return `
    <div class="run-row is-${html(run.decision)}" data-run-id="${html(run.id)}">
      <span class="when">${clock(run.occurred_at)}</span>
      <span class="glyph ${cls}">${glyphs[run.decision] || "·"}</span>
      <span class="what"><span class="agent">${html(run.provider || "noet")}</span><span class="purpose">${html(prettySummary(run))}</span></span>
      <span class="meta">${html(project)}${run.limit_hits ? ` · ${number(run.limit_hits)} limits` : ""}</span>
      <span class="meta model">${html(run.model || "model?")}</span>
      <span class="cost ${cls === "deny" ? "deny" : cls === "warn" ? "warn" : ""}">${run.decision === "ask" ? "—" : cost(run.cost_usd)}</span>
    </div>
  `;
}

function renderRunRuleOptions() {
  const select = document.querySelector('[data-runs-filter="rule"]');
  if (!select || !state.policy) return;
  const current = state.runsFilter.rule;
  const rules = state.policy.rule_stats.map((stat) => stat.rule).sort();
  select.innerHTML = `<option value="any">any</option>${rules.map((rule) => `<option value="${html(rule)}">${html(rule)}</option>`).join("")}`;
  select.value = rules.includes(current) ? current : "any";
}

function syncRunFilterControls() {
  const decision = document.querySelector('[data-runs-filter="decision"]');
  const rule = document.querySelector('[data-runs-filter="rule"]');
  const q = document.querySelector('[data-runs-filter="q"]');
  if (decision) decision.value = state.runsFilter.decision;
  if (rule) rule.value = state.runsFilter.rule;
  if (q && q.value !== state.runsFilter.q) q.value = state.runsFilter.q;
  const status = $("[data-runs-filter-state]");
  if (!status) return;
  const active = [
    state.runsFilter.decision !== "any" ? `decision:${state.runsFilter.decision}` : null,
    state.runsFilter.rule !== "any" ? `rule:${state.runsFilter.rule}` : null,
    state.runsFilter.q ? `q:${state.runsFilter.q}` : null,
  ].filter(Boolean);
  status.textContent = active.length ? active.join(" · ") : "/ to search";
}

async function reloadRuns() {
  state.runs = await fetchRuns({ reset: true });
  renderRuns();
  if (state.policy) {
    renderLiveTail();
  }
}

async function loadMoreRuns() {
  if (!state.runs?.next_offset) return;
  state.runs = await fetchRuns();
  renderRuns();
}

async function openRunDrawer(runId) {
  const cached = state.runs?.runs.find((item) => item.id === runId || item.agent_run_id === runId || item.trace_id === runId);
  const run = await json(`/v1/app/runs/${encodeURIComponent(cached?.id || runId)}`).catch(() => cached);
  if (!run) return;
  ensureRunDrawer();
  const cls = classes[run.decision] || "ok";
  $("[data-run-drawer-body]").innerHTML = `
    <div class="modal" data-run-modal>
      <div class="modal-head">
        <div>
          <div style="display:flex;align-items:baseline;gap:10px">
            <h2>${html(runTitle(run))}</h2>
            <span class="pill ${cls === "ok" ? "allow" : cls}"><span class="ball"></span>${html(run.decision)}</span>
          </div>
          <div class="meta" style="margin-top:6px">
            <span class="mono" style="margin-right:8px">${html(run.agent_run_id || run.id)}</span>
            · ${html(run.provider || "noet")} · ${html(run.matched_entity || "unattributed")} · ${html(run.model || "model?")} · ${html(new Date(run.occurred_at).toLocaleString())}
          </div>
        </div>
        <button class="close" type="button" data-run-drawer-close aria-label="close">×</button>
      </div>

      <div class="modal-body">
        <div class="rd-stats">
          <div class="rd-stat"><div class="k">cost</div><div class="v ${run.decision === "deny" ? "bad" : ""}">${cost(run.cost_usd)}</div></div>
          <div class="rd-stat"><div class="k">tokens</div><div class="v">${tokenLabel(run)}</div></div>
          <div class="rd-stat"><div class="k">tool calls</div><div class="v ${Number(run.tool_calls || 0) > 1000 ? "warn" : ""}">${run.tool_calls == null ? "—" : number(run.tool_calls)}</div></div>
          <div class="rd-stat"><div class="k">limits</div><div class="v ${run.limit_hits ? "bad" : ""}">${number(run.limit_hits)}</div></div>
        </div>

        <div class="rule-fired ${run.decision === "deny" ? "deny" : ""}">
          <div class="mono eyebrow">rule fired</div>
          <div><span class="mono">${html(run.rule || "unattributed")}</span><span> — ${html(ruleReason(run))}</span></div>
        </div>

        <div class="eyebrow" style="margin-bottom:8px">Timeline</div>
        <div class="rd-timeline">
          ${timelineRows(run)}
        </div>

        ${runDetailChips(run)}
      </div>

      <div class="modal-foot">
        <button class="btn" type="button" data-link-button="/policy">Open Policy</button>
        <span class="grow"></span>
        <button class="btn" type="button" data-link-button="/replay">Open Replay</button>
        ${run.trace_id ? `<a class="btn" href="/v1/reports/traces/${encodeURIComponent(run.trace_id)}" target="_blank" rel="noreferrer">Trace JSON</a>` : ""}
        <button class="btn primary" type="button" data-run-drawer-close>Close</button>
      </div>
    </div>
  `;
  $("[data-run-drawer-backdrop]").classList.add("open");
}

function closeRunDrawer() {
  $("[data-run-drawer-backdrop]")?.remove();
}

function ensureRunDrawer() {
  if ($("[data-run-drawer-backdrop]")) return;
  document.body.insertAdjacentHTML("beforeend", `
    <div class="scrim run-modal-scrim" data-run-drawer-backdrop><div data-run-drawer-body></div></div>
  `);
  const backdrop = $("[data-run-drawer-backdrop]");
  backdrop.addEventListener("click", (event) => {
    if (event.target === backdrop || event.target.closest("[data-run-drawer-close]")) closeRunDrawer();
  });
}

function runTitle(run) {
  const project = (run.entities || []).find((entity) => entity.startsWith("project:"))?.replace("project:", "");
  const model = run.model || run.provider || "agent run";
  return `${project || run.matched_entity || "agent"} — ${model}`;
}

function tokenLabel(run) {
  const tokens = run.actual_tokens || run.estimated_tokens;
  if (tokens == null) return "—";
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return number(tokens);
}

function ruleReason(run) {
  const decision = latestDecisionFields(run);
  if (decision.model_check === "denied" || run.model_check === "denied") return "provider/model is not allowed by budget";
  if (decision.binding_limit) return `limit hit: ${decision.binding_limit}`;
  if (decision.limit_hits) return `limit hit: ${decision.limit_hits}`;
  if (decision.rejected_reason) return decision.rejected_reason;
  if (run.decision_reason) return run.decision_reason;
  if (run.limit_hits) return `${number(run.limit_hits)} limit hit${run.limit_hits === 1 ? "" : "s"}`;
  if (run.decision === "allow") return "request matched policy and budget";
  if (run.decision === "deny") return "request was blocked by policy";
  if (run.decision === "ask") return "policy requires approval";
  return "decision recorded by policy";
}

function latestDecisionFields(run) {
  const decision = [...(run.timeline || [])].reverse().find((item) => item.kind?.startsWith("decision."));
  return decision?.fields || {};
}

function timelineRows(run) {
  const items = (run.timeline || []).map((item, index) => ({
    ...item,
    index,
    timestamp: Date.parse(item.occurred_at),
  }));
  if (!items.length) return `<div class="empty">No timeline events were recorded for this run.</div>`;
  const groups = timelineEventGroups(items);
  const times = items.map((item) => item.timestamp).filter(Number.isFinite);
  const start = Math.min(...times);
  const end = Math.max(...times);
  return `
    <div class="rd-time-axis">
      <span>start</span>
      <span>${html(durationLabel(end - start))}</span>
      <span>end</span>
    </div>
    ${groups.map((group) => `
      <details class="rd-group">
        <summary class="rd-row">
          <span><b>${html(group.label)}</b></span>
          <span class="bar">
            ${group.events.map((item) => `
              <span
                class="rd-event-segment"
                style="left:${timelinePosition(item, start, end)}%"
                title="${html(`${new Date(item.occurred_at).toLocaleTimeString()} · ${group.label} · ${item.kind}`)}"
              ></span>
            `).join("")}
          </span>
          <span style="text-align:right">${number(group.count)} event${group.count === 1 ? "" : "s"}</span>
        </summary>
        <div class="rd-events">
          ${group.events.map((item) => timelineEventCard(item, start)).join("")}
        </div>
      </details>
    `).join("")}
  `;
}

function timelineEventGroups(items) {
  const groups = new Map();
  for (const item of items) {
    const label = timelineEventLabel(item);
    const group = groups.get(label) || { label, count: 0, events: [] };
    group.count += 1;
    group.events.push(item);
    groups.set(label, group);
  }
  return Array.from(groups.values());
}

function timelinePosition(item, start, end) {
  if (!Number.isFinite(item.timestamp) || !Number.isFinite(start) || !Number.isFinite(end) || end <= start) {
    return 50;
  }
  return Math.max(0, Math.min(100, ((item.timestamp - start) / (end - start)) * 100));
}

function durationLabel(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "instant";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
  return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`;
}

function timelineEventLabel(item) {
  const kind = item.kind || "";
  if (kind === "pi.agent_context") return "Agent context";
  if (kind === "pi.authorize") return "Budget checks";
  if (kind === "pi.authorize_error") return "Budget check errors";
  if (kind.startsWith("decision.")) return "Recorded decisions";
  if (kind === "pi.provider_call.started") return "Model requests";
  if (kind === "pi.message_end") return "Model responses";
  if (kind === "pi.usage" || kind.includes("usage")) return "Usage records";
  if (kind === "pi.stream_summary") return "Stream updates";
  if (kind === "pi.tool_call") return "Tool calls";
  if (kind === "tool.observed" || kind === "pi.tool_observed") return "Tool results";
  if (kind === "pi.turn_end") return "Turns completed";
  return kind.replace(/^pi\./, "").replace(/[._-]+/g, " ").replace(/\b\w/g, (char) => char.toUpperCase()) || "Other events";
}

function timelineEventCard(item, start) {
  const fields = item.fields || {};
  const title = timelineEventTitle(item);
  const facts = timelineEventFacts(item);
  const offset = eventOffsetLabel(item, start);
  return `
    <article class="rd-event-card">
      <div class="rd-event-head">
        <span class="mono">${html(offset)}</span>
        <strong>${html(title)}</strong>
        <span class="mono">${html(item.kind)}</span>
      </div>
      <div class="rd-event-facts">
        ${facts.length ? facts.map((fact) => eventFact(fact[0], fact[1], fact[2])).join("") : eventFact("summary", item.summary)}
      </div>
      <details class="rd-raw">
        <summary>raw</summary>
        <pre>${html(item.summary || JSON.stringify(fields))}</pre>
      </details>
    </article>
  `;
}

function timelineEventTitle(item) {
  const f = item.fields || {};
  if ((item.kind || "").startsWith("decision.")) return `${f.action || item.kind.replace("decision.", "")} · ${f.selected_budget || f.rejected_budget || "policy"}`;
  if (item.kind === "pi.agent_context") return f.cwd || "agent context";
  if (item.kind === "pi.provider_call.started") return `${f.provider || "provider"}/${f.model || "model"}`;
  if (item.kind === "pi.authorize") return `${f.source || "noether"} budget check`;
  if (item.kind === "pi.message_end") return `${f.provider || "provider"}/${f.model || "model"} response`;
  if (item.kind === "pi.tool_call") return `${f.tool_name || "tool"} call`;
  if (item.kind === "tool.observed" || item.kind === "pi.tool_observed") return `${f.name || "tool"} result`;
  if ((item.kind || "").includes("usage")) return `${f.provider || "provider"}/${f.model || "model"} usage`;
  if (item.kind === "pi.turn_end") return `turn ${f.turn ?? "?"} completed`;
  if (item.kind === "pi.stream_summary") return "stream update";
  return timelineEventLabel(item);
}

function timelineEventFacts(item) {
  const f = item.fields || {};
  const shape = parseShape(f.shape);
  if ((item.kind || "").startsWith("decision.")) {
    return presentFacts([
      ["action", f.action],
      ["budget", f.selected_budget || f.rejected_budget],
      ["model", f.model],
      ["estimated", f.estimated_cost ? money(f.estimated_cost) : null],
      ["remaining", f.budget_window_remaining ? money(f.budget_window_remaining) : null],
      ["request", compactId(f.request), f.request],
      ["decision", compactId(f.decision_id), f.decision_id],
    ]);
  }
  if (item.kind === "pi.agent_context") {
    return presentFacts([
      ["cwd", f.cwd],
      ["tools", f.selected_tools_count || f.selected_tools],
      ["skills", f.skills_count || f.skills],
      ["context files", f.context_files_count || f.context_files],
      ["sample tools", f.tool_names],
      ["sample skills", f.skill_names],
    ]);
  }
  if (item.kind === "pi.provider_call.started") {
    return presentFacts([
      ["model", [f.provider, f.model].filter(Boolean).join("/")],
      ["context", f.context_usage_pct ? `${Number(f.context_usage_pct).toFixed(1)}%` : null],
      ["tokens", f.context_tokens],
      ["inputs", shape.input_count],
      ["tools", shape.tools_count],
      ["reasoning", shape.reasoning_effort],
      ["verbosity", shape.text_verbosity],
      ["call", compactId(f.provider_call), f.provider_call],
    ]);
  }
  if (item.kind === "pi.authorize" || item.kind === "pi.authorize_error") {
    return presentFacts([
      ["source", f.source],
      ["decision", compactId(f.decision), f.decision],
      ["reservation", compactId(f.reservation), f.reservation],
      ["request", compactId(f.request), f.request],
      ["provider call", compactId(f.provider_call), f.provider_call],
    ]);
  }
  if (item.kind === "pi.message_end") {
    return presentFacts([
      ["tokens", f.tokens],
      ["input", f.input_tokens],
      ["output", f.output_tokens],
      ["cache read", f.cache_read_tokens],
      ["cost", f.cost ? money(f.cost) : null],
      ["stop", f.stop],
      ["call", compactId(f.provider_call), f.provider_call],
    ]);
  }
  if (item.kind === "pi.tool_call") {
    const missing = f.tool_name === "exec_command"
      ? "command not captured"
      : f.tool_name === "skill"
        ? "skill name not captured"
        : null;
    return presentFacts([
      ["tool", f.tool_name],
      ["detail", missing],
      ["call", compactId(f.tool_call_id), f.tool_call_id],
      ["provider call", compactId(f.provider_call), f.provider_call],
      ["attribution", f.attribution],
    ]);
  }
  if (item.kind === "tool.observed" || item.kind === "pi.tool_observed") {
    return presentFacts([
      ["tool", f.name],
      ["success", f.success],
      ["duration", f.duration_ms ? `${number(f.duration_ms)}ms` : null],
      ["call", compactId(f.tool_call_id), f.tool_call_id],
      ["provider call", compactId(f.provider_call), f.provider_call],
    ]);
  }
  if ((item.kind || "").includes("usage")) {
    return presentFacts([
      ["tokens", f.total_tokens || f.tokens],
      ["input", f.input_tokens],
      ["output", f.output_tokens],
      ["cache read", f.cache_read_tokens],
      ["cost", f.cost ? money(f.cost) : null],
      ["stop", f.stop],
    ]);
  }
  if (item.kind === "pi.stream_summary") {
    return presentFacts([
      ["deltas", f.deltas],
      ["tool calls", f.tool_calls],
      ["provider call", compactId(f.provider_call), f.provider_call],
    ]);
  }
  if (item.kind === "pi.turn_end") {
    return presentFacts([
      ["turn", f.turn],
      ["model", [f.provider, f.model].filter(Boolean).join("/")],
      ["tokens", f.usage_tokens],
      ["cost", f.usage_cost ? money(f.usage_cost) : null],
    ]);
  }
  return presentFacts(Object.entries(f).slice(0, 6).map(([key, value]) => [key, value]));
}

function presentFacts(facts) {
  return facts.filter((fact) => fact[1] != null && fact[1] !== "");
}

function eventFact(label, value, title) {
  return `<span title="${html(title || value)}"><b>${html(label)}</b>${html(value)}</span>`;
}

function parseShape(shape) {
  if (!shape) return {};
  return Object.fromEntries(String(shape).split(",").map((part) => part.split("=")).filter((pair) => pair.length === 2));
}

function compactId(value) {
  const text = String(value || "");
  if (!text) return "";
  if (text.length <= 14) return text;
  return `${text.slice(0, 8)}…${text.slice(-4)}`;
}

function eventOffsetLabel(item, start) {
  const delta = Number.isFinite(item.timestamp) && Number.isFinite(start) ? item.timestamp - start : 0;
  return `+${durationLabel(delta)}`;
}

function runDetailChips(run) {
  const tools = toolAndSkillChips(run);
  const runContext = runContextChips(run);
  return `
    <div class="rd-sections">
      ${chipSection("Tools used", tools, "No tool events recorded.")}
      ${chipSection("Entities", (run.entities || []).map((entity) => ({ label: entity })), "No entities recorded.")}
      ${chipSection("Run context", runContext, "No run context recorded.")}
    </div>
  `;
}

function chipSection(title, chips, empty) {
  return `
    <section class="rd-chip-section">
      <div class="eyebrow">${html(title)}</div>
      <div class="timeline-tags">
        ${chips.length ? chips.map((chip) => `<span class="mono" title="${html(chip.title || chip.label)}">${html(chip.label)}</span>`).join("") : `<em>${html(empty)}</em>`}
      </div>
    </section>
  `;
}

function toolAndSkillChips(run) {
  const items = run.timeline || [];
  const tools = new Map();
  for (const item of items) {
    if (!/tool/.test(item.kind)) continue;
    const name = item.fields?.tool_name || item.fields?.name || item.kind;
    const record = tools.get(name) || { name, calls: 0, results: 0, ok: 0, failed: 0, duration: 0 };
    if (item.kind === "pi.tool_call") record.calls += 1;
    if (item.kind === "tool.observed" || item.kind === "pi.tool_observed") {
      record.results += 1;
      if (item.fields?.success === "true") record.ok += 1;
      if (item.fields?.success === "false") record.failed += 1;
      record.duration += Number(item.fields?.duration_ms || 0);
    }
    tools.set(name, record);
  }
  const chips = Array.from(tools.values()).map((tool) => {
    const count = tool.calls || tool.results;
    const details = [
      count ? `${count} call${count === 1 ? "" : "s"}` : null,
      tool.results ? `${tool.ok}/${tool.results} ok` : null,
      tool.failed ? `${tool.failed} failed` : null,
      tool.duration ? `${tool.duration}ms total` : null,
    ].filter(Boolean);
    return { label: details.length ? `${tool.name} · ${details[0]}` : tool.name, title: details.join(" · ") || tool.name };
  });
  return chips.slice(0, 16);
}

function runContextChips(run) {
  const items = run.timeline || [];
  const context = items.find((item) => item.kind === "pi.agent_context")?.fields || {};
  const model = items.find((item) => /provider_call|message_end|usage/.test(item.kind))?.fields || {};
  return [
    context.cwd ? { label: `cwd ${context.cwd}` } : null,
    (context.selected_tools_count || context.selected_tools) ? { label: `${context.selected_tools_count || context.selected_tools} tools in context` } : null,
    (context.skills_count || context.skills) ? { label: `${context.skills_count || context.skills} skills in context` } : null,
    context.skill_names ? { label: `context skills ${context.skill_names}` } : null,
    context.tool_names ? { label: `context tools ${context.tool_names}` } : null,
    model.context_usage_pct ? { label: `context ${Number(model.context_usage_pct).toFixed(1)}%` } : null,
    model.stop ? { label: `stop ${model.stop}` } : null,
  ].filter(Boolean);
}

function kv(key, value) {
  return `<div class="kv"><dt>${html(key)}</dt><dd>${html(value)}</dd></div>`;
}

function renderReplay() {
  const { baseline, proposal, has_proposed_policy, message, history_window_days } = state.replay;
  const windowLabel = history_window_days === 1 ? "last 24 hours" : history_window_days ? `last ${number(history_window_days)} days` : "local history";
  $("[data-replay-status]").innerHTML = `<div class="big">local ledger</div><div class="sub">${number(baseline.runs)} real runs · ${money(baseline.spend_usd)} baseline · ${html(windowLabel)}</div>`;
  if (!has_proposed_policy) {
    $("[data-replay-body]").innerHTML = `
      <div class="replay-empty">
        <div class="title">No proposed changes yet.</div>
        <p>Edit Policy and save a draft to see the proposed card and diff here.</p>
      </div>
    `;
    return;
  }
  const changed = proposal?.changed_lines || 0;
  const hasPolicyDiff = changed > 0;
  const changedRuns = proposal?.changed_runs || [];
  const recommendations = proposal?.recommendations || [];
  const proposed = proposal?.proposed || baseline;
  const delta = proposal?.spend_delta_usd || 0;
  if (!hasPolicyDiff) {
    $("[data-replay-body]").innerHTML = `
      <div class="scenarios">
        <div class="scenario">
          <div class="h"><span class="name">recorded history</span><span class="tag">${html(windowLabel)}</span><span class="ribbon"><span class="pill"><span class="ball"></span>baseline</span></span></div>
          <p class="desc">What Noether actually recorded in the replay window.</p>
          ${scenarioStats(baseline)}
        </div>
        <div class="scenario">
          <div class="h"><span class="name">no policy diff</span><span class="tag">draft matches current</span></div>
          <p class="desc">The saved draft is identical to the active policy, so there is nothing meaningful to simulate or enforce.</p>
          <div class="diff diff-list"><span class="same">Edit Policy and save a changed draft to compare outcomes over ${html(windowLabel)}.</span></div>
        </div>
      </div>
      <div class="reco">
        <div>
          <div class="verdict">No policy changes to replay</div>
          <div class="sub">Replay compares the active policy with a changed draft; it does not compare current policy against itself.</div>
        </div>
        <div class="right">
          <button class="btn primary" type="button" data-link-button="/policy">Edit policy</button>
        </div>
      </div>
    `;
    return;
  }
  const verdict = changedRuns.length
    ? `${number(changedRuns.length)} historical runs would change`
    : "No historical runs would change";
  const deltaLabel = deltaMoney(delta);
  const diffNote = html(proposal?.explanation || `This compares the active policy to the saved draft over ${windowLabel}.`);
  $("[data-replay-body]").innerHTML = `
    <div class="scenarios">
      <div class="scenario">
        <div class="h"><span class="name">recorded history</span><span class="tag">${html(windowLabel)}</span><span class="ribbon"><span class="pill"><span class="ball"></span>baseline</span></span></div>
        <p class="desc">What Noether actually recorded in the replay window.</p>
        ${scenarioStats(baseline)}
      </div>
      <div class="scenario winner">
        <div class="h"><span class="name">draft impact</span><span class="tag">${number(changed)} changed lines</span><span class="ribbon"><span class="pill allow"><span class="ball"></span>replayed</span></span></div>
        <p class="desc">${html(message)}</p>
        <div class="diff diff-list">${renderDiffPreview(proposal)}</div>
        ${scenarioStats(proposed)}
      </div>
    </div>
    <div class="reco">
      <div>
        <div class="verdict">${html(verdict)} · spend delta <b>${html(deltaLabel)}</b></div>
        <div class="sub">${diffNote}</div>
      </div>
      <div class="right">
        <button class="btn" type="button" data-link-button="/policy">${changed ? "Open diff in Policy" : "Open policy"}</button>
        ${proposal?.can_enforce ? `<button class="btn primary" type="button" data-policy-enforce>Adopt &amp; save →</button>` : ""}
      </div>
    </div>
    <div class="card changed-runs">
      <div class="eyebrow">impact summary</div>
      ${renderReplayImpact(changedRuns, delta)}
    </div>
    <div class="card changed-runs">
      <div class="eyebrow">recommendation</div>
      ${renderReplayRecommendations(recommendations)}
    </div>
    <div class="card changed-runs">
      <div class="eyebrow">example affected runs</div>
      ${renderChangedRuns(changedRuns)}
    </div>
  `;
}

function scenarioStats(totals) {
  return `
    <div class="stats">
      <div><div class="k">spent</div><div class="v">${money(totals.spend_usd)}</div></div>
      <div><div class="k">tokens</div><div class="v">${number(totals.tokens)}</div></div>
      <div><div class="k">allowed</div><div class="v decision allow">${number(totals.allow)}</div></div>
      <div><div class="k">warnings</div><div class="v decision warn">${number(totals.warn)}</div></div>
      <div><div class="k">denied</div><div class="v decision deny">${number(totals.deny)}</div></div>
      <div><div class="k">asked</div><div class="v decision ask">${number(totals.ask)}</div></div>
      <div><div class="k">limit hits</div><div class="v">${number(totals.limit_hits)}</div></div>
    </div>
  `;
}

function renderDiffPreview(proposal) {
  const lines = proposal?.preview || [];
  if (!lines.length) return `<span class="same">No pending policy edit. The changed runs below are a backtest: current policy behavior applied to older recorded outcomes.</span>`;
  return lines.map((line) => {
    const cls = line.kind === "added" ? "plus" : "minus";
    const sign = line.kind === "added" ? "+" : "-";
    return `<span class="${cls}">${sign} ${html(line.line)}</span>`;
  }).join("");
}

function renderChangedRuns(runs) {
  if (!runs.length) return `<div class="empty">No recorded run decisions would change under this draft.</div>`;
  return runs.slice(0, 12).map((run) => `
    <button class="changed-run" type="button" data-run-id="${html(run.run_id)}">
      <div>
        <div><span class="mono">${html(run.run_id)}</span> ${decisionTransition(run.from, run.to)}</div>
        <div class="sub">${html(run.summary || "unattributed")} ${run.rule ? `· caused by ${html(run.rule)}` : ""}</div>
      </div>
      <div class="right">${money(run.cost_usd)}</div>
    </button>
  `).join("");
}

function decisionTransition(from, to) {
  return `<span class="transition">${decisionPill(from)}<span class="arrow">→</span>${decisionPill(to)}</span>`;
}

function decisionPill(decision) {
  const cls = classes[decision] === "ok" ? "allow" : classes[decision] || decision;
  return `<span class="pill ${html(cls)}"><span class="ball"></span>${html(decision)}</span>`;
}

function renderReplayImpact(runs, delta) {
  if (!runs.length) {
    return `<div class="empty">The draft does not change any recorded outcomes in this window.</div>`;
  }
  const transitions = countBy(runs, (run) => `${run.from} → ${run.to}`);
  const rules = countBy(runs, (run) => run.rule || "unattributed");
  const topTransitions = topEntries(transitions, 4);
  const topRules = topEntries(rules, 4);
  const blocked = runs.filter((run) => run.to === "deny" && run.from !== "deny");
  const loosened = runs.filter((run) => run.from === "deny" && run.to !== "deny");
  return `
    <div class="recommendation">
      <div><b>${number(runs.length)} recorded outcome${runs.length === 1 ? "" : "s"} would change</b></div>
      <div class="sub">
        ${blocked.length ? `${number(blocked.length)} newly blocked. ` : ""}
        ${loosened.length ? `${number(loosened.length)} newly allowed/warned. ` : ""}
        Projected spend changes by ${deltaMoney(delta)}.
      </div>
      <div class="timeline-tags">
        ${topTransitions.map(([label, count]) => `<span class="mono">${html(label)} · ${number(count)}</span>`).join("")}
      </div>
      <div class="timeline-tags">
        ${topRules.map(([label, count]) => `<span class="mono">${html(label)} · ${number(count)}</span>`).join("")}
      </div>
    </div>
  `;
}

function countBy(items, keyFn) {
  const counts = new Map();
  for (const item of items) {
    const key = keyFn(item);
    counts.set(key, (counts.get(key) || 0) + 1);
  }
  return counts;
}

function topEntries(map, limit) {
  return Array.from(map.entries()).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).slice(0, limit);
}

function renderReplayRecommendations(recommendations) {
  if (!recommendations.length) return `<div class="empty">No replay recommendation available.</div>`;
  return recommendations.map((rec) => `
    <div class="recommendation">
      <div><b>${html(rec.title)}</b></div>
      <div class="sub">${html(rec.body)}</div>
      ${rec.rule ? `<div class="mono">${html(rec.rule)}</div>` : ""}
    </div>
  `).join("");
}

function groupRunsByDay(runs) {
  const groups = new Map();
  for (const run of runs) {
    const key = new Date(run.occurred_at).toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" });
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(run);
  }
  return Array.from(groups.entries());
}

function prettySummary(run) {
  const trace = run.trace_id ? `trace ${run.trace_id.slice(0, 8)}` : run.id.slice(0, 8);
  return `${run.decision} · ${run.rule || "unattributed"} · ${trace}`;
}

function clock(value) {
  return new Date(value).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function renderError(error) {
  const active = document.querySelector(".surface.active");
  if (active) active.insertAdjacentHTML("beforeend", `<div class="card empty">${html(error.message)}</div>`);
}

document.addEventListener("click", (event) => {
  if (event.target.closest("[data-run-drawer-close]")) {
    closeRunDrawer();
    return;
  }
  if (event.target.matches("[data-run-drawer-backdrop]")) {
    closeRunDrawer();
    return;
  }
  const link = event.target.closest("[data-link]");
  if (link) {
    event.preventDefault();
    showMode(link.getAttribute("href").replace("/", "") || "policy");
    return;
  }
  const buttonLink = event.target.closest("[data-link-button]");
  if (buttonLink) {
    closeRunDrawer();
    showMode(buttonLink.dataset.linkButton.replace("/", "") || "policy");
    return;
  }
  const runRow = event.target.closest("[data-run-id]");
  if (runRow) {
    openRunDrawer(runRow.dataset.runId).catch(renderError);
    return;
  }
  if (event.target.closest("[data-policy-save]")) {
    savePolicy();
    return;
  }
  if (event.target.closest("[data-policy-revert]")) {
    revertPolicyEditor();
    return;
  }
  if (event.target.closest("[data-policy-discard]")) {
    discardPolicyDraft();
    return;
  }
  if (event.target.closest("[data-policy-enforce]")) {
    enforcePolicy();
    return;
  }
  const applyButton = event.target.closest("[data-apply-suggestion]");
  if (applyButton) {
    applySuggestion(applyButton.dataset.applySuggestion).catch(renderError);
    return;
  }
  if (event.target.closest("[data-refresh-replay]")) {
    load("replay", true);
    return;
  }
  if (event.target.closest("[data-runs-more]")) {
    loadMoreRuns();
    return;
  }
  if (event.target.closest("[data-runs-clear]")) {
    state.runsFilter = { decision: "any", rule: "any", q: "" };
    reloadRuns().catch(renderError);
    return;
  }
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeRunDrawer();
});

document.addEventListener("input", (event) => {
  if (event.target.matches("[data-policy-source]")) {
    state.policySource = event.target.value;
    state.policyEditorDirty = true;
    renderPolicyHighlight();
    syncPolicyHighlightScroll(event.target);
  }
  if (event.target.matches("[data-runs-filter]")) {
    state.runsFilter[event.target.dataset.runsFilter] = event.target.value;
    clearTimeout(runFilterTimer);
    runFilterTimer = setTimeout(() => reloadRuns().catch(renderError), 180);
  }
});

document.addEventListener("change", (event) => {
  if (event.target.matches("[data-runs-filter]")) {
    state.runsFilter[event.target.dataset.runsFilter] = event.target.value;
    reloadRuns().catch(renderError);
  }
});

document.addEventListener("scroll", (event) => {
  if (event.target.matches?.("[data-policy-source]")) syncPolicyHighlightScroll(event.target);
}, true);

window.addEventListener("popstate", () => showMode(modeFromPath(), true));
showMode(modeFromPath(), true);
