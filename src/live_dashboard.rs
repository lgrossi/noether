pub fn brand_icon_svg() -> &'static str {
    include_str!("../assets/brand/noether-icon-32-invariant-seal-polished.svg")
}

pub fn dashboard_shell(
    selected_trace: Option<&str>,
    selected_page: Option<&str>,
    selected_simulation: Option<&str>,
) -> String {
    let selected_page = match selected_page {
        Some("overview" | "budgets" | "adoption" | "traces" | "strategy") => selected_page,
        _ => Some("overview"),
    };
    let bootstrap = serde_json::json!({
        "selectedTrace": selected_trace,
        "selectedPage": selected_page,
        "selectedSimulation": selected_simulation,
    });

    format!(
        r###"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Noether dashboard</title>
    <link rel="icon" type="image/svg+xml" href="/dashboard/brand/icon.svg">
    <link rel="stylesheet" href="/dashboard/app.css">
  </head>
  <body>
    <div class="app" id="dashboard-app-shell">
      <button id="dashboard-shell-scrim" class="shell-scrim hidden" type="button" aria-label="Close navigation"></button>
      <aside class="shell dashboard-shell" id="dashboard-shell" aria-label="Dashboard sections">
        <div class="brand">
          <div class="brand-lockup">
            <img src="/dashboard/brand/icon.svg" alt="" class="brand-icon" aria-hidden="true">
            <div class="brand-copy">
              <strong>Noether</strong>
            </div>
          </div>
        </div>
        <nav class="nav shell-nav">
          <a href="#overview" class="nav-link nav-button active" data-page="overview" aria-label="Overview" title="Overview">
            <span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 16l4-4 3 3 6-7 3 3"/><path d="M4 20h16"/></svg></span>
            <span class="nav-label"><strong>Overview</strong></span>
          </a>
          <a href="#budgets" class="nav-link nav-button" data-page="budgets" aria-label="Budgets" title="Budgets">
            <span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="6" width="18" height="12" rx="2"/><path d="M3 10h18"/><path d="M7 14h4"/></svg></span>
            <span class="nav-label"><strong>Budgets</strong></span>
          </a>
          <a href="#adoption" class="nav-link nav-button" data-page="adoption" aria-label="Adoption" title="Adoption">
            <span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M16 19v-1a4 4 0 0 0-4-4H7a4 4 0 0 0-4 4v1"/><circle cx="9.5" cy="7" r="3"/><path d="M20 19v-1a4 4 0 0 0-3-3.87"/><path d="M15 4.13a3 3 0 0 1 0 5.74"/></svg></span>
            <span class="nav-label"><strong>Adoption</strong></span>
          </a>
          <a href="#traces" class="nav-link nav-button" data-page="traces" aria-label="Traces" title="Traces">
            <span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="6" r="2"/><circle cx="18" cy="6" r="2"/><circle cx="12" cy="18" r="2"/><path d="M8 6h8"/><path d="M7.5 7.5l3 7"/><path d="M16.5 7.5l-3 7"/></svg></span>
            <span class="nav-label"><strong>Traces</strong></span>
          </a>
          <a href="#strategy" class="nav-link nav-button" data-page="strategy" aria-label="Simulation" title="Simulation">
            <span class="nav-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 21h16"/><path d="M7 17V9"/><path d="M12 17V5"/><path d="M17 17v-6"/></svg></span>
            <span class="nav-label"><strong>Simulation</strong></span>
          </a>
        </nav>
      </aside>

      <div class="viewport workspace-shell">
        <header class="topbar">
          <div class="topbar-left utility-meta">
            <button id="dashboard-mobile-nav-toggle" class="mobile-nav-button mobile-nav-toggle" type="button" aria-label="Toggle navigation">
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16"/><path d="M4 12h16"/><path d="M4 17h16"/></svg>
            </button>
            <span class="pill utility-pill">Workspace <strong id="dashboard-workspace-title">Overview</strong></span>
            <span class="pill utility-pill"><span class="dot green"></span>Live ledger</span>
            <span class="pill utility-pill"><span class="dot amber"></span>Enforce</span>
            <span class="pill utility-pill">Open exceptions <strong id="dashboard-open-exceptions">0</strong></span>
            <div id="dashboard-status" class="pill status-pill" role="status">Loading dashboard…</div>
          </div>
          <div class="topbar-right utility-meta">
            <span class="pill utility-pill">Lens <strong id="dashboard-lens-pill">team + project</strong></span>
            <span class="pill utility-pill">Window <strong id="dashboard-window-pill">last 30 days</strong></span>
            <span class="pill utility-pill">Exceptions <strong id="dashboard-slice-pill">open only</strong></span>
          </div>
        </header>

        <section class="controlbar" aria-label="Dashboard filters">
          <label class="field" id="dashboard-window-field">
            <span>Window</span>
            <select id="dashboard-window-select" aria-label="Time range"></select>
          </label>
          <label class="field" id="dashboard-lens-field">
            <span>Lens</span>
            <select id="dashboard-lens-select" aria-label="Scope grouping"></select>
          </label>
          <label class="field field-span-2" id="dashboard-entity-field">
            <span id="dashboard-entity-label">Entity</span>
            <select id="dashboard-entity-select" aria-label="Focus entity"></select>
          </label>
          <label class="field hidden" id="dashboard-trace-field">
            <span>Trace</span>
            <select id="dashboard-trace-select" aria-label="Selected trace"></select>
          </label>
          <label class="field hidden" id="dashboard-simulation-field">
            <span>Simulation</span>
            <select id="dashboard-simulation-select" aria-label="Simulation selection"></select>
          </label>
          <label class="field hidden" id="dashboard-strategy-objective-field">
            <span>Objective</span>
            <select id="dashboard-strategy-objective-select" aria-label="Strategy objective">
              <option value="balanced">Balanced</option>
              <option value="cost">Cost-first</option>
              <option value="adoption">Adoption-first</option>
            </select>
          </label>
        </section>

        <main class="content workspace">
          <section id="dashboard-page-overview" class="page section is-active active" data-view="overview"></section>
          <section id="dashboard-page-budgets" class="page section" data-view="budgets"></section>
          <section id="dashboard-page-adoption" class="page section" data-view="adoption"></section>
          <section id="dashboard-page-traces" class="page section" data-view="traces"></section>
          <section id="dashboard-page-strategy" class="page section" data-view="strategy"></section>
        </main>
      </div>
    </div>

    <script>window.NOETHER_DASHBOARD_BOOTSTRAP = {bootstrap};</script>
    <script src="/dashboard/app.js"></script>
  </body>
</html>"###,
        bootstrap = bootstrap,
    )
}

pub fn dashboard_css() -> &'static str {
    include_str!("../assets/dashboard/app.css")
}

pub fn dashboard_js() -> &'static str {
    include_str!("../assets/dashboard/app.js")
}
