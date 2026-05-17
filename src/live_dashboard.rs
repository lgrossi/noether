pub fn dashboard_shell(selected_trace: Option<&str>) -> String {
    let bootstrap = serde_json::json!({
        "selectedTrace": selected_trace,
    });
    format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Noether live dashboard</title>
    <link rel="stylesheet" href="/dashboard/app.css">
  </head>
  <body>
    <main class="dashboard-shell">
      <header class="hero">
        <div>
          <p class="eyebrow">Live dashboard</p>
          <h1>Noether live dashboard</h1>
          <p class="sub">
            Browser-served reporting surface backed by the live reporting API.
            This is distinct from the static export dashboard artifact.
          </p>
        </div>
        <div class="hero-actions">
          <label class="trace-picker">
            <span>Trace</span>
            <select id="dashboard-trace-select" aria-label="Trace selection">
              <option value="">Latest</option>
            </select>
          </label>
          <button id="dashboard-refresh" type="button">Refresh</button>
        </div>
      </header>

      <section id="dashboard-status" class="status" role="status">Loading dashboard...</section>
      <section id="dashboard-overview" class="overview" aria-live="polite"></section>
      <section id="dashboard-policy" class="panel-block"></section>
      <section id="dashboard-spend" class="panel-block"></section>
      <section id="dashboard-evidence" class="panel-block"></section>
    </main>

    <script>window.NOETHER_DASHBOARD_BOOTSTRAP = {bootstrap};</script>
    <script src="/dashboard/app.js"></script>
  </body>
</html>"#
    )
}

pub fn dashboard_css() -> &'static str {
    r#":root {
  color-scheme: dark;
  --bg: #0f172a;
  --bg-accent: #172554;
  --panel: rgba(17, 28, 51, 0.9);
  --panel-alt: rgba(15, 23, 42, 0.7);
  --line: #263449;
  --text: #e5edf7;
  --muted: #94a3b8;
  --good: #22c55e;
  --warn: #f59e0b;
  --bad: #ef4444;
  --blue: #38bdf8;
  --violet: #a78bfa;
}

* { box-sizing: border-box; }
body {
  margin: 0;
  font: 15px/1.5 system-ui, -apple-system, Segoe UI, sans-serif;
  background: radial-gradient(circle at top left, var(--bg-accent), var(--bg) 42%);
  color: var(--text);
}
main {
  max-width: 1180px;
  margin: 0 auto;
  padding: 32px 20px 48px;
}
h1, h2, h3, p { margin-top: 0; }
h1 { margin-bottom: 6px; font-size: 34px; letter-spacing: -0.04em; }
h2 { margin-bottom: 12px; font-size: 24px; letter-spacing: -0.03em; }
.eyebrow, .metric-label, .section-eyebrow {
  color: var(--muted);
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: .08em;
}
.sub, .muted, .status-note, .entry-meta, .empty {
  color: var(--muted);
}
.hero, .panel {
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 20px;
  box-shadow: 0 18px 55px rgba(0, 0, 0, .22);
}
.hero {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: flex-start;
  padding: 24px;
}
.hero-actions {
  display: flex;
  gap: 12px;
  align-items: end;
  flex-wrap: wrap;
}
.trace-picker {
  display: grid;
  gap: 6px;
}
select, button {
  min-height: 40px;
  border-radius: 12px;
  border: 1px solid var(--line);
  background: rgba(15, 23, 42, 0.9);
  color: var(--text);
  padding: 0 12px;
}
button { cursor: pointer; }
.status {
  margin-top: 14px;
  min-height: 24px;
  color: var(--muted);
}
.overview {
  margin-top: 14px;
  display: grid;
  gap: 14px;
  grid-template-columns: 1.25fr 1fr;
}
.panel, .metric-card {
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 18px;
}
.metric-grid {
  display: grid;
  gap: 14px;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}
.story-panel, .metric-card, .panel {
  padding: 18px;
}
.metric-value {
  margin-top: 6px;
  font-size: 30px;
  font-weight: 800;
  letter-spacing: -0.03em;
}
.panel-block {
  margin-top: 28px;
}
.section-header {
  margin-bottom: 14px;
}
.section-grid {
  display: grid;
  gap: 14px;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}
.entry-list {
  display: grid;
  gap: 12px;
}
.entry-card {
  padding: 16px;
  border-radius: 16px;
  border: 1px solid rgba(148, 163, 184, .14);
  background: var(--panel-alt);
}
.pill {
  display: inline-flex;
  align-items: center;
  padding: 4px 9px;
  border-radius: 999px;
  border: 1px solid var(--line);
  background: rgba(30, 41, 59, .85);
  color: var(--text);
  font-size: 12px;
}
.pill.allow { color: var(--good); }
.pill.warn { color: var(--warn); }
.pill.deny { color: var(--bad); }
.list-inline {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
}
.empty {
  padding: 16px;
  border: 1px dashed var(--line);
  border-radius: 14px;
  background: rgba(15, 23, 42, .45);
}
code {
  color: var(--blue);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
@media (max-width: 900px) {
  .hero, .overview, .section-grid, .metric-grid { grid-template-columns: 1fr; display: grid; }
}
"#
}

