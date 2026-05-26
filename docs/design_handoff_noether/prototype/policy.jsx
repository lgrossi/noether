/* global React */
const { useMemo, useState, useEffect, useRef } = React;

// ---------- helpers ----------
function fmt$(n) {
  if (n == null) return "—";
  if (n === 0) return "$0.00";
  return "$" + n.toFixed(2);
}
function fmtAgo(dt, now) {
  const s = (now - dt) / 1000;
  if (s < 60) return Math.floor(s) + "s";
  if (s < 3600) return Math.floor(s/60) + "m";
  if (s < 86400) return Math.floor(s/3600) + "h";
  return Math.floor(s/86400) + "d";
}
function fmtClock(dt) {
  return String(dt.getHours()).padStart(2, "0") + ":" + String(dt.getMinutes()).padStart(2, "0");
}
function fmtDay(dt) {
  const d = new Date(dt); d.setHours(0,0,0,0);
  return d.toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" });
}
function dayKey(dt) {
  const d = new Date(dt); d.setHours(0,0,0,0); return d.getTime();
}

const DECISION_GLYPHS = { allow: "●", warn: "▲", deny: "✕", ask: "?" };
const DECISION_CLASSES = { allow: "ok", warn: "warn", deny: "deny", ask: "ask" };

window.Noet = { fmt$, fmtAgo, fmtClock, fmtDay, dayKey, DECISION_GLYPHS, DECISION_CLASSES };

