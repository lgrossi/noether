const bootstrap = window.NOETHER_DASHBOARD_BOOTSTRAP || {};

const state = {
  page: bootstrap.selectedPage || "overview",
  filters: {
    window: "30d",
    lens: "project",
    entity: "",
    trace: bootstrap.selectedTrace || "",
    simulation: bootstrap.selectedSimulation || "",
  },
  selectedBudget: "",
  selectedAdoption: "",
  selectedStrategy: "",
  selectedTraceSpan: "",
  traceRequestScrollTop: 0,
  traceSpansScrollTop: 0,
  traceLoading: false,
  strategyObjective: "balanced",
  budgetSort: "pressure",
  adoptionSort: "coachable",
  traceView: "recent",
};

const cache = {
  overview: null,
  budgets: null,
  adoption: null,
  traces: null,
  strategy: null,
};

const moneyCompact = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  notation: "compact",
  maximumFractionDigits: 1,
});
const numberCompact = new Intl.NumberFormat("en-US", {
  notation: "compact",
  maximumFractionDigits: 1,
});
const numberFull = new Intl.NumberFormat("en-US");
const TRACE_REQUEST_ROW_HEIGHT = 30;
const TRACE_REQUEST_OVERSCAN = 10;

function el(id) {
  return document.getElementById(id);
}

const lensMetaMap = {
  project: { singular: "Project", plural: "Projects" },
  user: { singular: "User", plural: "Users" },
  team: { singular: "Team", plural: "Teams" },
  company: { singular: "Company", plural: "Companies" },
  workflow: { singular: "Workflow", plural: "Workflows" },
  surface: { singular: "Surface", plural: "Surfaces" },
  budget: { singular: "Budget", plural: "Budgets" },
  model: { singular: "Model", plural: "Models" },
};

function clamp(value, min, max) {
  return Math.min(Math.max(value, min), max);
}

function sum(values) {
  return values.reduce((acc, value) => acc + Number(value || 0), 0);
}