pub fn dashboard_js() -> &'static str {
    r#"const bootstrap = window.NOETHER_DASHBOARD_BOOTSTRAP || {};

function el(id) {
  return document.getElementById(id);
}

function money(value) {
  if (!value) return "$0";
  if (value < 0.01) return `$${value.toFixed(4)}`;
  return `$${value.toFixed(2)}`;
}

function number(value) {
  return new Intl.NumberFormat().format(value || 0);
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function outcomeClass(kind) {
  if ((kind || "").endsWith(".deny")) return "deny";
  if ((kind || "").endsWith(".warn")) return "warn";
  return "allow";
}

function sectionHeader(eyebrow, title, summary) {
  return `
    <div class="section-header">
      <div class="section-eyebrow">${escapeHtml(eyebrow)}</div>
      <h2>${escapeHtml(title)}</h2>
      <p class="muted">${escapeHtml(summary)}</p>
    </div>
  `;
}

function metricCard(label, value, hint) {
  return `
    <article class="metric-card">
      <div class="metric-label">${escapeHtml(label)}</div>
      <div class="metric-value">${escapeHtml(value)}</div>
      <div class="status-note">${escapeHtml(hint)}</div>
    </article>
  `;
}

function renderOverview(data) {
  const trace = data.featured_trace_id || "latest";
  const lead = data.decisions[0]?.summary || "No authorization decisions yet.";
  el("dashboard-overview").innerHTML = `
    <article class="story-panel panel">
      <div class="eyebrow">Outcome summary</div>
      <h2>Live reporting for <code>${escapeHtml(trace)}</code></h2>
      <p>${escapeHtml(lead)}</p>
      <div class="list-inline">
        <span class="pill ${outcomeClass(data.decisions[0]?.kind)}">${escapeHtml(data.decisions[0]?.kind || "no decisions yet")}</span>
        <span class="pill">traces ${number(data.available_traces.length)}</span>
        <span class="pill">observations ${number(data.observations.length)}</span>
      </div>
    </article>
    <section class="metric-grid">
      ${metricCard("Finalized spend", money(data.usage.total_cost_usd), "finalized cost in the current ledger view")}
      ${metricCard("Tokens", number(data.summary.usage.total_tokens), "finalized token volume")}
      ${metricCard("Decision mix", `${number(data.summary.decisions.allow)} allow · ${number(data.summary.decisions.warn)} warn · ${number(data.summary.decisions.deny)} deny`, "policy outcomes in the selected slice")}
      ${metricCard("Evidence", `${number(data.summary.activity.tools)} tools · ${number(data.summary.activity.agent)} agent · ${number(data.summary.activity.skill_context)} context`, "activity attached to the selected trace")}
    </section>
  `;
}

function renderPolicy(data) {
  const root = el("dashboard-policy");
  root.innerHTML = sectionHeader(
    "Policy",
    "Policy decisions",
    "This live view shows the current authorize outcomes and their routing evidence."
  );
  if (!data.summary.sections.policy) {
    root.innerHTML += '<div class="empty">No policy decisions have been recorded yet.</div>';
    return;
  }
  root.innerHTML += `
    <div class="section-grid">
      <section class="panel">
        <h3>Recent decisions</h3>
        <div class="entry-list">
          ${data.decisions.slice(0, 8).map((item) => `
            <article class="entry-card">
              <div class="list-inline">
                <span class="pill ${outcomeClass(item.kind)}">${escapeHtml(item.kind)}</span>
                ${item.routing?.selected_budget_id ? `<span class="pill">${escapeHtml(item.routing.selected_budget_id)}</span>` : ""}
              </div>
              <p>${escapeHtml(item.summary)}</p>
            </article>
          `).join("")}
        </div>
      </section>
      <section class="panel">
        <h3>Trace switching</h3>
        ${data.available_traces.length ? `
          <div class="entry-list">
            ${data.available_traces.map((trace) => `
              <article class="entry-card">
                <p><code>${escapeHtml(trace.trace_id)}</code></p>
                <p class="entry-meta">${escapeHtml(trace.latest_decision_kind || "decision")}</p>
                <p>${escapeHtml(trace.latest_decision_summary)}</p>
              </article>
            `).join("")}
          </div>
        ` : '<div class="empty">No trace-backed decisions are available yet.</div>'}
      </section>
    </div>
  `;
}

function renderSpend(data) {
  const root = el("dashboard-spend");
  root.innerHTML = sectionHeader(
    "Spend",
    "Spend and adoption",
    "Finalized usage rows and protected-adoption signals are rendered from live report data."
  );
  if (!data.summary.sections.spend) {
    root.innerHTML += '<div class="empty">No finalized usage has landed yet.</div>';
    return;
  }
  const usageRows = data.usage.rows || [];
  const adoption = data.usage.protected_adoption;
  root.innerHTML += `
    <div class="section-grid">
      <section class="panel">
        <h3>Usage rows</h3>
        ${usageRows.length ? `
          <div class="entry-list">
            ${usageRows.slice(0, 8).map((row) => `
              <article class="entry-card">
                <p><strong>${escapeHtml(row.project || row.subject || "unattributed")}</strong></p>
                <p class="entry-meta">${escapeHtml(row.provider || "provider")} · ${escapeHtml(row.model || "model")}</p>
                <p>${money(row.total_cost_usd)} · ${number(row.total_tokens)} tokens</p>
              </article>
            `).join("")}
          </div>
        ` : '<div class="empty">No usage rows are available yet.</div>'}
      </section>
      <section class="panel">
        <h3>Adoption view</h3>
        ${adoption ? `
          <div class="entry-list">
            <article class="entry-card">
              <p><strong>Protected opportunity</strong></p>
              <p>${money(adoption.unused_protected_opportunity_usd)}</p>
            </article>
            <article class="entry-card">
              <p><strong>Carryover liability</strong></p>
              <p>${money(adoption.carryover_liability_usd)}</p>
            </article>
            <article class="entry-card">
              <p><strong>Low adopters</strong></p>
              <p>${number(adoption.low_adopters.length)}</p>
            </article>
            <article class="entry-card">
              <p><strong>High adopters</strong></p>
              <p>${number(adoption.high_adopters.length)}</p>
            </article>
          </div>
        ` : '<div class="empty">Protected-adoption reporting is not active in this ledger slice.</div>'}
      </section>
    </div>
  `;
}

function renderEvidence(data) {
  const root = el("dashboard-evidence");
  root.innerHTML = sectionHeader(
    "Evidence",
    "Run evidence",
    "Trace items and observations explain how the selected run unfolded."
  );
  const traceItems = data.trace?.items || [];
  if (!data.summary.sections.evidence) {
    root.innerHTML += '<div class="empty">No trace or observation evidence is available yet.</div>';
    return;
  }
  root.innerHTML += `
    <div class="section-grid">
      <section class="panel">
        <h3>Trace timeline</h3>
        ${traceItems.length ? `
          <div class="entry-list">
            ${traceItems.slice(0, 10).map((item) => `
              <article class="entry-card">
                <div class="list-inline"><span class="pill">${escapeHtml(item.kind)}</span></div>
                <p>${escapeHtml(item.summary)}</p>
              </article>
            `).join("")}
          </div>
        ` : '<div class="empty">No trace items were returned for the selected trace.</div>'}
      </section>
      <section class="panel">
        <h3>Observations</h3>
        ${(data.observations || []).length ? `
          <div class="entry-list">
            ${data.observations.slice(0, 10).map((item) => `
              <article class="entry-card">
                <div class="list-inline"><span class="pill">${escapeHtml(item.kind)}</span></div>
                <p>${escapeHtml(item.summary)}</p>
              </article>
            `).join("")}
          </div>
        ` : '<div class="empty">No observations matched the selected trace yet.</div>'}
      </section>
    </div>
  `;
}

function updateTracePicker(data, selectedTrace) {
  const select = el("dashboard-trace-select");
  const chosen = selectedTrace ?? data.featured_trace_id ?? "";
  const options = ['<option value="">Latest</option>'].concat(
    data.available_traces.map((trace) => {
      const selected = trace.trace_id === chosen ? " selected" : "";
      return `<option value="${escapeHtml(trace.trace_id)}"${selected}>${escapeHtml(trace.trace_id)}</option>`;
    })
  );
  select.innerHTML = options.join("");
}

async function fetchDashboard(trace) {
  const url = new URL("/v1/reports/dashboard-data", window.location.origin);
  if (trace) url.searchParams.set("trace", trace);
  const response = await fetch(url, { headers: { Accept: "application/json" } });
  if (!response.ok) {
    throw new Error(`dashboard fetch failed: ${response.status}`);
  }
  return response.json();
}

async function loadDashboard(trace) {
  const status = el("dashboard-status");
  status.textContent = "Loading dashboard...";
  try {
    const data = await fetchDashboard(trace);
    updateTracePicker(data, trace);
    renderOverview(data);
    renderPolicy(data);
    renderSpend(data);
    renderEvidence(data);
    status.textContent = `Showing ${data.featured_trace_id || "latest"} · ${number(data.decisions.length)} decisions · ${number(data.observations.length)} observations`;
  } catch (error) {
    console.error(error);
    status.textContent = `Unable to load dashboard: ${error.message}`;
  }
}

function currentTrace() {
  return el("dashboard-trace-select").value || bootstrap.selectedTrace || "";
}

document.addEventListener("DOMContentLoaded", () => {
  el("dashboard-refresh").addEventListener("click", () => loadDashboard(currentTrace()));
  el("dashboard-trace-select").addEventListener("change", (event) => {
    const trace = event.target.value || "";
    const nextUrl = new URL(window.location.href);
    if (trace) nextUrl.searchParams.set("trace", trace);
    else nextUrl.searchParams.delete("trace");
    window.history.replaceState({}, "", nextUrl);
    bootstrap.selectedTrace = trace || null;
    loadDashboard(trace);
  });
  loadDashboard(bootstrap.selectedTrace || "");
});
"#
}