// ---------- POLICY SURFACE ----------
function PolicySurface({ state, dispatch }) {
  const { runs, evalAll, ruleFocus, now } = state;
  // Editor + tail + rule strip visualize the proposed (in-progress) policy.
  // The baseline `state.policy` is what's saved/enforced; Replay diffs the two.
  const policy = state.proposed;

  // tail = most recent 12 runs with their evaluation
  const tail = useMemo(() => {
    return runs.slice(0, 14).map(r => ({ run: r, e: evalAll.results.get(r.id) }));
  }, [runs, evalAll]);

  // suggestion: prefer tightening retries (catches runaways sooner) if it would
  // have helped; otherwise raise cap if it's blocking real work.
  const suggestion = useMemo(() => {
    if (state.appliedSuggestion) return null;
    const retryDenies = evalAll.tally["limits.retries"]?.deny || 0;
    const budget = policy.budgets[0];
    const capDenies = evalAll.tally[`budgets.${budget.id}.cap_usd`]?.deny || 0;
    if (retryDenies >= 2) {
      return {
        type: "retries_lower",
        from: policy.limits.retries,
        to: Math.max(2, policy.limits.retries - 1),
        savedCount: retryDenies,
        copy: <><b>Retries are doing real work.</b> {retryDenies} runs hit the cap of {policy.limits.retries} this week — most were tool loops on incident. Dropping to {policy.limits.retries - 1} would catch them one step sooner.</>,
      };
    }
    if (capDenies >= 1) {
      const recommended = Math.ceil((evalAll.totals.totalSpend + evalAll.totals.prevented) * 1.05);
      return {
        type: "cap_raise",
        from: budget.cap_usd,
        to: Math.max(recommended, budget.cap_usd + 15),
        copy: <><b>Cap is too tight.</b> <span className="mono" style={{ fontSize: 12 }}>cap_usd</span> denied {capDenies} run(s). ${recommended} covers the period with headroom.</>,
      };
    }
    return null;
  }, [policy, evalAll, state.appliedSuggestion]);

  function toggleRuleFocus(rule) {
    dispatch({ type: "ruleFocus", rule: ruleFocus === rule ? null : rule });
  }

  function clickRuleInRuns(rule) {
    dispatch({ type: "ruleFocus", rule });
    dispatch({ type: "go", mode: "runs" });
  }

  return (
    <section className="surface active" data-screen-label="01 Policy">
      <div className="page-head">
        <div>
          <div className="eyebrow">policy</div>
          <h1>What's allowed here.</h1>
          <p className="lede">
            One file decides what agents can do, what they can spend, and when to ask first.
            Edits are dry-run until you flip enforce.
          </p>
        </div>
        <div className="right">
          <div className="big">7 rules</div>
          <div className="sub">{policy.enforced ? "enforced · decisions block" : "dry-run · decisions logged"}</div>
        </div>
      </div>

      <div className="policy-grid">

        <PolicyEditor
          policy={policy}
          tally={evalAll.tally}
          ruleFocus={ruleFocus}
          dirty={state.dirty}
          onEdit={(patch) => dispatch({ type: "patchPolicy", patch })}
          onLineClick={(rule) => toggleRuleFocus(rule)}
          onEnforce={() => dispatch({ type: "enforce" })}
          onReplay={() => dispatch({ type: "go", mode: "replay" })}
        />

        <aside className="tail">
          <div className="card tail-card">
            <div className="card-head">
              <h3>Decisions · live</h3>
              <span className="status" style={{ marginLeft: 6 }}><span className="pulse"></span></span>
              <span className="meta">{ruleFocus ? <>filter: <span className="mono" style={{ fontSize: 11, color: "var(--accent)" }}>{ruleFocus}</span> · <a onClick={() => dispatch({ type: "ruleFocus", rule: null })} style={{ cursor:"pointer", color:"var(--ink-soft)" }}>clear</a></> : "last 24h · all rules"}</span>
            </div>
            <div className="tail-list">
              {tail.filter(({ e }) => !ruleFocus || e.rule === ruleFocus).slice(0, 10).map(({ run, e }, i) => (
                <div
                  key={run.id}
                  className={`tail-row is-${e.decision} ${i === 0 ? "is-new" : ""}`}
                  onClick={() => dispatch({ type: "openRun", id: run.id })}
                >
                  <span className="t">{fmtClock(run.dt)}</span>
                  <span className={`glyph ${DECISION_CLASSES[e.decision]}`}>{DECISION_GLYPHS[e.decision]}</span>
                  <span className="what">
                    {run.agent} · {run.purpose.length > 44 ? run.purpose.slice(0, 44) + "…" : run.purpose}
                    {" "}<span className="ref">{run.id}</span>
                    {e.rule && <> <span className="ref" style={{ color: "var(--ink-faint)" }}>· {e.rule.replace(/^[^.]+\./, "")}</span></>}
                  </span>
                  <span className="cost">{e.decision === "deny" ? "blocked" : e.decision === "ask" ? "waiting" : fmt$(run.cost)}</span>
                </div>
              ))}
            </div>
            <div className="tail-summary">
              <span><b>{evalAll.totals.allowed + evalAll.totals.warned + evalAll.totals.denied + evalAll.totals.asked}</b> in window</span>
              <span><b style={{ color: "var(--ok)" }}>{evalAll.totals.allowed}</b> allow</span>
              <span><b style={{ color: "var(--warn)" }}>{evalAll.totals.warned}</b> warn</span>
              <span><b style={{ color: "var(--deny)" }}>{evalAll.totals.denied}</b> deny</span>
              <span><b style={{ color: "var(--accent)" }}>{evalAll.totals.asked}</b> ask</span>
              <span style={{ marginLeft: "auto", cursor: "pointer" }} onClick={() => dispatch({ type: "go", mode: "runs" })}>
                → open in Runs
              </span>
            </div>
          </div>

          {suggestion && (
            <div className="suggest" role="note">
              <span className="glyph">⌁</span>
              <div className="body">
                <p>{suggestion.copy}</p>
                <div className="diff">
                  {suggestion.type === "retries_lower" ? (
                    <>
                      <span><span className="minus">- retries: {suggestion.from}</span></span>
                      <span><span className="plus">+ retries: {suggestion.to}</span></span>
                    </>
                  ) : (
                    <>
                      <span><span className="minus">- cap_usd: "${suggestion.from}"</span></span>
                      <span><span className="plus">+ cap_usd: "${suggestion.to}"</span></span>
                    </>
                  )}
                </div>
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <button className="btn primary" onClick={() => dispatch({ type: "applySuggestion", suggestion })}>Apply</button>
                <button className="btn ghost" onClick={() => dispatch({ type: "applySuggestion", suggestion, thenGo: "replay" })}>Simulate first</button>
              </div>
            </div>
          )}
        </aside>
      </div>

      <RuleStrip
        tally={evalAll.tally}
        ruleFocus={ruleFocus}
        onPick={(rule) => clickRuleInRuns(rule)}
      />

      <div className="cross">
        <span className="label">next</span>
        <span className="grow">
          {state.dirty
            ? <>You have <b>unsaved policy changes</b>. Replay before enforcing.</>
            : policy.enforced
              ? <>Policy is enforced. Decisions are now blocking.</>
              : <>Policy is in dry-run. Edits won't block anything yet.</>
          }
        </span>
        {state.dirty && (
          <button className="btn" onClick={() => dispatch({ type: "revertChanges" })}>
            Revert
          </button>
        )}
        <button className="btn" onClick={() => dispatch({ type: "go", mode: "replay" })}>
          {state.dirty ? "Replay diff" : "Replay against last 7d"} <span className="arrow">→</span>
        </button>
        <button
          className={`btn ${policy.enforced ? "" : "primary"}`}
          onClick={() => dispatch({ type: policy.enforced ? "unenforce" : "enforce" })}
        >
          {policy.enforced ? "Back to dry-run" : (state.dirty ? "Adopt & enforce" : "Enforce")}
        </button>
      </div>
    </section>
  );
}

