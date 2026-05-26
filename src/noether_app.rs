pub fn app_shell() -> &'static str {
    r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Noether</title>
    <link rel="icon" type="image/svg+xml" href="/app/favicon.svg">
    <link rel="stylesheet" href="/app/app.css">
  </head>
  <body>
    <header class="topbar">
      <a class="brand" href="/policy" data-link>
        <img class="brand-mark" src="/app/logo.svg" alt="" aria-hidden="true">
        <span>noether</span>
      </a>
      <div class="crumb">~/work/api <span class="branch">· main</span></div>
      <nav class="modes" aria-label="Primary">
        <a href="/policy" data-link data-mode="policy">Policy</a>
        <a href="/runs" data-link data-mode="runs">Runs</a>
        <a href="/replay" data-link data-mode="replay">Replay</a>
      </nav>
      <div class="cmd" role="button" data-command-button>
        <span class="glyph">›</span>
        <span class="placeholder">ask Noether...</span>
        <span class="kbd">⌘K</span>
      </div>
      <div class="enforce-wedge" data-top-status><span class="pip"></span><span>loading</span></div>
    </header>

    <main>
      <section class="surface" data-surface="policy">
        <div class="page-head">
          <div>
            <div class="eyebrow">policy</div>
            <h1>What&apos;s allowed here.</h1>
            <p class="lede">One file decides what agents can do, what they can spend, and when to ask first. Edits are saved as a draft until you replay and enforce.</p>
          </div>
          <div class="right" data-policy-status>
            <div class="big">loading</div>
            <div class="sub">reading policy</div>
          </div>
        </div>

        <div class="policy-grid">
          <section class="editor">
            <div class="editor-head">
              <span class="path" data-policy-title>policy.yaml</span>
              <span class="modified" data-policy-draft-badge></span>
              <span class="branch" data-policy-path>local file</span>
            </div>
            <div class="editor-code">
              <pre class="editor-highlight" data-policy-highlight aria-hidden="true"></pre>
              <textarea class="editor-source" data-policy-source spellcheck="false" wrap="off" aria-label="Policy source"></textarea>
            </div>
            <div class="editor-foot">
              <div class="policy-state" data-policy-state>loading policy state</div>
              <div class="info" data-policy-save-state>Edits are not enforced until you promote the draft.</div>
              <div class="spacer"></div>
              <button class="btn" type="button" data-policy-revert>Revert editor</button>
              <button class="btn" type="button" data-policy-discard>Discard draft</button>
              <button class="btn" type="button" data-policy-save>Save draft</button>
              <button class="btn primary" type="button" data-policy-enforce>Enforce draft</button>
            </div>
          </section>

          <aside class="tail">
            <div class="card tail-card">
              <div class="card-head">
                <h3>Decisions · live</h3>
                <span class="status"><span class="pulse"></span></span>
                <span class="meta">real ledger · latest</span>
              </div>
              <div class="tail-list" data-live-tail></div>
              <div class="tail-summary" data-tail-summary></div>
            </div>
            <div data-policy-suggestions></div>
          </aside>
        </div>

        <section class="rule-strip">
          <h3>Rule evidence</h3>
          <div class="sub">Hit counts from the local SQLite ledger. Use this to decide what to edit, then replay before enforcing.</div>
          <div class="rule-list" data-rule-stats></div>
        </section>

        <div class="cross">
          <span class="label">next</span>
          <span class="grow" data-policy-next>Save a draft, replay it against history, then enforce.</span>
          <button class="btn" type="button" data-link-button="/runs">→ Open Runs</button>
          <button class="btn primary" type="button" data-link-button="/replay">Replay draft →</button>
        </div>
      </section>

      <section class="surface" data-surface="runs">
        <div class="page-head">
          <div>
            <div class="eyebrow">runs</div>
            <h1>What actually happened.</h1>
            <p class="lede">Every agent run, attributed and decided. Use this feed to find policy pressure, denied work, and spend spikes.</p>
          </div>
          <div class="right" data-runs-status>
            <div class="big">loading</div>
            <div class="sub">reading ledger</div>
          </div>
        </div>

        <div class="filterbar">
          <span class="mono">filter</span>
          <label class="chip">decision
            <select data-runs-filter="decision">
              <option value="any">any</option>
              <option value="allow">allow</option>
              <option value="warn">warn</option>
              <option value="deny">deny</option>
              <option value="ask">ask</option>
            </select>
          </label>
          <label class="chip">rule
            <select data-runs-filter="rule"><option value="any">any</option></select>
          </label>
          <label class="chip search">search
            <input data-runs-filter="q" placeholder="trace, model, rule..." autocomplete="off">
          </label>
          <button class="btn ghost" type="button" data-runs-clear>clear</button>
          <span style="margin-left:auto" class="mono" data-runs-filter-state>/ to search</span>
        </div>
        <div class="runs-table" data-runs-list></div>
      </section>

      <section class="surface" data-surface="replay">
        <div class="page-head">
          <div>
            <div class="eyebrow">replay</div>
            <h1>What would change.</h1>
            <p class="lede">Compare the active policy with your saved draft before flipping enforce. Recent authorization requests are re-evaluated locally against the proposed policy.</p>
          </div>
          <div class="right" data-replay-status>
            <div class="big">loading</div>
            <div class="sub">reading baseline</div>
          </div>
        </div>

        <div class="replay-head">
          <div class="pickers">
            <div class="picker"><span class="k">against</span><span class="v">real · local ledger</span></div>
            <div class="picker"><span class="k">strategies</span><span class="v">current · draft</span></div>
            <div class="picker"><span class="k">scope</span><span class="v">all projects · recent history</span></div>
          </div>
          <button class="btn primary" type="button" data-refresh-replay>Re-run</button>
        </div>
        <div data-replay-body></div>
      </section>
    </main>
    <script src="/app/app.js"></script>
  </body>
</html>"#
}

pub fn app_css() -> String {
    [
        include_str!("../docs/design_handoff_noether/prototype/noether.css"),
        include_str!("../docs/design_handoff_noether/prototype/noether-surfaces.css"),
        include_str!("../docs/design_handoff_noether/prototype/noether-polish.css"),
        include_str!("../assets/noether_app/app.css"),
    ]
    .join("\n")
}

pub fn app_js() -> &'static str {
    include_str!("../assets/noether_app/app.js")
}

pub fn logo_svg() -> &'static str {
    include_str!("../docs/design_handoff_noether/logo/noether-mark.svg")
}

pub fn favicon_svg() -> &'static str {
    include_str!("../docs/design_handoff_noether/logo/favicon.svg")
}