function maxValue(values, fallback = 1) {
  const numeric = values.map((value) => Number(value || 0)).filter((value) => Number.isFinite(value));
  return numeric.length ? Math.max(...numeric, fallback) : fallback;
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function money(value) {
  const numeric = Number(value || 0);
  if (Math.abs(numeric) >= 1000) return moneyCompact.format(numeric);
  if (Math.abs(numeric) < 0.01 && numeric !== 0) return `$${numeric.toFixed(4)}`;
  return `$${numeric.toFixed(2)}`;
}

function signedMoney(value) {
  const numeric = Number(value || 0);
  if (numeric === 0) return "$0.00";
  return `${numeric > 0 ? "+" : "-"}${money(Math.abs(numeric))}`;
}

function number(value) {
  const numeric = Number(value || 0);
  return Math.abs(numeric) >= 1000 ? numberCompact.format(numeric) : numberFull.format(numeric);
}

function pct(value, digits = 0) {
  return `${Number(value || 0).toFixed(digits)}%`;
}

function shortDate(value) {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function shortDay(value) {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleDateString([], { month: "short", day: "numeric" });
}

function toneClass(value) {
  const text = String(value || "").toLowerCase();
  if (text.includes("deny") || text.includes("limit") || text.includes("risk") || text.includes("critical")) return "bad";
  if (text.includes("warn") || text.includes("pressure") || text.includes("tool") || text.includes("intervention")) return "warn";
  if (text.includes("healthy") || text.includes("stable") || text.includes("opportunity") || text.includes("protected")) return "good";
  return "accent";
}

function emptyState(message) {
  return `<div class="empty-state">${escapeHtml(message)}</div>`;
}

function summaryPairs(text) {
  const pairs = {};
  for (const match of String(text || "").matchAll(/([a-zA-Z_]+)=([^=\s]+)/g)) {
    pairs[match[1]] = match[2];
  }
  return pairs;
}

function prettyTraceSummary(event) {
  const text = String(event?.summary || "");
  const pairs = summaryPairs(text);
  if (event?.kind?.startsWith("decision.")) {
    return [
      event.kind.replace("decision.", ""),
      pairs.model ? pairs.model.replace("openai/", "").replace("anthropic/", "") : "",
      pairs.estimated_cost ? money(pairs.estimated_cost) : "",
      pairs.selected_budget ? `budget ${pairs.selected_budget}` : "",
    ].filter(Boolean).join(" · ");
  }
  if (event?.kind === "usage.finalized") {
    return [
      pairs.model ? pairs.model.replace("openai/", "").replace("anthropic/", "") : "",
      pairs.total_tokens ? `${number(pairs.total_tokens)} tokens` : "",
      pairs.cost ? money(pairs.cost) : "",
      pairs.cache_read_tokens ? `${number(pairs.cache_read_tokens)} cache read` : "",
    ].filter(Boolean).join(" · ");
  }
  if (event?.kind === "tool.observed") {
    return [
      pairs.name || "tool",
      pairs.duration_ms ? `${pairs.duration_ms}ms` : "",
      pairs.success ? (pairs.success === "true" ? "ok" : "failed") : "",
    ].filter(Boolean).join(" · ");
  }
  return text;
}

function queryForDashboard(includeSimulation = false) {
  const params = new URLSearchParams();
  if (state.page !== "strategy") {
    if (state.filters.window) params.set("window", state.filters.window);
    if (state.filters.lens) params.set("lens", state.filters.lens);
    if (state.filters.entity) params.set("entity", state.filters.entity);
    if (state.page === "traces" && state.filters.trace) params.set("trace", state.filters.trace);
  }
  if (includeSimulation && state.filters.simulation) params.set("simulation", state.filters.simulation);
  return params.toString();
}

async function fetchJson(path, includeSimulation = false) {
  const query = queryForDashboard(includeSimulation);
  const response = await fetch(`${path}?${query}`, { headers: { Accept: "application/json" } });
  if (!response.ok) throw new Error(`${path} failed: ${response.status}`);
  return response.json();
}

function setStatus(message) {
  el("dashboard-status").textContent = message;
}

function appShell() {
  return el("dashboard-app-shell");
}

function lensMeta(lens = state.filters.lens) {
  return lensMetaMap[lens] || { singular: "Entity", plural: "Entities" };
}

function workspaceTitleFor(page) {
  return page === "strategy" ? "Simulation" : page.charAt(0).toUpperCase() + page.slice(1);
}

function setWorkspaceTitle(page) {
  const node = el("dashboard-workspace-title");
  if (node) node.textContent = workspaceTitleFor(page);
}

function setChromePills({
  windowLabel,
  lensLabel,
  sliceLabel,
  openExceptions,
} = {}) {
  if (windowLabel && el("dashboard-window-pill")) el("dashboard-window-pill").textContent = windowLabel;
  if (lensLabel && el("dashboard-lens-pill")) el("dashboard-lens-pill").textContent = lensLabel;
  if (sliceLabel && el("dashboard-slice-pill")) el("dashboard-slice-pill").textContent = sliceLabel;
  if (openExceptions != null && el("dashboard-open-exceptions")) el("dashboard-open-exceptions").textContent = String(openExceptions);
}

function setMobileNav(open) {
  const shell = appShell();
  if (!shell) return;
  shell.classList.toggle("mobile-nav-open", open);
  el("dashboard-shell-scrim")?.classList.toggle("hidden", !open);
}

function optionLabel(options, selected, fallback = selected) {
  return options.find((option) => option.id === selected)?.label || fallback || "";
}

function statusChip(label) {
  return `<span class="page-chip">${escapeHtml(label)}</span>`;
}

function railChip(label) {
  return `<span class="rail-chip">${escapeHtml(label)}</span>`;
}

function controlChip(label, active, attrs = "") {
  return `<button class="chip${active ? " active" : ""}" type="button" ${attrs}>${escapeHtml(label)}</button>`;
}

function titleCase(value) {
  return String(value || "")
    .replaceAll("_", " ")
    .replace(/\b\w/g, (match) => match.toUpperCase());
}

function sectionCopy(title, note = "") {
  return `
    <div class="section-copy">
      <h2>${escapeHtml(title)}</h2>
      ${note ? `<div class="panel-note">${escapeHtml(note)}</div>` : ""}
    </div>
  `;
}

function entityLabelFromFilters(filters) {
  return optionLabel(filters?.entities || [], filters?.selected_entity, filters?.selected_entity || "");
}

function pageScopeNote(filters, count, noun) {
  const lensId = filters?.selected_lens || state.filters.lens;
  const lensLabel = optionLabel(filters?.lenses || [], lensId, lensMeta(lensId).singular);
  const focusedEntity = entityLabelFromFilters(filters);
  if (focusedEntity) return `${lensLabel} focus · ${focusedEntity}`;
  return `${lensLabel} lens · ${number(count)} ${noun}`;
}

function pageHeading(title, meta = "") {
  return `
    <header class="page-heading">
      <h1>${escapeHtml(title)}</h1>
      ${meta ? `<div class="page-heading-meta">${meta}</div>` : ""}
    </header>
  `;
}

function actionList(items) {
  return `
    <div class="action-list">
      ${items.map((item) => `
        <div class="action-item">
          <strong>${escapeHtml(item.label)}</strong>
          <span>${escapeHtml(item.value)}</span>
        </div>
      `).join("")}
    </div>
  `;
}

function pageStrip(kicker, title, summary, chips = []) {
  return `
    <header class="page-strip">
      <div class="page-strip-copy">
        <div class="page-overline">${escapeHtml(kicker)}</div>
        <h1 class="page-title">${escapeHtml(title)}</h1>
        ${summary ? `<p class="page-summary">${escapeHtml(summary)}</p>` : ""}
      </div>
      <div class="page-strip-meta">
        ${chips.map(statusChip).join("")}
      </div>
    </header>
  `;
}

function metricStrip(cards) {
  if (!cards.length) return "";
  return `
    <section class="metric-grid">
      ${cards.map((card) => `
        <div class="metric-strip">
          <div class="metric-label">${escapeHtml(card.label)}</div>
          <div class="metric-value">${escapeHtml(card.value)}</div>
          <div class="metric-note">${escapeHtml(card.note || "")}</div>
          ${card.toneValue ? `<div class="metric-${card.toneValue ? "" : ""} metric-tone-${escapeHtml(card.tone || "accent")}">${escapeHtml(card.toneValue)}</div>` : ""}
        </div>
      `).join("")}
    </section>
  `;
}

function surface(title, note, body, bodyClass = "") {
  return `
    <section class="surface ${bodyClass}">
      <div class="surface-head">
        <div>
          <h2 class="surface-title">${escapeHtml(title)}</h2>
          ${note ? `<div class="panel-note">${escapeHtml(note)}</div>` : ""}
        </div>
      </div>
      <div class="surface-body">${body}</div>
    </section>
  `;
}

function railModule(title, note, body) {
  return `
    <section class="rail-module">
      <div class="rail-head">
        <div>
          <h3 class="rail-title">${escapeHtml(title)}</h3>
          ${note ? `<div class="panel-note">${escapeHtml(note)}</div>` : ""}
        </div>
      </div>
      <div class="rail-body">${body}</div>
    </section>
  `;
}

function evidenceBlock(title, note, body) {
  return `
    <section class="evidence-block">
      <div class="evidence-head">
        <div>
          <h3 class="evidence-title">${escapeHtml(title)}</h3>
          ${note ? `<div class="panel-note">${escapeHtml(note)}</div>` : ""}
        </div>
      </div>
      <div class="evidence-body">${body}</div>
    </section>
  `;
}

function drawSparkline(points, key, stroke = "#88a8ff", fill = "rgba(136,168,255,0.12)") {
  if (!points.length) return "";
  const width = 150;
  const height = 34;
  const max = maxValue(points.map((point) => point[key]), 1);
  const step = points.length === 1 ? width : width / (points.length - 1);
  const coords = points.map((point, index) => {
    const x = index * step;
    const y = height - 4 - ((Number(point[key] || 0) / max) * (height - 8));
    return `${x},${y}`;
  });
  const area = [`0,${height}`, ...coords, `${width},${height}`].join(" ");
  return `
    <div class="spark-shell">
      <svg viewBox="0 0 ${width} ${height}" preserveAspectRatio="none">
        <polygon points="${area}" fill="${fill}"></polygon>
        <polyline points="${coords.join(" ")}" fill="none" stroke="${stroke}" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"></polyline>
      </svg>
    </div>
  `;
}

function trendChart(points, series) {
  if (!points.length) return emptyState("No time-series data in the current slice.");
  const width = 840;
  const height = 260;
  const leftPad = 28;
  const rightPad = 14;
  const topPad = 18;
  const bottomPad = 34;
  const usableWidth = width - leftPad - rightPad;
  const usableHeight = height - topPad - bottomPad;
  const step = points.length === 1 ? usableWidth : usableWidth / (points.length - 1);
  const paths = series.map((line) => {
    const max = maxValue(points.map((point) => point[line.key]), 1);
    const coords = points.map((point, index) => {
      const x = leftPad + (index * step);
      const y = topPad + usableHeight - ((Number(point[line.key] || 0) / max) * usableHeight);
      return { x, y };
    });
    return { ...line, coords };
  });
  return `
    <div class="chart-shell">
      <svg viewBox="0 0 ${width} ${height}" preserveAspectRatio="none">
        ${[0, 0.25, 0.5, 0.75, 1].map((tick) => {
          const y = topPad + usableHeight - (tick * usableHeight);
          return `<line x1="${leftPad}" y1="${y}" x2="${width - rightPad}" y2="${y}" stroke="rgba(136,151,178,0.12)" stroke-width="1"></line>`;
        }).join("")}
        ${paths.map((line) => `
          <polyline points="${line.coords.map((coord) => `${coord.x},${coord.y}`).join(" ")}" fill="none" stroke="${line.color}" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"></polyline>
          ${line.coords.map((coord) => `<circle cx="${coord.x}" cy="${coord.y}" r="3" fill="${line.color}"></circle>`).join("")}
        `).join("")}
        ${points.map((point, index) => {
          const x = leftPad + (index * step);
          const anchor = index === 0 ? "start" : index === points.length - 1 ? "end" : "middle";
          return `<text x="${x}" y="${height - 10}" fill="#96a2be" font-size="11" text-anchor="${anchor}">${escapeHtml(point.label || "")}</text>`;
        }).join("")}
      </svg>
    </div>
    <div class="legend-row">
      ${series.map((line) => `
        <span class="legend-item">
          <span class="legend-dot" style="background:${line.color}"></span>
          ${escapeHtml(line.label)}
        </span>
      `).join("")}
    </div>
  `;
}

function inlineBar(value, max, formatter, tone = "accent") {
  const width = clamp((Number(value || 0) / Math.max(max, 1)) * 100, 0, 100);
  return `
    <span class="pill-bar">
      <span class="pill-track"><span class="pill-fill ${tone}" style="width:${width}%"></span></span>
      <span>${escapeHtml(formatter(value))}</span>
    </span>
  `;
}

function renderSharedFilters(filters) {
  const meta = lensMeta(filters.selected_lens || state.filters.lens);
  const selectedTraceValue = state.page === "traces" ? state.filters.trace : (filters.selected_trace || "");
  el("dashboard-window-select").innerHTML = (filters.windows || []).map((option) => `
    <option value="${escapeHtml(option.id)}"${option.id === filters.selected_window ? " selected" : ""}>${escapeHtml(option.label)}</option>
  `).join("");
  el("dashboard-lens-select").innerHTML = (filters.lenses || []).map((option) => `
    <option value="${escapeHtml(option.id)}"${option.id === filters.selected_lens ? " selected" : ""}>${escapeHtml(option.label)}</option>
  `).join("");
  el("dashboard-entity-select").innerHTML = [`<option value="">All ${escapeHtml(meta.plural.toLowerCase())}</option>`].concat(
    (filters.entities || []).map((option) => `
      <option value="${escapeHtml(option.value)}"${option.value === filters.selected_entity ? " selected" : ""}>${escapeHtml(option.label)} · ${money(option.spend_usd)}</option>
    `)
  ).join("");
  el("dashboard-trace-select").innerHTML = ['<option value="">All traces</option>'].concat(
    (filters.traces || []).map((option) => `
      <option value="${escapeHtml(option.trace_id)}"${option.trace_id === selectedTraceValue ? " selected" : ""}>${escapeHtml(option.trace_id)} · ${money(option.spend_usd)}</option>
    `)
  ).join("");

  state.filters.window = filters.selected_window || state.filters.window;
  state.filters.lens = filters.selected_lens || state.filters.lens;
  state.filters.entity = filters.selected_entity || "";
  if (el("dashboard-entity-label")) el("dashboard-entity-label").textContent = meta.singular;
  setChromePills({
    windowLabel: optionLabel(filters.windows || [], state.filters.window, "last 30 days"),
    lensLabel: optionLabel(filters.lenses || [], state.filters.lens, "team + project").toLowerCase(),
    sliceLabel: state.page === "traces" && state.filters.trace ? "selected trace" : "all requests",
  });
  syncControlsForPage();
}

function syncControlsForPage() {
  el("dashboard-strategy-objective-select").value = state.strategyObjective;
  const inStrategy = state.page === "strategy";
  const inTraces = state.page === "traces";
  el("dashboard-window-field")?.classList.toggle("hidden", inStrategy);
  el("dashboard-lens-field")?.classList.toggle("hidden", inStrategy);
  el("dashboard-entity-field")?.classList.toggle("hidden", inStrategy);
  el("dashboard-trace-field")?.classList.toggle("hidden", !inTraces);
  el("dashboard-simulation-field")?.classList.toggle("hidden", !inStrategy);
  el("dashboard-strategy-objective-field")?.classList.toggle("hidden", !inStrategy);
}

function showPage(page) {
  state.page = page;
  document.querySelectorAll(".page").forEach((node) => {
    node.classList.remove("is-active");
    node.classList.remove("active");
  });
  document.querySelectorAll(".nav-button[data-page]").forEach((node) => node.classList.remove("active"));
  el(`dashboard-page-${page}`).classList.add("is-active");
  el(`dashboard-page-${page}`).classList.add("active");
  document.querySelector(`.nav-button[data-page="${page}"]`)?.classList.add("active");
  setWorkspaceTitle(page);
  syncControlsForPage();
}

function overviewChips(data) {
  return [
    `${optionLabel(data.filters?.windows || [], data.filters?.selected_window)}`,
    `${optionLabel(data.filters?.lenses || [], data.filters?.selected_lens)}`,
    `${number((data.filters?.traces || []).length)} traces`,
  ];
}

function overviewMetrics(data) {
  const totalDecisions = Number(data.policy?.allow || 0) + Number(data.policy?.warn || 0) + Number(data.policy?.deny || 0);
  return [
    { label: "Spend", value: money(sum((data.spend_trend || []).map((point) => point.spend_usd))), note: "Finalized observed spend" },
    { label: "Decisions", value: number(totalDecisions), note: `${number(data.policy?.limit_hits || 0)} limit hits` },
    { label: "Cache", value: data.kpis?.find((card) => card.id === "cache")?.value || "—", note: data.kpis?.find((card) => card.id === "cache")?.delta || "" },
    { label: "Adoption", value: data.kpis?.find((card) => card.id === "adoption")?.value || "—", note: "Current slice posture" },
  ];
}

function sortTraceWatchlist(traces) {
  return [...traces].sort((left, right) => {
    return Number(right.limit_hits || 0) - Number(left.limit_hits || 0)
      || Number(right.spend_usd || 0) - Number(left.spend_usd || 0);
  });
}

function renderOverviewQueue(traces) {
  if (!traces.length) return emptyState("No exceptions in the current slice.");
  return `
    <div class="exception-list">
      ${traces.map((trace) => `
        <button class="exception-button" type="button" data-exception-trace-id="${escapeHtml(trace.trace_id)}">
          <strong>${escapeHtml(trace.trace_id)}</strong>
          <div class="rail-note">${escapeHtml(shortDate(trace.latest_at))}</div>
          <div class="table-chip-row" style="margin-top:8px">
            ${(trace.badges || []).map(railChip).join("")}
          </div>
        </button>
      `).join("")}
    </div>
  `;
}

function renderOverviewPolicy(data) {
  const lanes = [
    { label: "Allow", value: Number(data.policy?.allow || 0), tone: "good" },
    { label: "Warn", value: Number(data.policy?.warn || 0), tone: "warn" },
    { label: "Deny", value: Number(data.policy?.deny || 0), tone: "bad" },
  ];
  const max = maxValue(lanes.map((lane) => lane.value), 1);
  return `
    <div class="mini-stats" style="margin-bottom:12px">
      <div class="mini-stat"><div class="metric-label">Limit hits</div><div class="mini-value">${number(data.policy?.limit_hits || 0)}</div></div>
      <div class="mini-stat"><div class="metric-label">Lifecycle limits</div><div class="mini-value">${number(data.policy?.lifecycle_limits || 0)}</div></div>
    </div>
    <div class="rail-list">
      ${lanes.map((lane) => `
        <div class="rail-item">
          <strong>${escapeHtml(lane.label)}</strong>
          <div style="margin-top:8px">${inlineBar(lane.value, max, number, lane.tone)}</div>
        </div>
      `).join("")}
    </div>
  `;
}

function renderOverviewModelMix(rows) {
  if (!rows.length) return emptyState("No model activity.");
  const max = maxValue(rows.map((row) => row.spend_usd), 1);
  return `
    <div class="rail-list">
      ${rows.map((row) => `
        <div class="rail-item">
          <strong>${escapeHtml(row.label)}</strong>
          <div class="rail-note">${number(row.traces)} traces · ${number(row.total_tokens)} tokens</div>
          <div style="margin-top:8px">${inlineBar(row.spend_usd, max, money, "cyan")}</div>
        </div>
      `).join("")}
    </div>
  `;
}
function renderOverviewNextReviews(data, watchlist) {
  const items = [];
  for (const insight of (data.insights || []).slice(0, 3)) {
    items.push({ title: insight.title, note: insight.summary });
  }
  for (const trace of watchlist) {
    if (items.length >= 3) break;
    items.push({
      title: `Open ${trace.trace_id}`,
      note: (trace.badges || []).join(" · ") || `${money(trace.spend_usd)} · ${number(trace.total_tokens)} tokens`,
    });
  }
  if (!items.length) return emptyState("No follow-up reviews are visible.");
  return `
    <div class="rail-list">
      ${items.map((item) => `
        <div class="rail-item">
          <strong>${escapeHtml(item.title)}</strong>
          <div class="rail-note">${escapeHtml(item.note)}</div>
        </div>
      `).join("")}
    </div>
  `;
}

function renderOverviewTable(rows) {
  if (!rows.length) return emptyState("No inspection rows are available.");
  const max = maxValue(rows.map((row) => row.spend_usd), 1);
  return `
    <div class="table-shell">
      <div class="table-scroller">
        <table class="dense-table">
          <thead>
            <tr>
              <th>Entity</th>
              <th>Spend</th>
              <th>Share</th>
              <th>Tokens</th>
              <th>Traces</th>
            </tr>
          </thead>
          <tbody>
            ${rows.map((row) => `
              <tr class="clickable" data-overview-entity="${escapeHtml(row.label)}">
                <td><div class="entity-stack"><strong>${escapeHtml(row.label)}</strong></div></td>
                <td>${inlineBar(row.spend_usd, max, money, "accent")}</td>
                <td>${pct(row.share_pct)}</td>
                <td>${number(row.total_tokens)}</td>
                <td>${number(row.traces)}</td>
              </tr>
            `).join("")}
          </tbody>
        </table>
      </div>
    </div>
  `;
}

function renderOverview(data) {
  cache.overview = data;
  const watchlist = sortTraceWatchlist(data.filters?.traces || []).slice(0, 6);
  const totalSpend = sum((data.spend_trend || []).map((point) => point.spend_usd));
  setChromePills({
    openExceptions: watchlist.length,
    sliceLabel: "open only",
  });
  el("dashboard-page-overview").innerHTML = `
    <div class="dashboard-page">
      <div class="section-head">
        ${sectionCopy("Operating picture", pageScopeNote(data.filters, (data.filters?.entities || []).length, "visible groups"))}
        <div class="section-actions">
          ${controlChip("All entities", !state.filters.entity, 'data-clear-entity="1"')}
          ${controlChip("Teams", state.filters.lens === "team", 'data-filter-lens="team"')}
          ${controlChip("Projects", state.filters.lens === "project", 'data-filter-lens="project"')}
          ${controlChip("Users", state.filters.lens === "user", 'data-filter-lens="user"')}
        </div>
      </div>
      <section class="workbench-grid">
        <div class="workbench-main">
          <section class="surface">
            <div class="surface-head surface-head-split">
              <div>
                <h2 class="surface-title">Spend, activity, and decision pressure</h2>
                <div class="panel-note">30-day spend, trace volume, and decision markers</div>
              </div>
              <div class="header-stat-row">
                <div class="inline-stat"><span class="metric-label">Spend</span><span class="inline-stat-value">${money(totalSpend)}</span></div>
                <div class="inline-stat"><span class="metric-label">At risk</span><span class="inline-stat-value warn">${number(watchlist.length)}</span></div>
                <div class="inline-stat"><span class="metric-label">Denied</span><span class="inline-stat-value bad">${number(data.policy?.deny || 0)}</span></div>
              </div>
            </div>
            <div class="surface-body">
              ${trendChart(data.spend_trend || [], [
                { key: "spend_usd", label: "Spend", color: "#a8b9ff" },
                { key: "traces", label: "Traces", color: "#59df8d" },
                { key: "decisions", label: "Decision markers", color: "#f4c45e" },
              ])}
            </div>
          </section>
          ${evidenceBlock(
            "Inspection queue",
            "Ranked by spend and limit activity.",
            renderOverviewTable(data.spend_distribution || [])
          )}
        </div>
        <aside class="workbench-rail sticky">
          ${railModule("Exception queue", "High-pressure traces worth opening next.", renderOverviewQueue(watchlist))}
          ${railModule("Control posture", "Decision mix for the current slice.", renderOverviewPolicy(data))}
          ${railModule("Next reviews", "Follow-on workflow items.", renderOverviewNextReviews(data, watchlist))}
        </aside>
      </section>
    </div>
  `;

  document.querySelectorAll("[data-exception-trace-id]").forEach((button) => {
    button.addEventListener("click", () => {
      state.filters.trace = button.dataset.exceptionTraceId || "";
      showPage("traces");
      loadCurrentPage();
    });
  });

  document.querySelectorAll("[data-overview-entity]").forEach((row) => {
    row.addEventListener("click", () => {
      state.filters.entity = row.dataset.overviewEntity || "";
      loadCurrentPage();
    });
  });
  document.querySelectorAll("[data-filter-lens]").forEach((button) => {
    button.addEventListener("click", () => {
      state.filters.lens = button.dataset.filterLens || state.filters.lens;
      state.filters.entity = "";
      loadCurrentPage();
    });
  });
  document.querySelectorAll("[data-clear-entity]").forEach((button) => {
    button.addEventListener("click", () => {
      state.filters.entity = "";
      loadCurrentPage();
    });
  });
}

function ensureSelectedBudget(data) {
  const budgets = data.budgets || [];
  if (!budgets.some((budget) => budget.budget_id === state.selectedBudget)) {
    state.selectedBudget = budgets[0]?.budget_id || "";
  }
  return budgets.find((budget) => budget.budget_id === state.selectedBudget) || null;
}

function budgetsChips(data) {
  return [
    `${number((data.budgets || []).length)} budgets`,
    `${number((data.protected_adoption?.low_adopters || []).length)} low adopters`,
  ];
}

function budgetTable(data) {
  const trends = new Map((data.budget_trends || []).map((series) => [series.budget_id, series.points]));
  const sortedBudgets = [...(data.budgets || [])].sort((left, right) => {
    if (state.budgetSort === "limits") {
      return Number(right.limit_hits || 0) - Number(left.limit_hits || 0)
        || Number(right.pressure_ratio || 0) - Number(left.pressure_ratio || 0);
    }
    if (state.budgetSort === "fallbacks") {
      return Number(right.fallback_count || 0) - Number(left.fallback_count || 0)
        || Number(right.pressure_ratio || 0) - Number(left.pressure_ratio || 0);
    }
    return Number(right.pressure_ratio || 0) - Number(left.pressure_ratio || 0)
      || Number(right.spend_usd || 0) - Number(left.spend_usd || 0);
  });
  const maxSpend = maxValue(sortedBudgets.map((budget) => budget.spend_usd), 1);
  return `
    <div class="table-shell">
      <div class="table-scroller">
        <table class="dense-table">
          <thead>
            <tr>
              <th>Budget</th>
              <th>Trend</th>
              <th>Pressure</th>
              <th>Spend</th>
              <th>Budget window</th>
              <th>Projected</th>
              <th>Peak day</th>
              <th>Limit hits</th>
            </tr>
          </thead>
          <tbody>
            ${sortedBudgets.map((budget) => {
              const trend = trends.get(budget.budget_id) || [];
              const tone = budget.limit_hits > 0 ? "bad" : Number(budget.pressure_ratio || 0) >= 70 ? "warn" : "good";
              return `
                <tr class="clickable ${budget.budget_id === state.selectedBudget ? "is-selected" : ""}" data-budget-id="${escapeHtml(budget.budget_id)}">
                  <td>
                    <div class="entity-stack">
                      <strong>${escapeHtml(budget.budget_id)}</strong>
                      <div class="table-sub">${number(budget.decision_count)} decisions</div>
                    </div>
                  </td>
                  <td>${drawSparkline(trend, "spend_usd", tone === "bad" ? "#ff8c8c" : tone === "warn" ? "#f4c45e" : "#59df8d", tone === "bad" ? "rgba(255,140,140,0.12)" : tone === "warn" ? "rgba(244,196,94,0.12)" : "rgba(89,223,141,0.12)")}</td>
                  <td>${inlineBar(budget.pressure_ratio || 0, 100, (value) => pct(value), tone)}</td>
                  <td>${inlineBar(budget.spend_usd, maxSpend, money, "accent")}</td>
                  <td>${budget.budget_window_remaining_usd == null ? "n/a" : money(budget.budget_window_remaining_usd)}</td>
                  <td class="${budget.projected_delta_usd > 0 ? "warn" : "good"}">${budget.projected_delta_usd == null ? "n/a" : escapeHtml(signedMoney(budget.projected_delta_usd))}</td>
                  <td>${pct(budget.peak_day_share)}</td>
                  <td>${number(budget.limit_hits)}</td>
                </tr>
              `;
            }).join("")}
          </tbody>
        </table>
      </div>
    </div>
  `;
}

function budgetRail(data, selected) {
  if (!selected) return emptyState("No budget selected.");
  const compare = (data.budgets || []).find((budget) => budget.budget_id !== selected.budget_id);
  return `
    <div class="callout">
      <h3>Selected budget: <span class="mono">${escapeHtml(selected.budget_id)}</span></h3>
      <p>${selected.budget_window_remaining_usd == null ? "Selected budget-window room is unknown." : `${money(selected.budget_window_remaining_usd)} remains in the selected budget window. Tighter limits can still bind sooner.`}</p>
    </div>
    ${actionList([
      { label: "Why here", value: `${escapeHtml(selected.behavior_label)} with ${number(selected.decision_count)} routed decisions in the current window.` },
      { label: "What changed", value: `${number(selected.limit_hits)} limit hits and ${pct(selected.pressure_ratio || 0)} current pressure are attached to this budget.` },
      { label: "Compared to", value: compare ? `${compare.budget_id} is the next closest peer by spend and pressure.` : "No peer budget is available for comparison." },
      { label: "Next action", value: selected.projected_delta_usd > 0 ? `Review burst traces before raising the cap by ${signedMoney(selected.projected_delta_usd)}.` : "Check whether the current cap can stay flat without shifting protected room." },
    ])}
  `;
}

function budgetEvidence(data, selected) {
  const trend = (data.budget_trends || []).find((series) => series.budget_id === selected?.budget_id)?.points || [];
  const maxConcentration = maxValue((data.concentration || []).map((row) => row.spend_usd), 1);
  const low = (data.protected_adoption?.low_adopters || []).slice(0, 4);
  const high = (data.protected_adoption?.high_adopters || []).slice(0, 4);
  const userScopedProtectedPool = state.filters.lens === "user";
  return `
    <div class="evidence-grid">
      ${evidenceBlock(
        selected ? `${selected.budget_id} trend` : "Budget trend",
        "Trend stays below the table so the table remains the page lead.",
        trend.length ? trendChart(trend, [{ key: "spend_usd", label: "Spend", color: "#88a8ff" }]) : emptyState("No trend points for the selected budget.")
      )}
      ${evidenceBlock(
        "Peer concentration",
        "Read the selected budget against its peers instead of as an isolated number.",
        `<div class="rail-list">
          ${(data.concentration || []).slice(0, 6).map((row) => `
            <div class="rail-item">
              <strong>${escapeHtml(row.label)}</strong>
              <div class="rail-note">${number(row.traces)} traces</div>
              <div style="margin-top:8px">${inlineBar(row.spend_usd, maxConcentration, money, "warn")}</div>
            </div>
          `).join("")}
        </div>`
      )}
      ${evidenceBlock(
        "Protected low adopters",
        userScopedProtectedPool
          ? "Unused protected room changes how pressure should be interpreted."
          : "Protected adoption pools are user-scoped, so switch to the user lens for actionable detail.",
        userScopedProtectedPool && low.length ? `<div class="rail-list">${low.map((entry) => `
          <div class="rail-item">
            <strong>${escapeHtml(entry.entity_key)}</strong>
            <div class="rail-note">${escapeHtml(entry.budget_id)}</div>
            <div class="good" style="margin-top:6px">${money(entry.current_grant_usd)}</div>
          </div>
        `).join("")}</div>` : emptyState(userScopedProtectedPool ? "No low adopters are currently surfaced." : "Switch to the user lens to review protected-room exposure.")
      )}
      ${evidenceBlock(
        "Protected heavy consumers",
        userScopedProtectedPool
          ? "High consumers deserve separate review from overall pressure."
          : "This panel is intentionally hidden outside the user lens to keep the page aligned with the active grouping.",
        userScopedProtectedPool && high.length ? `<div class="rail-list">${high.map((entry) => `
          <div class="rail-item">
            <strong>${escapeHtml(entry.entity_key)}</strong>
            <div class="rail-note">${escapeHtml(entry.budget_id)}</div>
            <div class="warn" style="margin-top:6px">${money(entry.used_current_grant_usd)}</div>
          </div>
        `).join("")}</div>` : emptyState(userScopedProtectedPool ? "No high protected consumers are present." : "Switch to the user lens to review concentrated protected-pool usage.")
      )}
    </div>
  `;
}

function renderBudgets(data) {
  cache.budgets = data;
  const selected = ensureSelectedBudget(data);
  setChromePills({
    openExceptions: (data.budgets || []).filter((budget) => Number(budget.limit_hits || 0) > 0 || Number(budget.pressure_ratio || 0) >= 80).length,
    sliceLabel: "pressure watch",
  });
  el("dashboard-page-budgets").innerHTML = `
    <div class="dashboard-page">
      <div class="section-head">
        ${sectionCopy("Budget pressure", pageScopeNote(data.filters, (data.budgets || []).length, "ranked budgets"))}
        <div class="section-actions">
          ${controlChip("Sort by pressure", state.budgetSort === "pressure", 'data-budget-sort="pressure"')}
          ${controlChip("Sort by limit load", state.budgetSort === "limits", 'data-budget-sort="limits"')}
          ${controlChip("Sort by fallback load", state.budgetSort === "fallbacks", 'data-budget-sort="fallbacks"')}
        </div>
      </div>
      <section class="workbench-grid">
        <div class="workbench-main">
          ${surface("Budget pressure table", "Selected budget-window room, limit load, and fallback concentration.", budgetTable(data))}
          ${budgetEvidence(data, selected)}
        </div>
        <aside class="workbench-rail sticky">
          ${railModule("Selected budget", "Near-cap pools need evidence before cap changes.", budgetRail(data, selected))}
          ${railModule("Recent budget events", "What changed most recently.", renderOverviewNextReviews({ insights: data.insights || [] }, []))}
        </aside>
      </section>
    </div>
  `;

  document.querySelectorAll("[data-budget-id]").forEach((row) => {
    row.addEventListener("click", () => {
      state.selectedBudget = row.dataset.budgetId || "";
      renderBudgets(cache.budgets);
    });
  });
  document.querySelectorAll("[data-budget-sort]").forEach((button) => {
    button.addEventListener("click", () => {
      state.budgetSort = button.dataset.budgetSort || "pressure";
      renderBudgets(cache.budgets);
    });
  });
}

function adoptionChips(data) {
  return [
    `${number((data.leaderboard || []).length)} ranked entities`,
    `${number((data.low_adopters || []).length)} low adopters`,
  ];
}

function interventionScore(row) {
  return (row.health_label === "Healthy" ? 0 : 80)
    + (100 - Number(row.cache_ratio || 0))
    + (Number(row.tool_events || 0) * 2)
    + (Number(row.limit_hits || 0) * 10)
    + Number(row.spend_usd || 0);
}

function sortedAdoptionRows(rows) {
  return [...rows].sort((left, right) => {
    if (state.adoptionSort === "blocked") {
      return Number(right.limit_hits || 0) - Number(left.limit_hits || 0)
        || interventionScore(right) - interventionScore(left);
    }
    if (state.adoptionSort === "concentrated") {
      return Number(right.spend_usd || 0) - Number(left.spend_usd || 0)
        || interventionScore(right) - interventionScore(left);
    }
    return interventionScore(right) - interventionScore(left);
  });
}

function ensureSelectedAdoption(rows) {
  if (!rows.some((row) => row.entity === state.selectedAdoption)) {
    state.selectedAdoption = rows[0]?.entity || "";
  }
  return rows.find((row) => row.entity === state.selectedAdoption) || null;
}

function adoptionTable(rows) {
  const maxSpend = maxValue(rows.map((row) => row.spend_usd), 1);
  const maxToolsPerTrace = maxValue(rows.map((row) => Number(row.tool_events || 0) / Math.max(Number(row.traces || 0), 1)), 1);
  return `
    <div class="table-shell">
      <div class="table-scroller">
        <table class="dense-table">
          <thead>
            <tr>
              <th>Entity</th>
              <th>Intervention</th>
              <th>Spend</th>
              <th>Cache</th>
              <th>Tool / trace</th>
              <th>Limit hits</th>
              <th>Health</th>
            </tr>
          </thead>
          <tbody>
            ${rows.map((row) => {
              const toolsPerTrace = Number(row.tool_events || 0) / Math.max(Number(row.traces || 0), 1);
              return `
                <tr class="clickable ${row.entity === state.selectedAdoption ? "is-selected" : ""}" data-adoption-entity="${escapeHtml(row.entity)}">
                  <td><div class="entity-stack"><strong>${escapeHtml(row.entity)}</strong><div class="table-sub">${number(row.traces)} traces · ${number(row.total_tokens)} tokens</div></div></td>
                  <td>${escapeHtml(row.opportunity_label)}</td>
                  <td>${inlineBar(row.spend_usd, maxSpend, money, "accent")}</td>
                  <td>${inlineBar(row.cache_ratio, 100, (value) => pct(value), "cyan")}</td>
                  <td>${inlineBar(toolsPerTrace, maxToolsPerTrace, (value) => Number(value).toFixed(1), "warn")}</td>
                  <td>${number(row.limit_hits)}</td>
                  <td><span class="table-pill">${escapeHtml(row.health_label)}</span></td>
                </tr>
              `;
            }).join("")}
          </tbody>
        </table>
      </div>
    </div>
  `;
}

function protectedMatches(data, entity) {
  return {
    low: (data.low_adopters || []).filter((entry) => entry.entity_key === entity),
    high: (data.high_adopters || []).filter((entry) => entry.entity_key === entity),
  };
}

function adoptionRail(data, row) {
  if (!row) return emptyState("No entity selected.");
  const match = protectedMatches(data, row.entity);
  const peer = (data.leaderboard || []).find((entry) => entry.entity !== row.entity);
  return `
    <div class="callout">
      <h3>Selected subject: <span class="mono">${escapeHtml(row.entity)}</span></h3>
      <p>${escapeHtml(row.opportunity_label)}</p>
    </div>
    ${actionList([
      { label: "Why here", value: `${money(row.spend_usd)} spend with ${number(row.traces)} traces and ${pct(row.cache_ratio)} cache reuse.` },
      { label: "What changed", value: `${number(row.tool_events)} tool events and ${number(row.limit_hits)} limit hits changed the current posture.` },
      { label: "Compared to", value: peer ? `${peer.entity} is the nearest visible peer for coaching comparison.` : "No peer comparison is available in the current slice." },
      { label: "Next action", value: match.low.length ? `Use ${money(match.low[0].current_grant_usd)} protected room to coach this subject into a healthier path.` : "Coach prompt trimming and routing before adding more protected budget." },
    ])}
  `;
}

function adoptionEvidence(data, rows, selected) {
  const low = (data.low_adopters || []).slice(0, 4);
  const high = (data.high_adopters || []).slice(0, 4);
  const blockerRows = [...rows]
    .filter((row) => row.health_label !== "Healthy" || row.cache_ratio < 10 || row.limit_hits > 0)
    .slice(0, 6);
  const userScopedProtectedPool = state.filters.lens === "user";
  return `
    <div class="support-grid">
      ${evidenceBlock(
        "Protected pool roster",
        userScopedProtectedPool
          ? "Low adopters and heavy consumers stay visible under the queue."
          : "Protected adoption pools are user-scoped, so this evidence only becomes actionable in the user lens.",
        `
          <div class="rail-list">
            ${userScopedProtectedPool
              ? (low.length || high.length
                ? `${low.map((entry) => `
                  <div class="rail-item">
                    <strong>${escapeHtml(entry.entity_key)}</strong>
                    <div class="rail-note">${escapeHtml(entry.budget_id)}</div>
                    <div class="good" style="margin-top:6px">${money(entry.current_grant_usd)}</div>
                  </div>
                `).join("")}
                ${high.map((entry) => `
                  <div class="rail-item">
                    <strong>${escapeHtml(entry.entity_key)}</strong>
                    <div class="rail-note">${escapeHtml(entry.budget_id)}</div>
                    <div class="warn" style="margin-top:6px">${money(entry.used_current_grant_usd)}</div>
                  </div>
                `).join("")}`
                : emptyState("No protected adoption signals are currently surfaced."))
              : emptyState("Switch to the user lens to review protected low adopters.")}
          </div>
        `
      )}
      ${evidenceBlock(
        selected?.entity || "Coaching evidence",
        "Keep the queue primary and the friction evidence below.",
        `
          <div class="rail-list">
            ${blockerRows.map((row) => `
              <div class="rail-item">
                <strong>${escapeHtml(row.entity)}</strong>
                <div class="rail-note">${escapeHtml(row.opportunity_label)}</div>
                <div class="table-chip-row" style="margin-top:8px">
                  <span class="table-pill">${escapeHtml(row.health_label)}</span>
                  <span class="table-pill">${pct(row.cache_ratio)}</span>
                  <span class="table-pill">${number(row.tool_events)} tools</span>
                </div>
              </div>
            `).join("") || emptyState("No friction evidence is available.")}
          </div>
        `
      )}
    </div>
  `;
}

function renderAdoption(data) {
  cache.adoption = data;
  const rows = sortedAdoptionRows(data.leaderboard || []);
  const selected = ensureSelectedAdoption(rows);
  setChromePills({
    openExceptions: rows.filter((row) => row.health_label !== "Healthy" || Number(row.limit_hits || 0) > 0).length,
    sliceLabel: "coaching queue",
  });
  el("dashboard-page-adoption").innerHTML = `
    <div class="dashboard-page">
      <div class="section-head">
        ${sectionCopy("Adoption queue", pageScopeNote(data.filters, rows.length, "ranked subjects"))}
        <div class="section-actions">
          ${controlChip("Coachable first", state.adoptionSort === "coachable", 'data-adoption-sort="coachable"')}
          ${controlChip("Blocked first", state.adoptionSort === "blocked", 'data-adoption-sort="blocked"')}
          ${controlChip("Concentrated first", state.adoptionSort === "concentrated", 'data-adoption-sort="concentrated"')}
        </div>
      </div>
      <section class="workbench-grid">
        <div class="workbench-main">
          ${surface("Adoption intervention queue", "Ordered by intervention value.", adoptionTable(rows))}
          ${adoptionEvidence(data, rows, selected)}
        </div>
        <aside class="workbench-rail sticky">
          ${railModule("Selected entity", "Coaching path.", adoptionRail(data, selected))}
        </aside>
      </section>
    </div>
  `;

  document.querySelectorAll("[data-adoption-entity]").forEach((row) => {
    row.addEventListener("click", () => {
      state.selectedAdoption = row.dataset.adoptionEntity || "";
      renderAdoption(cache.adoption);
    });
  });
  document.querySelectorAll("[data-adoption-sort]").forEach((button) => {
    button.addEventListener("click", () => {
      state.adoptionSort = button.dataset.adoptionSort || "coachable";
      renderAdoption(cache.adoption);
    });
  });
}

function normalizeTraceEvents(events) {
  return events.map((event, index) => ({
    ...event,
    stamp: Number.isNaN(Date.parse(event.occurred_at)) ? index : Date.parse(event.occurred_at),
  }));
}

function sortTraceRequests(traces) {
  return [...traces].sort((left, right) => {
    if (state.traceView === "spend") {
      return Number(right.spend_usd || 0) - Number(left.spend_usd || 0)
        || Date.parse(right.latest_at || 0) - Date.parse(left.latest_at || 0);
    }
    if (state.traceView === "denials") {
      return Number(right.limit_hits || 0) - Number(left.limit_hits || 0)
        || Date.parse(right.latest_at || 0) - Date.parse(left.latest_at || 0);
    }
    return Date.parse(right.latest_at || 0) - Date.parse(left.latest_at || 0)
      || Number(right.spend_usd || 0) - Number(left.spend_usd || 0);
  });
}

function traceRequestList(traces) {
  const ranked = sortTraceRequests(traces);
  if (!ranked.length) return emptyState("No requests are available in the current slice.");
  return `
    <div class="trace-request-columns">
      <span></span>
      <span>Time</span>
      <span>Request</span>
      <span>Decisions</span>
      <span>Tools</span>
      <span>Tokens</span>
      <span>Cost</span>
    </div>
    <div class="trace-request-viewport" id="trace-request-viewport">
      <div class="trace-request-canvas" id="trace-request-canvas" style="height:${ranked.length * TRACE_REQUEST_ROW_HEIGHT}px">
        <div class="trace-request-window" id="trace-request-window"></div>
      </div>
    </div>
  `;
}

function traceRequestRow(trace) {
  return `
    <button class="trace-select trace-request-card ${trace.trace_id === state.filters.trace ? "active" : ""}" type="button" data-trace-id="${escapeHtml(trace.trace_id)}">
      <span class="trace-request-status"><span class="trace-status-dot ${Number(trace.limit_hits || 0) > 0 ? "bad" : "good"}"></span></span>
      <span class="trace-request-time">${escapeHtml(shortDate(trace.latest_at))}</span>
      <span class="trace-request-main">
        <strong>${escapeHtml(trace.trace_id)}</strong>
        ${(trace.badges || []).length ? `<small>${escapeHtml((trace.badges || []).slice(0, 2).join(" · "))}</small>` : ""}
      </span>
      <span class="trace-request-metric">${number(trace.decisions)}</span>
      <span class="trace-request-metric">${number(trace.tool_events)}</span>
      <span class="trace-request-metric">${number(trace.total_tokens)}</span>
      <span class="trace-request-metric">${money(trace.spend_usd)}</span>
    </button>
  `;
}

function hydrateTraceRequestList(traces) {
  const ranked = sortTraceRequests(traces);
  const viewport = el("trace-request-viewport");
  const windowEl = el("trace-request-window");
  const canvas = el("trace-request-canvas");
  if (!viewport || !windowEl || !canvas) return;
  canvas.style.height = `${ranked.length * TRACE_REQUEST_ROW_HEIGHT}px`;
  viewport.scrollTop = Math.min(state.traceRequestScrollTop, Math.max(canvas.offsetHeight - viewport.clientHeight, 0));

  const renderWindow = () => {
    state.traceRequestScrollTop = viewport.scrollTop;
    const visibleCount = Math.max(Math.ceil(viewport.clientHeight / TRACE_REQUEST_ROW_HEIGHT), 1);
    const start = Math.max(Math.floor(viewport.scrollTop / TRACE_REQUEST_ROW_HEIGHT) - TRACE_REQUEST_OVERSCAN, 0);
    const end = Math.min(start + visibleCount + TRACE_REQUEST_OVERSCAN * 2, ranked.length);
    const slice = ranked.slice(start, end);
    windowEl.style.transform = `translateY(${start * TRACE_REQUEST_ROW_HEIGHT}px)`;
    windowEl.innerHTML = slice.map(traceRequestRow).join("");
    windowEl.querySelectorAll("[data-trace-id]").forEach((button) => {
      button.addEventListener("click", () => {
        const next = button.dataset.traceId || "";
        if (state.filters.trace !== next) state.traceSpansScrollTop = 0;
        state.filters.trace = state.filters.trace === next ? "" : next;
        if (!state.filters.trace) state.traceSpansScrollTop = 0;
        state.selectedTraceSpan = "";
        state.traceLoading = Boolean(state.filters.trace);
        renderTraces(cache.traces);
        loadCurrentPage();
      });
    });
  };

  renderWindow();
  viewport.onscroll = () => renderWindow();
}

function traceContextTone(traceData) {
  if (traceData.denies > 0) return "bad";
  if (traceData.warns > 0) return "warn";
  if (traceData.events.length) return "good";
  return "accent";
}

function durationLabel(ms) {
  const numeric = Number(ms || 0);
  if (!Number.isFinite(numeric) || numeric <= 0) return "0ms";
  if (numeric < 1000) return `${Math.round(numeric)}ms`;
  if (numeric < 60000) return `${(numeric / 1000).toFixed(numeric < 10000 ? 2 : 1)}s`;
  const minutes = Math.floor(numeric / 60000);
  const seconds = ((numeric % 60000) / 1000).toFixed(0);
  return `${minutes}m ${seconds}s`;
}

function parseTraceNumber(value) {
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : 0;
}

function tracePairsFor(event) {
  return summaryPairs(event?.summary || "");
}

function traceModelLabel(value) {
  return String(value || "").replace("openai/", "").replace("anthropic/", "");
}

function tracePercentLabel(value) {
  const numeric = Number(value || 0);
  if (!Number.isFinite(numeric)) return "";
  return numeric <= 1 ? pct(numeric * 100, 0) : pct(numeric, 0);
}

function traceEventTitle(event) {
  switch (event.kind) {
    case "pi.agent_context": return "Agent context captured";
    case "pi.authorize": return "Authorization recorded";
    case "pi.authorize_error": return "Authorization error";
    case "pi.provider_call.started": return "Provider call started";
    case "pi.tool_call": return "Tool call started";
    case "tool.observed": return "Tool result observed";
    case "pi.message_end": return "Assistant message completed";
    case "usage.finalized": return "Usage finalized";
    case "pi.turn_end": return "Turn completed";
    case "pi.agent_end": return "Agent run completed";
    case "pi.stream_summary": return "Stream summary recorded";
    case "eval.score":
    case "eval.annotation": return "Evaluation recorded";
    default:
      if (event.kind.startsWith("decision.")) return `Policy ${event.kind.replace("decision.", "")}`;
      if (event.kind.startsWith("limit.report_only.")) return "Lifecycle limit observed";
      return titleCase(event.kind);
  }
}

function traceEventTone(event) {
  if (event.kind.includes(".deny") || event.kind.includes("authorize_error")) return "bad";
  if (event.kind.includes(".warn") || event.kind.startsWith("limit.report_only.")) return "warn";
  if (event.kind === "usage.finalized" || event.kind === "tool.observed" || event.kind === "pi.message_end") return "good";
  return "accent";
}

function traceEventFacts(event) {
  const pairs = event.pairs;
  const facts = [];
  if (event.kind.startsWith("decision.")) {
    if (pairs.selected_budget) facts.push(`budget ${pairs.selected_budget}`);
    if (pairs.model) facts.push(traceModelLabel(pairs.model));
    if (pairs.estimated_tokens) facts.push(`${number(pairs.estimated_tokens)} ctx tokens`);
    if (pairs.estimated_cost) facts.push(money(pairs.estimated_cost));
  } else if (event.kind === "pi.agent_context") {
    if (pairs.selected_tools_count) facts.push(`${number(pairs.selected_tools_count)} tools loaded`);
    if (pairs.skills_count) facts.push(`${number(pairs.skills_count)} skills`);
    if (pairs.context_files_count) facts.push(`${number(pairs.context_files_count)} context files`);
  } else if (event.kind === "pi.provider_call.started") {
    if (pairs.provider || pairs.model) facts.push(traceModelLabel(pairs.model || `${pairs.provider || ""}`));
    if (pairs.context_tokens) facts.push(`${number(pairs.context_tokens)} ctx tokens`);
    if (pairs.context_window) facts.push(`${number(pairs.context_window)} window`);
    if (pairs.context_usage_pct) facts.push(`${tracePercentLabel(pairs.context_usage_pct)} used`);
    if (pairs.shape) facts.push(pairs.shape.replaceAll(",", " · "));
  } else if (event.kind === "pi.tool_call") {
    if (pairs.tool_name) facts.push(pairs.tool_name);
    if (pairs.tool_call_id) facts.push(pairs.tool_call_id);
  } else if (event.kind === "tool.observed") {
    if (pairs.name) facts.push(pairs.name);
    if (pairs.duration_ms) facts.push(durationLabel(pairs.duration_ms));
    if (pairs.success) facts.push(pairs.success === "true" ? "ok" : "failed");
  } else if (event.kind === "pi.message_end" || event.kind === "usage.finalized") {
    if (pairs.model) facts.push(traceModelLabel(pairs.model));
    if (pairs.input_tokens) facts.push(`${number(pairs.input_tokens)} in`);
    if (pairs.output_tokens) facts.push(`${number(pairs.output_tokens)} out`);
    if (pairs.total_tokens || pairs.tokens) facts.push(`${number(pairs.total_tokens || pairs.tokens)} total`);
    if (pairs.cost) facts.push(money(pairs.cost));
    if (pairs.stop) facts.push(`stop ${pairs.stop}`);
  } else if (event.kind === "pi.turn_end") {
    if (pairs.turn) facts.push(`turn ${pairs.turn}`);
    if (pairs.usage_tokens) facts.push(`${number(pairs.usage_tokens)} tokens`);
    if (pairs.usage_cost) facts.push(money(pairs.usage_cost));
  } else if (event.kind === "pi.agent_end") {
    if (pairs.messages) facts.push(`${number(pairs.messages)} messages`);
    if (pairs.provider_calls) facts.push(`${number(pairs.provider_calls)} provider calls`);
    if (pairs.fallback_attribution && pairs.fallback_attribution !== "0") facts.push(`${pairs.fallback_attribution} fallback attribution`);
    if (pairs.unmatched_attribution && pairs.unmatched_attribution !== "0") facts.push(`${pairs.unmatched_attribution} unmatched`);
  } else if (event.kind.startsWith("limit.report_only.")) {
    if (pairs.tool_calls) facts.push(`${number(pairs.tool_calls)} tool calls`);
    if (pairs.agent_steps) facts.push(`${number(pairs.agent_steps)} steps`);
    if (pairs.retries) facts.push(`${number(pairs.retries)} retries`);
  } else if (event.kind.startsWith("eval.")) {
    if (pairs.label) facts.push(pairs.label);
    if (pairs.score) facts.push(`score ${pairs.score}`);
  }
  return facts.filter(Boolean);
}

function traceEventMetadata(event) {
  const pairs = event.pairs;
  return [
    pairs.provider_call ? `provider call ${pairs.provider_call}` : "",
    pairs.tool_call_id ? `tool call ${pairs.tool_call_id}` : "",
    pairs.matched_entity ? `matched ${pairs.matched_entity}` : "",
    pairs.selection_reason ? pairs.selection_reason.replaceAll("_", " ") : "",
    pairs.cwd ? pairs.cwd : "",
    pairs.tool_names ? `tools ${pairs.tool_names.replaceAll(",", ", ")}` : "",
    pairs.skill_names ? `skills ${pairs.skill_names.replaceAll(",", ", ")}` : "",
    pairs.attribution ? `attribution ${pairs.attribution}` : "",
  ].filter(Boolean);
}

function inspectTrace(events) {
  const normalized = normalizeTraceEvents(events).sort((left, right) => left.stamp - right.stamp);
  const first = normalized[0];
  const last = normalized[normalized.length - 1];
  const start = first ? first.stamp : 0;
  const enriched = normalized.map((event, index) => {
    const pairs = tracePairsFor(event);
    return {
      ...event,
      index: index + 1,
      offsetMs: Math.max(event.stamp - start, 0),
      pairs,
      title: traceEventTitle(event),
      tone: traceEventTone(event),
      facts: traceEventFacts({ ...event, pairs }),
      metadata: traceEventMetadata({ ...event, pairs }),
    };
  });

  const usageEvents = enriched.filter((event) => event.kind === "usage.finalized");
  const fallbackUsageEvents = enriched.filter((event) => event.kind === "pi.message_end");
  const costSource = usageEvents.length ? usageEvents : fallbackUsageEvents;
  const totalCost = sum(costSource.map((event) => parseTraceNumber(event.pairs.cost)));
  const totalTokens = sum(costSource.map((event) => parseTraceNumber(event.pairs.total_tokens || event.pairs.tokens)));
  const inputTokens = sum(costSource.map((event) => parseTraceNumber(event.pairs.input_tokens)));
  const outputTokens = sum(costSource.map((event) => parseTraceNumber(event.pairs.output_tokens)));
  const providerCalls = enriched.filter((event) => event.kind === "pi.provider_call.started");
  const turns = enriched.filter((event) => event.kind === "pi.turn_end");
  const toolCalls = enriched.filter((event) => event.kind === "pi.tool_call");
  const toolObserved = enriched.filter((event) => event.kind === "tool.observed");
  const uniqueTools = [...new Set(toolObserved.concat(toolCalls).map((event) => event.pairs.name || event.pairs.tool_name).filter(Boolean))];
  const decisions = enriched.filter((event) => event.kind.startsWith("decision."));
  const denies = decisions.filter((event) => event.kind.endsWith(".deny")).length;
  const warns = decisions.filter((event) => event.kind.endsWith(".warn")).length;
  const peakContextTokens = maxValue(enriched.map((event) => parseTraceNumber(event.pairs.context_tokens || event.pairs.estimated_tokens)), 0);
  const peakContextWindow = maxValue(enriched.map((event) => parseTraceNumber(event.pairs.context_window)), 0);
  const peakContextPct = maxValue(enriched.map((event) => {
    const numeric = parseTraceNumber(event.pairs.context_usage_pct);
    return numeric <= 1 ? numeric * 100 : numeric;
  }), 0);
  const categoryCounts = new Map();
  for (const event of enriched) {
    categoryCounts.set(event.category, (categoryCounts.get(event.category) || 0) + 1);
  }
  return {
    first,
    last,
    durationMs: first && last ? Math.max(last.stamp - first.stamp, 0) : 0,
    events: enriched,
    totalCost,
    totalTokens,
    inputTokens,
    outputTokens,
    providerCalls,
    turns,
    toolCalls,
    toolObserved,
    uniqueTools,
    decisions,
    denies,
    warns,
    peakContextTokens,
    peakContextWindow,
    peakContextPct,
    categoryCounts,
  };
}

function traceSnapshot(data, traceData) {
  const selected = (data.traces || []).find((trace) => trace.trace_id === state.filters.trace) || {};
  const decisionLabel = traceData.denies > 0
    ? `${traceData.denies} deny`
    : traceData.warns > 0
      ? `${traceData.warns} warn`
      : `${number(traceData.decisions.length)} allow`;
  const models = [...new Set(traceData.providerCalls.map((event) => traceModelLabel(event.pairs.model || event.pairs.provider)).filter(Boolean))];
  const budgets = [...new Set(traceData.decisions.map((event) => event.pairs.selected_budget).filter(Boolean))];
  return `
    <div class="trace-detail-header">
      <div class="trace-detail-topline">
        <div class="trace-detail-title">
          <strong>${escapeHtml(state.filters.trace || "No trace selected")}</strong>
          <span class="trace-detail-inline">${escapeHtml([
            traceData.first?.occurred_at ? shortDate(traceData.first.occurred_at) : "",
            durationLabel(traceData.durationMs),
            `${number(traceData.events.length)} spans`,
            `${number(traceData.uniqueTools.length)} tools`,
            `${money(traceData.totalCost || selected.spend_usd || 0)}`,
            `${number(traceData.totalTokens || selected.total_tokens || 0)} tokens`,
          ].filter(Boolean).join(" · "))}</span>
        </div>
        <div class="trace-detail-status ${traceData.denies > 0 ? "bad" : traceData.warns > 0 ? "warn" : "good"}">${escapeHtml(decisionLabel)}</div>
      </div>
    </div>
  `;
}

function activeTraceSpan(traceData) {
  if (!traceData.events.length) {
    state.selectedTraceSpan = "";
    return null;
  }
  return traceData.events.find((event) => String(event.index) === String(state.selectedTraceSpan)) || null;
}

function traceSpanStatus(event) {
  const pairs = event.pairs;
  if (event.kind.startsWith("decision.")) return event.kind.replace("decision.", "");
  if (event.kind === "tool.observed") return pairs.success === "false" ? "failed" : "ok";
  if (event.kind === "usage.finalized") return "finalized";
  if (event.kind === "pi.message_end") return "message";
  if (event.kind === "pi.provider_call.started") return "started";
  if (event.kind === "pi.tool_call") return "called";
  if (event.kind === "pi.turn_end") return "turn";
  if (event.kind === "pi.agent_end") return "done";
  if (event.kind.startsWith("limit.report_only.")) return "report";
  if (event.kind.startsWith("eval.")) return "eval";
  return event.category || "event";
}

function traceSpanDuration(event) {
  return event.pairs.duration_ms ? durationLabel(event.pairs.duration_ms) : "";
}

function traceSpanTokens(event) {
  if (event.pairs.total_tokens || event.pairs.tokens) return number(event.pairs.total_tokens || event.pairs.tokens);
  if (event.pairs.usage_tokens) return number(event.pairs.usage_tokens);
  if (event.pairs.context_tokens || event.pairs.estimated_tokens) return `ctx ${number(event.pairs.context_tokens || event.pairs.estimated_tokens)}`;
  return "";
}

function traceSpanCost(event) {
  if (event.pairs.cost) return money(event.pairs.cost);
  if (event.pairs.usage_cost) return money(event.pairs.usage_cost);
  if (event.pairs.estimated_cost) return money(event.pairs.estimated_cost);
  return "";
}

function traceSpanUsage(event) {
  const cost = traceSpanCost(event);
  if (cost) return cost;
  const tokens = traceSpanTokens(event);
  if (tokens) return tokens;
  return "";
}

function traceSpanSubline(event) {
  const pairs = event.pairs;
  const parts = [
    event.kind,
    pairs.name || pairs.tool_name || "",
    traceModelLabel(pairs.model || pairs.provider || ""),
    pairs.selected_budget || "",
    pairs.context_usage_pct ? `ctx ${tracePercentLabel(pairs.context_usage_pct)}` : "",
  ].filter(Boolean);
  return parts.slice(0, 4).join(" · ");
}

function traceSpanKind(event) {
  const pairs = event.pairs;
  return [
    event.kind,
    pairs.name || pairs.tool_name || traceModelLabel(pairs.model || pairs.provider || ""),
  ].filter(Boolean).slice(0, 2).join(" · ");
}

function traceSpanGroupLabel(event) {
  return titleCase(event.category || event.kind || "event");
}

function traceSpanWaterfall(traceData, event) {
  const total = Math.max(traceData.durationMs, 1);
  const startPct = clamp((event.offsetMs / total) * 100, 0, 100);
  const rawDuration = parseTraceNumber(event.pairs.duration_ms);
  const widthPct = rawDuration > 0 ? Math.max((rawDuration / total) * 100, 0.8) : 0;
  return `
    <span class="trace-waterfall-track">
      ${widthPct > 0
        ? `<span class="trace-waterfall-bar ${event.tone}" style="left:${startPct}%;width:${Math.min(widthPct, 100 - startPct)}%"></span>`
        : `<span class="trace-waterfall-dot ${event.tone}" style="left:${startPct}%"></span>`}
    </span>
  `;
}

function traceSpanTimeline(traceData) {
  if (!traceData.events.length) return emptyState("No correlated trace events are available.");
  const selectedSpan = activeTraceSpan(traceData);
  const rows = [];
  let previousCategory = "";
  for (const event of traceData.events) {
    if (event.category !== previousCategory) {
      rows.push(`
        <div class="trace-span-group">
          <span>${escapeHtml(traceSpanGroupLabel(event))}</span>
        </div>
      `);
      previousCategory = event.category;
    }
    rows.push(`
      <button class="trace-span-row ${event.tone} ${selectedSpan?.index === event.index ? "active" : ""}" type="button" data-trace-span="${escapeHtml(String(event.index))}">
        <span class="trace-span-time">
          <strong>${escapeHtml(`+${durationLabel(event.offsetMs)}`)}</strong>
          <small>${escapeHtml(shortDate(event.occurred_at))}</small>
        </span>
        <span class="trace-span-main">
          <span class="trace-span-order">${number(event.index)}</span>
          <strong>${escapeHtml(event.title)}</strong>
        </span>
        <span class="trace-span-kind">${escapeHtml(traceSpanKind(event) || traceSpanSubline(event) || "—")}</span>
        <span class="trace-span-waterfall">${traceSpanWaterfall(traceData, event)}</span>
        <span class="trace-span-metric">${escapeHtml(traceSpanDuration(event) || "—")}</span>
        <span class="trace-span-metric">${escapeHtml(traceSpanUsage(event) || "")}</span>
        <span class="trace-span-metric trace-span-status ${event.tone}"><span class="trace-status-dot ${event.tone}"></span>${escapeHtml(traceSpanStatus(event))}</span>
      </button>
    `);
  }
  return `
    <div class="trace-span-columns">
      <span>Time</span>
      <span>Span</span>
      <span>Kind</span>
      <span>Waterfall</span>
      <span>Duration</span>
      <span>Usage</span>
      <span>Status</span>
    </div>
    <div class="trace-span-list">
      ${rows.join("")}
    </div>
  `;
}

function traceSpanCardTimeline(traceData) {
  if (!traceData.events.length) return emptyState("No correlated trace events are available.");
  const selectedSpan = activeTraceSpan(traceData);
  const rows = [];
  let previousCategory = "";
  for (const event of traceData.events) {
    if (event.category !== previousCategory) {
      rows.push(`
        <div class="trace-span-group">
          <span>${escapeHtml(traceSpanGroupLabel(event))}</span>
        </div>
      `);
      previousCategory = event.category;
    }
    const meta = [
      traceSpanKind(event) || traceSpanSubline(event),
      traceSpanDuration(event),
      traceSpanUsage(event),
    ].filter(Boolean).join(" · ");
    rows.push(`
      <button class="trace-span-card ${event.tone} ${selectedSpan?.index === event.index ? "active" : ""}" type="button" data-trace-span="${escapeHtml(String(event.index))}">
        <span class="trace-span-card-time">
          <strong>${escapeHtml(`+${durationLabel(event.offsetMs)}`)}</strong>
          <small>${escapeHtml(shortDate(event.occurred_at))}</small>
        </span>
        <span class="trace-span-card-main">
          <span class="trace-span-order">${number(event.index)}</span>
          <strong>${escapeHtml(event.title)}</strong>
        </span>
        <span class="trace-span-card-status ${event.tone}"><span class="trace-status-dot ${event.tone}"></span>${escapeHtml(traceSpanStatus(event))}</span>
        <span class="trace-span-card-meta">${escapeHtml(meta || "No metadata")}</span>
        <span class="trace-span-card-waterfall">${traceSpanWaterfall(traceData, event)}</span>
      </button>
    `);
  }
  return `
    <div class="trace-span-card-list">
      ${rows.join("")}
    </div>
  `;
}

function traceSpanInspector(data, traceData) {
  const span = activeTraceSpan(traceData);
  if (!span) return "";
  return `
    <div class="trace-inspector-card ${span.tone}">
      <div class="trace-inspector-head">
        <div>
          <div class="page-overline">Selected span</div>
          <h3 class="rail-title">${escapeHtml(span.title)}</h3>
          <div class="trace-detail-subline">${escapeHtml(shortDate(span.occurred_at))} · ${escapeHtml(span.kind)}</div>
        </div>
        <div class="trace-inspector-actions">
          <div class="trace-detail-status ${span.tone}">${escapeHtml(traceSpanStatus(span))}</div>
          <button class="trace-close-button" type="button" data-trace-span-close aria-label="Close span inspector">×</button>
        </div>
      </div>
      <dl class="trace-inspector-grid">
        <div><dt>Order</dt><dd>${number(span.index)}</dd></div>
        <div><dt>Offset</dt><dd>${durationLabel(span.offsetMs)}</dd></div>
        <div><dt>Duration</dt><dd>${traceSpanDuration(span) || "—"}</dd></div>
        <div><dt>Tokens</dt><dd>${traceSpanTokens(span) || "—"}</dd></div>
        <div><dt>Cost</dt><dd>${traceSpanCost(span) || "—"}</dd></div>
        <div><dt>Status</dt><dd>${escapeHtml(traceSpanStatus(span))}</dd></div>
      </dl>
      ${span.facts.length ? `<div class="trace-inspector-section"><strong>Facts</strong><div class="trace-inspector-lines">${span.facts.map((fact) => `<span>${escapeHtml(fact)}</span>`).join("")}</div></div>` : ""}
      ${span.metadata.length ? `<div class="trace-inspector-section"><strong>Metadata</strong><div class="trace-inspector-lines">${span.metadata.map((item) => `<span>${escapeHtml(item)}</span>`).join("")}</div></div>` : ""}
      <details class="trace-support-details">
        <summary>Raw summary</summary>
        <code class="trace-span-raw">${escapeHtml(span.summary || "")}</code>
      </details>
      <details class="trace-support-details">
        <summary>Trace aggregates</summary>
        <div class="trace-support-grid">
          <div class="trace-support-card">
            <h3>Tools</h3>
            ${traceToolBreakdown(traceData)}
          </div>
          <div class="trace-support-card">
            <h3>Context + policy</h3>
            ${traceContextSummary(data, traceData)}
          </div>
          <div class="trace-support-card">
            <h3>Span mix</h3>
            ${traceCategoryBreakdown(traceData)}
          </div>
        </div>
      </details>
    </div>
  `;
}

function traceDrawer(data, traceData) {
  const span = activeTraceSpan(traceData);
  if (!span) return "";
  return `
    <div class="trace-drawer-shell" data-trace-drawer>
      <button class="trace-drawer-backdrop" type="button" data-trace-span-close aria-label="Close span drawer"></button>
      <aside class="trace-drawer" tabindex="-1" aria-label="Trace span inspector">
        ${traceSpanInspector(data, traceData)}
      </aside>
    </div>
  `;
}

function traceSpansDrawer(data, traceData, contextTone, hasSelectedTrace) {
  if (!hasSelectedTrace) return "";
  const selected = (data.traces || []).find((trace) => trace.trace_id === state.filters.trace) || {};
  return `
    <div class="trace-spans-drawer-shell" data-trace-spans-drawer>
      <button class="trace-spans-drawer-backdrop" type="button" data-trace-detail-close aria-label="Close trace spans"></button>
      <aside class="trace-spans-drawer trace-detail-pane ${contextTone}" tabindex="-1" aria-label="Trace spans">
        <div class="trace-spans-drawer-head">
          <div>
            <div class="page-overline">Selected request</div>
            <h3 class="rail-title" title="${escapeHtml(state.filters.trace)}">${escapeHtml(state.filters.trace)}</h3>
            <div class="trace-detail-subline">${number(selected.decisions || 0)} decisions · ${number(selected.tool_events || 0)} tools · ${money(selected.spend_usd || 0)}</div>
          </div>
          <button class="trace-close-button" type="button" data-trace-detail-close aria-label="Close trace spans">×</button>
        </div>
        <div class="trace-spans-drawer-body" data-trace-spans-body>
          ${state.traceLoading ? `
            <div class="trace-loading-state">Loading ${escapeHtml(state.filters.trace)}…</div>
          ` : `
            <div class="trace-spans-summary">${traceSnapshot(data, traceData)}</div>
            <div class="trace-spans-intro">
              <span>${number(traceData.events.length)} spans</span>
              <p>Ordered events inside the selected request. Click any span to open its inspector.</p>
            </div>
            <div class="trace-spans-table">${traceSpanCardTimeline(traceData)}</div>
          `}
        </div>
      </aside>
    </div>
  `;
}

function traceToolBreakdown(traceData) {
  const rows = new Map();
  for (const event of traceData.toolObserved.concat(traceData.toolCalls)) {
    const name = event.pairs.name || event.pairs.tool_name || "unknown";
    if (!rows.has(name)) rows.set(name, { name, calls: 0, observed: 0, durationMs: 0 });
    const row = rows.get(name);
    if (event.kind === "pi.tool_call") row.calls += 1;
    if (event.kind === "tool.observed") {
      row.observed += 1;
      row.durationMs += parseTraceNumber(event.pairs.duration_ms);
    }
  }
  const values = [...rows.values()].sort((left, right) => right.observed - left.observed || right.calls - left.calls || right.durationMs - left.durationMs);
  if (!values.length) return emptyState("No tool activity was recorded for this trace.");
  return `
    <div class="trace-mini-table">
      ${values.map((row) => `
        <div class="trace-mini-row">
          <strong>${escapeHtml(row.name)}</strong>
          <span>${number(row.calls || row.observed)} spans</span>
          <span>${row.durationMs ? durationLabel(row.durationMs) : "no duration"}</span>
        </div>
      `).join("")}
    </div>
  `;
}

function traceContextSummary(data, traceData) {
  const models = [...new Set(traceData.providerCalls.map((event) => traceModelLabel(event.pairs.model || event.pairs.provider)).filter(Boolean))];
  const budgets = [...new Set(traceData.decisions.map((event) => event.pairs.selected_budget).filter(Boolean))];
  return `
    <div class="rail-list">
      <div class="rail-item">
        <strong>Context peak</strong>
        <div class="rail-note">${traceData.peakContextTokens ? `${number(traceData.peakContextTokens)} tokens` : "No context token metric"}</div>
        <div class="rail-note">${traceData.peakContextWindow ? `${number(traceData.peakContextWindow)} window · ${tracePercentLabel(traceData.peakContextPct)} used` : "No context window metric"}</div>
      </div>
      <div class="rail-item">
        <strong>Models</strong>
        <div class="rail-note">${models.length ? escapeHtml(models.join(" · ")) : "No provider call model recorded"}</div>
      </div>
      <div class="rail-item">
        <strong>Budgets touched</strong>
        <div class="rail-note">${budgets.length ? escapeHtml(budgets.join(" · ")) : "No budget decision recorded"}</div>
      </div>
      <div class="rail-item">
        <strong>Policy outcomes</strong>
        <div class="rail-note">${number(data.policy?.allow || 0)} allow · ${number(data.policy?.warn || 0)} warn · ${number(data.policy?.deny || 0)} deny</div>
        <div class="rail-note">${number(data.policy?.limit_hits || 0)} total limit hits · ${number(data.policy?.lifecycle_limits || 0)} lifecycle checks</div>
      </div>
    </div>
  `;
}

function traceCategoryBreakdown(traceData) {
  const rows = [...traceData.categoryCounts.entries()]
    .map(([category, count]) => ({ category, count }))
    .sort((left, right) => right.count - left.count);
  if (!rows.length) return emptyState("No trace categories were recorded.");
  const max = maxValue(rows.map((row) => row.count), 1);
  return `
    <div class="rail-list">
      ${rows.map((row) => `
        <div class="rail-item">
          <strong>${escapeHtml(row.category.replaceAll("_", " "))}</strong>
          <div style="margin-top:8px">${inlineBar(row.count, max, number, toneClass(row.category))}</div>
        </div>
      `).join("")}
    </div>
  `;
}

function rememberTraceSpansScroll() {
  const body = document.querySelector("[data-trace-spans-body]");
  if (body) state.traceSpansScrollTop = body.scrollTop;
}

function renderTraces(data) {
  rememberTraceSpansScroll();
  cache.traces = data;
  const hasSelectedTrace = Boolean(state.filters.trace);
  const hasLoadedSelectedTrace = hasSelectedTrace && data.selected_trace_id === state.filters.trace;
  state.traceLoading = hasSelectedTrace && !hasLoadedSelectedTrace;
  const traceData = hasLoadedSelectedTrace ? inspectTrace(data.timeline || []) : inspectTrace([]);
  const selectedSpan = activeTraceSpan(traceData);
  const contextTone = traceContextTone(traceData);
  setChromePills({
    openExceptions: (data.traces || []).filter((trace) => Number(trace.limit_hits || 0) > 0).length,
    sliceLabel: state.filters.trace ? "selected trace" : "all requests",
  });
  el("dashboard-page-traces").innerHTML = `
    <div class="dashboard-page">
      <div class="section-head">
        ${sectionCopy("Trace explorer", pageScopeNote(data.filters, (data.traces || []).length, "requests"))}
        <div class="section-actions">
          ${controlChip("Newest requests", state.traceView === "recent", 'data-trace-view="recent"')}
          ${controlChip("High spend", state.traceView === "spend", 'data-trace-view="spend"')}
          ${controlChip("Most denials", state.traceView === "denials", 'data-trace-view="denials"')}
        </div>
      </div>
      <section class="trace-explorer-shell">
        <section class="trace-request-main-pane">
          <div class="trace-pane-head">
            <h3 class="rail-title">Requests</h3>
            <div class="panel-note">Each row is one trace root. Click a request to open its spans.</div>
          </div>
          ${traceRequestList(data.traces || [])}
        </section>
      </section>
      ${traceSpansDrawer(data, traceData, contextTone, hasSelectedTrace)}
      ${traceDrawer(data, traceData)}
    </div>
  `;
  hydrateTraceRequestList(data.traces || []);
  const spansBody = document.querySelector("[data-trace-spans-body]");
  if (spansBody) {
    spansBody.scrollTop = Math.min(state.traceSpansScrollTop, Math.max(spansBody.scrollHeight - spansBody.clientHeight, 0));
    spansBody.addEventListener("scroll", () => {
      state.traceSpansScrollTop = spansBody.scrollTop;
    }, { passive: true });
  }
  document.querySelectorAll("[data-trace-span]").forEach((button) => {
    button.addEventListener("click", () => {
      const next = button.dataset.traceSpan || "";
      state.selectedTraceSpan = state.selectedTraceSpan === next ? "" : next;
      renderTraces(cache.traces);
    });
  });
  document.querySelectorAll("[data-trace-view]").forEach((button) => {
    button.addEventListener("click", () => {
      state.traceView = button.dataset.traceView || "recent";
      renderTraces(cache.traces);
    });
  });
  document.querySelectorAll("[data-trace-detail-close]").forEach((button) => {
    button.addEventListener("click", () => {
      state.filters.trace = "";
      state.selectedTraceSpan = "";
      state.traceSpansScrollTop = 0;
      renderTraces(cache.traces);
    });
  });
  document.querySelectorAll("[data-trace-span-close]").forEach((button) => {
    button.addEventListener("click", () => {
      state.selectedTraceSpan = "";
      renderTraces(cache.traces);
    });
  });
  const drawer = document.querySelector("[data-trace-drawer] .trace-drawer");
  if (drawer) drawer.focus({ preventScroll: true });
  const spansDrawer = document.querySelector("[data-trace-spans-drawer] .trace-spans-drawer");
  if (spansDrawer) spansDrawer.focus({ preventScroll: true });
}

function traceDrawerContainsTarget(target) {
  if (!target || typeof target.closest !== "function") return false;
  return Boolean(target.closest("[data-trace-drawer]") || target.closest("[data-trace-span]"));
}

function closeTraceDrawerIfUnfocused(target) {
  if (state.page !== "traces" || !state.selectedTraceSpan) return;
  if (traceDrawerContainsTarget(target)) return;
  state.selectedTraceSpan = "";
  if (cache.traces) renderTraces(cache.traces);
}

function traceSpansDrawerContainsTarget(target) {
  if (!target || typeof target.closest !== "function") return false;
  return Boolean(
    target.closest("[data-trace-spans-drawer]")
      || target.closest("[data-trace-drawer]")
      || target.closest("[data-trace-id]")
  );
}

function closeTraceSpansDrawerIfUnfocused(target) {
  if (state.page !== "traces" || !state.filters.trace) return;
  if (traceSpansDrawerContainsTarget(target)) return;
  state.filters.trace = "";
  state.selectedTraceSpan = "";
  state.traceSpansScrollTop = 0;
  if (cache.traces) renderTraces(cache.traces);
}

function normalize(value, values) {
  const max = maxValue(values, 1);
  return Number(value || 0) / max;
}

function rankStrategies(cards, objective) {
  const costs = cards.map((card) => Number(card.total_cost_usd || 0));
  const denied = cards.map((card) => Number(card.denied_requests || 0));
  const runaway = cards.map((card) => Number(card.runaway_spend_prevented_usd || 0));
  return [...cards]
    .map((card) => {
      const costNorm = normalize(card.total_cost_usd, costs);
      const denyNorm = normalize(card.denied_requests, denied);
      const runawayNorm = normalize(card.runaway_spend_prevented_usd, runaway);
      let score = 0;
      if (objective === "cost") {
        score = (1 - costNorm) * 0.5 + (1 - denyNorm) * 0.15 + runawayNorm * 0.15 + Number(card.fairness_score || 0) * 0.1 + Number(card.adoption_coverage || 0) * 0.1;
      } else if (objective === "adoption") {
        score = Number(card.adoption_coverage || 0) * 0.45 + Number(card.fairness_score || 0) * 0.2 + runawayNorm * 0.2 + (1 - costNorm) * 0.1 + (1 - denyNorm) * 0.05;
      } else {
        score = Number(card.adoption_coverage || 0) * 0.3 + Number(card.fairness_score || 0) * 0.25 + runawayNorm * 0.2 + (1 - costNorm) * 0.15 + (1 - denyNorm) * 0.1;
      }
      return { card, score };
    })
    .sort((left, right) => right.score - left.score || Number(left.card.total_cost_usd || 0) - Number(right.card.total_cost_usd || 0))
    .map((entry) => entry.card);
}

function ensureSelectedStrategy(cards) {
  if (!cards.some((card) => card.id === state.selectedStrategy)) {
    state.selectedStrategy = cards[0]?.id || "";
  }
  return cards.find((card) => card.id === state.selectedStrategy) || null;
}

function objectiveLabel() {
  if (state.strategyObjective === "cost") return "Cost-first";
  if (state.strategyObjective === "adoption") return "Adoption-first";
  return "Balanced";
}

function objectiveReason(card) {
  if (!card) return "";
  if (state.strategyObjective === "cost") {
    return `${card.id} minimizes total spend at ${money(card.total_cost_usd)} while preserving as much fairness and runaway spend prevented as possible.`;
  }
  if (state.strategyObjective === "adoption") {
    return `${card.id} keeps adoption at ${pct((card.adoption_coverage || 0) * 100)} while preserving more fairness and runaway spend prevented than the looser option.`;
  }
  return `${card.id} best balances adoption, fairness, runaway spend prevented, and total cost for this simulation.`;
}

function strategyScopeNote(data, cards) {
  const simulation = (data.simulations || []).find((item) => item.id === data.selected_simulation_id);
  return `${simulation?.name || simulation?.id || "No simulation"} · ${number(cards.length)} strategies · ${objectiveLabel()} objective`;
}

function strategyMetricSummary(card) {
  return [
    { label: "Cost", value: money(card.total_cost_usd) },
    { label: "Adoption", value: pct((card.adoption_coverage || 0) * 100) },
    { label: "Fairness", value: Number(card.fairness_score || 0).toFixed(2) },
    { label: "Runaway spend prevented ($)", value: money(card.runaway_spend_prevented_usd || 0) },
  ];
}

function objectiveExplanation() {
  if (state.strategyObjective === "cost") {
    return "Cost-first weights spend 45%, fairness 20%, runaway spend prevented 15%, adoption 10%, and lower denials 10%.";
  }
  if (state.strategyObjective === "adoption") {
    return "Adoption-first weights adoption 45%, fairness 20%, runaway spend prevented 20%, lower spend 10%, and lower denials 5%.";
  }
  return "Balanced weights adoption 30%, fairness 25%, runaway spend prevented 20%, lower spend 15%, and lower denials 10%.";
}

function strategyDeltaChips(winner, runnerUp) {
  if (!winner || !runnerUp) return "";
  const fairnessDelta = Number(winner.fairness_score || 0) - Number(runnerUp.fairness_score || 0);
  const costDelta = Number(runnerUp.total_cost_usd || 0) - Number(winner.total_cost_usd || 0);
  const runawayDelta = Number(winner.runaway_spend_prevented_usd || 0) - Number(runnerUp.runaway_spend_prevented_usd || 0);
  const denyDelta = Number(winner.denied_requests || 0) - Number(runnerUp.denied_requests || 0);
  return `
    <div class="delta-strip">
      <span class="delta-chip">${escapeHtml(winner.id)} vs ${escapeHtml(runnerUp.id)}</span>
      <span class="delta-chip">${costDelta >= 0 ? `${money(costDelta)} cheaper` : `${money(Math.abs(costDelta))} more expensive`}</span>
      <span class="delta-chip">${fairnessDelta >= 0 ? "+" : ""}${fairnessDelta.toFixed(2)} fairness</span>
      <span class="delta-chip">${runawayDelta >= 0 ? "+" : "-"}${money(Math.abs(runawayDelta))} runaway spend prevented</span>
      <span class="delta-chip">${denyDelta >= 0 ? "+" : ""}${number(denyDelta)} denied</span>
    </div>
  `;
}

function strategyDecisionCard(card, label, peer, recommended = false) {
  const comparisonCopy = recommended || !peer
    ? objectiveReason(card)
    : `${card.id} remains the looser alternative: ${money(card.total_cost_usd)} total cost, fairness ${Number(card.fairness_score || 0).toFixed(2)}, and ${money(card.runaway_spend_prevented_usd || 0)} runaway spend prevented against ${peer.id}.`;
  return `
    <article class="decision-card${recommended ? " is-recommended" : ""}">
      <div class="decision-card-head">
        <div>
          <div class="section-kicker">${escapeHtml(label)}</div>
          <div class="matrix-banner-title">${escapeHtml(card.id)}</div>
          <div class="panel-note">${escapeHtml(card.description || "")}</div>
        </div>
        <div class="table-chip-row">
          ${(card.badges || []).slice(0, 2).map(railChip).join("")}
          ${recommended ? statusChip(objectiveLabel()) : ""}
        </div>
      </div>
      <div class="decision-kpis">
        ${strategyMetricSummary(card).map((metric) => `
          <div class="decision-kpi">
            <div class="metric-label">${escapeHtml(metric.label)}</div>
            <div class="decision-kpi-value">${escapeHtml(metric.value)}</div>
          </div>
        `).join("")}
      </div>
      <div class="decision-card-copy">${escapeHtml(comparisonCopy)}</div>
      <div class="table-chip-row" style="margin-top:10px">
        <span class="table-pill">${number(card.denied_requests || 0)} denied</span>
        <span class="table-pill">${number(card.fallback_count || 0)} fallbacks</span>
        ${peer ? `<span class="table-pill">${escapeHtml(peer.id)}</span>` : ""}
      </div>
    </article>
  `;
}

function strategyHeadToHead(cards) {
  if (!cards.length) return emptyState("No strategies are available.");
  const winner = cards[0];
  const runnerUp = cards[1];
  return `
    ${strategyDeltaChips(winner, runnerUp)}
    <div class="decision-grid">
      ${strategyDecisionCard(winner, "Recommended", runnerUp, true)}
      ${runnerUp ? strategyDecisionCard(runnerUp, "Alternative", winner, false) : ""}
    </div>
  `;
}

function strategyDifferenceSummary(cards) {
  if (cards.length < 2) return emptyState("At least two strategies are needed for a comparison.");
  const winner = cards[0];
  const runnerUp = cards[1];
  const tradeoffs = [
    {
      label: "Cheaper",
      tone: "accent",
      side: Number(winner.total_cost_usd || 0) <= Number(runnerUp.total_cost_usd || 0) ? winner.id : runnerUp.id,
      value: money(Math.abs(Number(runnerUp.total_cost_usd || 0) - Number(winner.total_cost_usd || 0))),
      note: `${Number(winner.total_cost_usd || 0) <= Number(runnerUp.total_cost_usd || 0) ? winner.id : runnerUp.id} costs less in this simulation.`,
    },
    {
      label: "Fairer",
      tone: "cyan",
      side: Number(winner.fairness_score || 0) >= Number(runnerUp.fairness_score || 0) ? winner.id : runnerUp.id,
      value: `${Math.abs(Number(winner.fairness_score || 0) - Number(runnerUp.fairness_score || 0)).toFixed(2)}`,
      note: `${Number(winner.fairness_score || 0) >= Number(runnerUp.fairness_score || 0) ? winner.id : runnerUp.id} preserves a fairer outcome mix.`,
    },
    {
      label: "More protective",
      tone: "warn",
      side: Number(winner.runaway_spend_prevented_usd || 0) >= Number(runnerUp.runaway_spend_prevented_usd || 0) ? winner.id : runnerUp.id,
      value: money(Math.abs(Number(winner.runaway_spend_prevented_usd || 0) - Number(runnerUp.runaway_spend_prevented_usd || 0))),
      note: `${Number(winner.runaway_spend_prevented_usd || 0) >= Number(runnerUp.runaway_spend_prevented_usd || 0) ? winner.id : runnerUp.id} prevents more runaway spend.`,
    },
    {
      label: "Stricter",
      tone: "bad",
      side: Number(winner.denied_requests || 0) >= Number(runnerUp.denied_requests || 0) ? winner.id : runnerUp.id,
      value: `${number(Math.abs(Number(winner.denied_requests || 0) - Number(runnerUp.denied_requests || 0)))} requests`,
      note: `${Number(winner.denied_requests || 0) >= Number(runnerUp.denied_requests || 0) ? winner.id : runnerUp.id} denies more requests to get that protection.`,
    },
  ];
  return `
    <div class="tradeoff-grid">
      ${tradeoffs.map((item) => `
        <div class="tradeoff-card ${escapeHtml(item.tone)}">
          <div class="metric-label">${escapeHtml(item.label)}</div>
          <div class="tradeoff-side">${escapeHtml(item.side)}</div>
          <div class="tradeoff-value">${escapeHtml(item.value)}</div>
          <div class="tradeoff-note">${escapeHtml(item.note)}</div>
        </div>
      `).join("")}
    </div>
  `;
}

function strategyMatrix(cards) {
  if (cards.length <= 2) {
    return `
      <div class="strategy-score-list">
        ${cards.map((card) => `
          <button class="strategy-score-row ${card.id === state.selectedStrategy ? "active" : ""}" type="button" data-strategy-id="${escapeHtml(card.id)}">
            <div class="strategy-score-main">
              <strong>${escapeHtml(card.id)}</strong>
              <div class="table-sub">${escapeHtml(card.description || "")}</div>
            </div>
            <div class="strategy-score-metrics">
              <span><strong>${money(card.total_cost_usd)}</strong><em>cost</em></span>
              <span><strong>${Number(card.fairness_score || 0).toFixed(2)}</strong><em>fairness</em></span>
              <span><strong>${money(card.runaway_spend_prevented_usd)}</strong><em>runaway spend prevented ($)</em></span>
              <span><strong>${number(card.denied_requests)}</strong><em>denied</em></span>
            </div>
          </button>
        `).join("")}
      </div>
    `;
  }
  const maxCost = maxValue(cards.map((card) => card.total_cost_usd), 1);
  const maxRunaway = maxValue(cards.map((card) => card.runaway_spend_prevented_usd), 1);
  return `
    <div class="table-shell">
      <div class="table-scroller">
        <table class="dense-table">
          <thead>
            <tr>
              <th>Strategy</th>
              <th>Cost</th>
              <th>Adoption</th>
              <th>Fairness</th>
              <th>Runaway spend prevented ($)</th>
              <th>Denied</th>
              <th>Fallbacks</th>
            </tr>
          </thead>
          <tbody>
            ${cards.map((card) => `
              <tr class="clickable ${card.id === state.selectedStrategy ? "is-selected" : ""}" data-strategy-id="${escapeHtml(card.id)}">
                <td><div class="entity-stack"><strong>${escapeHtml(card.id)}</strong><div class="table-sub">${escapeHtml(card.description || "")}</div></div></td>
                <td>${inlineBar(card.total_cost_usd, maxCost, money, "accent")}</td>
                <td>${pct((card.adoption_coverage || 0) * 100)}</td>
                <td>${Number(card.fairness_score || 0).toFixed(2)}</td>
                <td>${inlineBar(card.runaway_spend_prevented_usd, maxRunaway, money, "warn")}</td>
                <td>${number(card.denied_requests)}</td>
                <td>${number(card.fallback_count)}</td>
              </tr>
            `).join("")}
          </tbody>
        </table>
      </div>
    </div>
  `;
}

function strategyRail(selected, recommendations) {
  if (!selected) return emptyState("No strategy selected.");
  return `
    <div class="strategy-rail-summary">${escapeHtml(objectiveReason(selected))}</div>
    ${metricStrip([
      { label: "Adoption", value: pct((selected.adoption_coverage || 0) * 100), note: "coverage" },
      { label: "Fairness", value: Number(selected.fairness_score || 0).toFixed(2), note: "score" },
      { label: "Denied", value: number(selected.denied_requests || 0), note: "requests" },
    ])}
    <div class="trace-rail-note">${escapeHtml(recommendations?.[0] || "Best fit for this objective and simulation setup.")}</div>
  `;
}

function strategyModelMix(cards) {
  const palette = ["#7ed4f7", "#88a8ff", "#a98cff", "#59df8d", "#f4c45e", "#ff8c8c"];
  if (!cards.some((card) => (card.model_mix || []).length)) return emptyState("No model mix is available.");
  return `
    <div class="rail-list">
      ${cards.map((card) => {
        const total = Math.max(sum((card.model_mix || []).map((entry) => entry.total_cost_usd)), 1);
        return `
          <div class="rail-item">
            <strong>${escapeHtml(card.id)}</strong>
            <div class="rail-note">${number(card.allowed_requests || 0)} allowed · ${number(card.warned_requests || 0)} warned</div>
            <div class="stack-track" style="margin-top:8px">
              ${(card.model_mix || []).map((entry, index) => `<div class="stack-segment" style="width:${(Number(entry.total_cost_usd || 0) / total) * 100}%;background:${palette[index % palette.length]}" title="${escapeHtml(entry.model_id)}"></div>`).join("")}
            </div>
          </div>
        `;
      }).join("")}
    </div>
  `;
}

function renderStrategy(data) {
  cache.strategy = data;
  const simSelect = el("dashboard-simulation-select");
  simSelect.innerHTML = (data.simulations || []).length
    ? data.simulations.map((simulation) => `<option value="${escapeHtml(simulation.id)}"${simulation.id === data.selected_simulation_id ? " selected" : ""}>${escapeHtml(simulation.name || simulation.id)}</option>`).join("")
    : `<option value="">No simulations</option>`;
  if (!state.filters.simulation && data.selected_simulation_id) state.filters.simulation = data.selected_simulation_id;
  const ranked = rankStrategies(data.strategy_cards || [], state.strategyObjective);
  const selected = ensureSelectedStrategy(ranked);
  setChromePills({
    openExceptions: ranked.filter((card) => Number(card.denied_requests || 0) > 0).length,
    sliceLabel: `${objectiveLabel().toLowerCase()} objective`,
  });
  el("dashboard-page-strategy").innerHTML = `
    <div class="dashboard-page">
      <div class="section-head">
        ${sectionCopy("Strategy comparison", strategyScopeNote(data, ranked))}
        <div class="section-actions">
          ${controlChip("Balanced", state.strategyObjective === "balanced", 'data-objective="balanced"')}
          ${controlChip("Cost-first", state.strategyObjective === "cost", 'data-objective="cost"')}
          ${controlChip("Adoption-first", state.strategyObjective === "adoption", 'data-objective="adoption"')}
        </div>
      </div>
      <div class="objective-note">${escapeHtml(objectiveExplanation())}</div>
      <section class="workbench-grid strategy-layout">
        <div class="workbench-main">
          ${surface("Recommendation", "Read the winner against its strongest visible alternative.", strategyHeadToHead(ranked), "matrix-surface")}
          ${evidenceBlock("All strategies", ranked.length <= 2 ? "Quiet support scorecard for the visible options." : "Compact comparison across the selected objective.", strategyMatrix(ranked))}
          ${evidenceBlock("Where they differ", "Single compact support view after the main decision.", strategyDifferenceSummary(ranked))}
        </div>
        <aside class="workbench-rail sticky strategy-rail">
          ${railModule("Why selected", selected?.id || "Recommendation and tradeoff rationale.", strategyRail(selected, data.recommendations || []))}
          ${railModule("Model mix", "Model concentration per strategy.", strategyModelMix(ranked))}
        </aside>
      </section>
    </div>
  `;

  document.querySelectorAll("[data-strategy-id]").forEach((row) => {
    row.addEventListener("click", () => {
      state.selectedStrategy = row.dataset.strategyId || "";
      renderStrategy(cache.strategy);
    });
  });
  document.querySelectorAll("[data-objective]").forEach((button) => {
    button.addEventListener("click", () => {
      state.strategyObjective = button.dataset.objective || "balanced";
      state.selectedStrategy = "";
      renderStrategy(cache.strategy);
      setStatus(`Strategy lab ready · objective ${objectiveLabel()}`);
    });
  });
}

async function loadCurrentPage() {
  setStatus(`Loading ${state.page}…`);
  try {
    const filters = await fetchJson("/v1/dashboard/filters");
    renderSharedFilters(filters);
    if (state.page === "overview") {
      const data = await fetchJson("/v1/dashboard/overview");
      renderOverview(data);
      setStatus(`Overview ready · ${optionLabel(data.filters.windows || [], data.filters.selected_window)} · ${optionLabel(data.filters.lenses || [], data.filters.selected_lens)}`);
      return;
    }
    if (state.page === "budgets") {
      const data = await fetchJson("/v1/dashboard/budgets");
      renderBudgets(data);
      setStatus(`Budgets ready · ${number((data.budgets || []).length)} ranked buckets`);
      return;
    }
    if (state.page === "adoption") {
      const data = await fetchJson("/v1/dashboard/adoption");
      renderAdoption(data);
      setStatus(`Adoption ready · ${number((data.leaderboard || []).length)} ranked entities`);
      return;
    }
    if (state.page === "traces") {
      const data = await fetchJson("/v1/dashboard/traces");
      renderTraces(data);
      setStatus(`Trace explorer ready · ${number((data.timeline || []).length)} events`);
      return;
    }
    const data = await fetchJson("/v1/dashboard/strategy-lab", true);
    renderStrategy(data);
    setStatus(`Strategy lab ready · ${number((data.strategy_cards || []).length)} strategies`);
  } catch (error) {
    console.error(error);
    setStatus(`Unable to load dashboard: ${error.message}`);
  }
}

function wireControls() {
  document.querySelectorAll(".nav-button[data-page]").forEach((button) => {
    button.addEventListener("click", (event) => {
      event.preventDefault();
      showPage(button.dataset.page);
      setMobileNav(false);
      loadCurrentPage();
    });
  });
  el("dashboard-mobile-nav-toggle")?.addEventListener("click", () => {
    setMobileNav(true);
  });
  el("dashboard-shell-scrim")?.addEventListener("click", () => {
    setMobileNav(false);
  });

  el("dashboard-window-select").addEventListener("change", (event) => {
    state.filters.window = event.target.value || "30d";
    loadCurrentPage();
  });

  el("dashboard-lens-select").addEventListener("change", (event) => {
    state.filters.lens = event.target.value || "project";
    state.filters.entity = "";
    state.filters.trace = "";
    state.selectedTraceSpan = "";
    state.traceSpansScrollTop = 0;
    state.selectedAdoption = "";
    state.selectedBudget = "";
    loadCurrentPage();
  });

  el("dashboard-entity-select").addEventListener("change", (event) => {
    state.filters.entity = event.target.value || "";
    state.filters.trace = "";
    state.selectedTraceSpan = "";
    state.traceSpansScrollTop = 0;
    state.selectedAdoption = "";
    state.selectedBudget = "";
    loadCurrentPage();
  });

  el("dashboard-trace-select").addEventListener("change", (event) => {
    state.filters.trace = event.target.value || "";
    state.selectedTraceSpan = "";
    state.traceSpansScrollTop = 0;
    loadCurrentPage();
  });

  el("dashboard-simulation-select").addEventListener("change", (event) => {
    state.filters.simulation = event.target.value || "";
    state.selectedStrategy = "";
    loadCurrentPage();
  });

  el("dashboard-strategy-objective-select").addEventListener("change", (event) => {
    state.strategyObjective = event.target.value || "balanced";
    if (cache.strategy) {
      state.selectedStrategy = "";
      renderStrategy(cache.strategy);
      setStatus(`Strategy lab ready · objective ${objectiveLabel()}`);
    }
  });

  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    if (state.page !== "traces" || (!state.selectedTraceSpan && !state.filters.trace)) return;
    state.selectedTraceSpan = "";
    state.filters.trace = "";
    state.traceSpansScrollTop = 0;
    if (cache.traces) renderTraces(cache.traces);
  });

  document.addEventListener("pointerdown", (event) => {
    closeTraceSpansDrawerIfUnfocused(event.target);
    closeTraceDrawerIfUnfocused(event.target);
  });

  document.addEventListener("focusin", (event) => {
    closeTraceSpansDrawerIfUnfocused(event.target);
    closeTraceDrawerIfUnfocused(event.target);
  });
}

let refreshTimer = null;
let eventSource = null;

function scheduleRefresh() {
  window.clearTimeout(refreshTimer);
  refreshTimer = window.setTimeout(() => loadCurrentPage(), 180);
}

function startLiveUpdates() {
  if (eventSource) eventSource.close();
  eventSource = new EventSource("/v1/reports/updates");
  eventSource.addEventListener("report-update", () => {
    if (state.page !== "strategy") scheduleRefresh();
  });
  eventSource.onerror = () => {
    setStatus(`${el("dashboard-status").textContent} · waiting for live updates`);
  };
}

document.addEventListener("DOMContentLoaded", () => {
  wireControls();
  showPage(state.page);
  startLiveUpdates();
  loadCurrentPage();
});