// ---------- EDITOR ----------
function PolicyEditor({ policy, tally, ruleFocus, dirty, onEdit, onLineClick, onEnforce, onReplay }) {
  // We render the YAML by hand so values stay inline-editable. Lines map to rules.
  const lines = [
    { n: 1, code: <span className="yc"># local-first · observe → warn → enforce</span>, rule: null },
    { n: 2, code: <><span className="yk">defaults</span>:</>, rule: null },
    { n: 3, code: <>  <span className="yk">decision_mode</span>: <span className="ys">{policy.defaults.decision_mode}</span></>, rule: null, tally: <>applied to <b>all</b></> },
    { n: 4, code: <>  <span className="yk">attribute_to</span>: <span className="ys">{policy.defaults.attribute_to}</span></>, rule: null, tally: <><b>5</b> projects</> },
    { n: 5, code: "", rule: null },
    { n: 6, code: <><span className="yk">models</span>:</>, rule: null },
    { n: 7, code: <>  <span className="yk">allow</span>: [{policy.models.allow.map((g, i) => <React.Fragment key={i}><span className="ys">"{g}"</span>{i < policy.models.allow.length - 1 ? ", " : ""}</React.Fragment>)}]</>, rule: "models.allow", tally: <><span className="ok">{(tally["models.allow"]?.allow) || 0} allow</span></> },
    { n: 8, code: <>  <span className="yk">deny</span>:  [{policy.models.deny.map((g, i) => <React.Fragment key={i}><span className="ys">"{g}"</span>{i < policy.models.deny.length - 1 ? ", " : ""}</React.Fragment>)}]</>, rule: "models.deny", tally: tally["models.deny"]?.deny ? <span className="deny">{tally["models.deny"].deny} deny</span> : null },
    { n: 9, code: "", rule: null },
    { n: 10, code: <><span className="yk">budgets</span>:</>, rule: null },
    { n: 11, code: <>  - <span className="yk">id</span>: <span className="ys">{policy.budgets[0].id}</span></>, rule: null },
    { n: 12, code: <>    <span className="yk">window</span>: <span className="ys">"{policy.budgets[0].window}"</span></>, rule: null },
    { n: 13, code: <>    <span className="yk">cap_usd</span>: <EditableNumber value={policy.budgets[0].cap_usd} onChange={(v) => onEdit({ kind: "cap_usd", value: v })} prefix='"$' suffix='"' /></>, rule: `budgets.${policy.budgets[0].id}.cap_usd`, tally: (tally[`budgets.${policy.budgets[0].id}.cap_usd`]?.deny || 0) > 0 ? <><span className="deny">{tally[`budgets.${policy.budgets[0].id}.cap_usd`].deny} deny</span></> : null, hasIssue: (tally[`budgets.${policy.budgets[0].id}.cap_usd`]?.deny || 0) > 0 },
    { n: 14, code: <>    <span className="yk">on_exhaust</span>: <span className="ys">{policy.budgets[0].on_exhaust}</span></>, rule: null },
    { n: 15, code: "", rule: null },
    { n: 16, code: <><span className="yk">limits</span>:</>, rule: null },
    { n: 17, code: <>  <span className="yk">request_cost_usd</span>: <EditableNumber value={policy.limits.request_cost_usd} step={0.25} onChange={(v) => onEdit({ kind: "request_cost_usd", value: v })} /></>, rule: "limits.request_cost_usd", tally: tally["limits.request_cost_usd"]?.warn ? <><span className="warn">{tally["limits.request_cost_usd"].warn} warn</span></> : null },
    { n: 18, code: <>  <span className="yk">tools_per_turn</span>:  <EditableNumber value={policy.limits.tools_per_turn} step={1} int onChange={(v) => onEdit({ kind: "tools_per_turn", value: v })} /></>, rule: "limits.tools_per_turn", tally: tally["limits.tools_per_turn"]?.warn ? <><span className="warn">{tally["limits.tools_per_turn"].warn} warn</span></> : null },
    { n: 19, code: <>  <span className="yk">retries</span>:         <EditableNumber value={policy.limits.retries} step={1} int onChange={(v) => onEdit({ kind: "retries", value: v })} /></>, rule: "limits.retries", tally: tally["limits.retries"]?.deny ? <><span className="deny">{tally["limits.retries"].deny} deny</span></> : <b>—</b> },
    { n: 20, code: "", rule: null },
    { n: 21, code: <><span className="yk">tools</span>:</>, rule: null },
    { n: 22, code: <>  <span className="yk">ask_for</span>: [{policy.tools.ask_for.map((t, i) => <React.Fragment key={i}><span className="ys">"{t}"</span>{i < policy.tools.ask_for.length - 1 ? ", " : ""}</React.Fragment>)}]</>, rule: "tools.ask_for", tally: tally["tools.ask_for"]?.ask ? <><span className="ask">{tally["tools.ask_for"].ask} ask</span></> : null },
  ];

  return (
    <div className="editor" aria-label="Policy file">
      <div className="editor-head">
        <span className="path">policy.noet.yaml</span>
        {dirty && <span className="modified">● modified</span>}
        <span className="branch">main · 4h</span>
      </div>
      <pre>
        {lines.map(l => (
          <span
            key={l.n}
            className={`ln ${l.hasIssue ? "has-issue" : ""} ${ruleFocus && l.rule === ruleFocus ? "active" : ""}`}
            onClick={() => l.rule && onLineClick(l.rule)}
          >
            <span className="gutter">{l.n}</span>
            <span className="code">{l.code || " "}</span>
            <span className="tally">{l.tally || ""}</span>
          </span>
        ))}
      </pre>
      <div className="editor-foot">
        <span className={dirty ? "dirty" : "saved"}>
          {dirty ? "● unsaved changes" : "✓ saved"}
        </span>
        <div style={{ flex: 1 }} />
        <button className="btn" onClick={onReplay}>Replay against last 7d <span className="arrow">→</span></button>
        <button
          className={`btn ${policy.enforced ? "" : "primary"}`}
          onClick={onEnforce}
        >
          {policy.enforced ? "Re-enforce" : "Enforce"}
        </button>
      </div>
    </div>
  );
}

function EditableNumber({ value, onChange, step = 1, int = false, prefix = "", suffix = "" }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const ref = useRef(null);
  useEffect(() => { if (editing && ref.current) { ref.current.focus(); ref.current.select(); } }, [editing]);
  if (!editing) {
    return (
      <span
        className="yn editable"
        tabIndex={0}
        onClick={(e) => { e.stopPropagation(); setDraft(String(value)); setEditing(true); }}
        title="click to edit"
      >
        {prefix}{value}{suffix}
      </span>
    );
  }
  return (
    <input
      ref={ref}
      type="number"
      step={step}
      value={draft}
      onClick={(e) => e.stopPropagation()}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => {
        let v = Number(draft);
        if (Number.isFinite(v)) onChange(int ? Math.round(v) : v);
        setEditing(false);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.target.blur();
        if (e.key === "Escape") setEditing(false);
      }}
      style={{
        width: 64,
        font: '500 13px "Geist Mono", monospace',
        color: "var(--accent)",
        border: "1px solid var(--accent)",
        background: "#fff",
        padding: "0 4px",
        borderRadius: 3,
      }}
    />
  );
}

// ---------- RULE STRIP ----------
function RuleStrip({ tally, ruleFocus, onPick }) {
  const rows = [
    { rule: "models.allow", name: "models.allow", line: 7 },
    { rule: "budgets.personal-local.cap_usd", name: "budgets.personal-local.cap_usd", line: 13 },
    { rule: "limits.request_cost_usd", name: "limits.request_cost_usd", line: 17 },
    { rule: "limits.tools_per_turn", name: "limits.tools_per_turn", line: 18 },
    { rule: "limits.retries", name: "limits.retries", line: 19 },
    { rule: "tools.ask_for", name: "tools.ask_for", line: 22 },
    { rule: "models.deny", name: "models.deny", line: 8 },
  ];
  return (
    <div className="rule-strip">
      <h3>How each rule played out · last 7 days</h3>
      <p className="sub">Click a rule to see only the runs it touched.</p>
      <div className="rule-list">
        {rows.map(r => {
          const t = tally[r.rule] || {};
          const total = (t.allow||0) + (t.warn||0) + (t.deny||0) + (t.ask||0);
          const pct = (k) => total ? Math.max(2, Math.round((t[k]||0)/total*100)) : 0;
          const fired = total > 0;
          const status = t.deny ? "deny" : t.warn ? "warn" : (t.ask ? "" : "");
          return (
            <div
              key={r.rule}
              className={`rule-row ${status} ${ruleFocus === r.rule ? "active" : ""}`}
              onClick={() => onPick(r.rule)}
            >
              <span className="dot" style={fired ? undefined : { background: "var(--ink-faint)" }}></span>
              <span className="name">{r.name} <span className="loc">L{r.line}</span></span>
              <span className="bar">
                {fired ? (
                  <>
                    {t.allow > 0 && <span className="ok" style={{ width: pct("allow") + "%" }}></span>}
                    {t.warn > 0 && <span className="warn" style={{ width: pct("warn") + "%" }}></span>}
                    {t.ask > 0 && <span className="ask" style={{ width: pct("ask") + "%" }}></span>}
                    {t.deny > 0 && <span className="deny" style={{ width: pct("deny") + "%" }}></span>}
                  </>
                ) : null}
              </span>
              <span className="counts">
                {fired ? (
                  <>
                    {t.allow ? <><b>{t.allow}</b> allow </> : null}
                    {t.warn ? <><span className="" style={{color:"var(--warn)"}}>{t.warn}</span> warn </> : null}
                    {t.ask ? <><span style={{color:"var(--accent)"}}>{t.ask}</span> ask </> : null}
                    {t.deny ? <><span style={{color:"var(--deny)"}}>{t.deny}</span> deny</> : null}
                  </>
                ) : <span style={{ color: "var(--ink-faint)" }}>never fired</span>}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

window.PolicySurface = PolicySurface;
