/* global React */
const { useMemo: useMemoR } = React;

function RunsSurface({ state, dispatch }) {
  const { runs, evalAll, ruleFocus, projectFocus, agentFocus, decisionFocus, now } = state;
  const { fmt$, fmtClock, fmtDay, dayKey, DECISION_GLYPHS, DECISION_CLASSES } = window.Noet;

  const filtered = useMemoR(() => {
    return runs.filter(r => {
      const e = evalAll.results.get(r.id);
      if (ruleFocus && e.rule !== ruleFocus) return false;
      if (projectFocus && r.project !== projectFocus) return false;
      if (agentFocus && r.agent !== agentFocus) return false;
      if (decisionFocus && e.decision !== decisionFocus) return false;
      return true;
    });
  }, [runs, evalAll, ruleFocus, projectFocus, agentFocus, decisionFocus]);

  const totals = useMemoR(() => {
    let spent = 0, n = filtered.length, hits = 0;
    for (const r of filtered) {
      const e = evalAll.results.get(r.id);
      if (e.decision !== "deny" && e.decision !== "ask") spent += r.cost;
      if (e.decision === "deny") hits++;
    }
    return { spent, n, hits };
  }, [filtered, evalAll]);

  // group by day
  const days = useMemoR(() => {
    const m = new Map();
    for (const r of filtered) {
      const k = dayKey(r.dt);
      if (!m.has(k)) m.set(k, []);
      m.get(k).push(r);
    }
    return Array.from(m.entries());
  }, [filtered]);

  const chips = [
    { k: "project", v: projectFocus, label: "project", action: () => dispatch({ type: "projectFocus", v: null }) },
    { k: "agent", v: agentFocus, label: "agent", action: () => dispatch({ type: "agentFocus", v: null }) },
    { k: "decision", v: decisionFocus, label: "decision", action: () => dispatch({ type: "decisionFocus", v: null }) },
    { k: "rule", v: ruleFocus, label: "rule", action: () => dispatch({ type: "ruleFocus", rule: null }) },
  ];
  const hasFilter = chips.some(c => c.v);

  return (
    <section className="surface active" data-screen-label="02 Runs">
      <div className="page-head">
        <div>
          <div className="eyebrow">runs</div>
          <h1>What actually happened.</h1>
          <p className="lede">
            Every agent run, attributed and decided. Click a row to see the trace, or turn a finding into a policy edit.
          </p>
        </div>
        <div className="right">
          <div className="big">{fmt$(totals.spent)}</div>
          <div className="sub">{totals.n} runs · last 7 days · {totals.hits} limits hit</div>
        </div>
      </div>

      <div className="filterbar">
        <span className="mono" style={{ fontSize: 11, letterSpacing: "0.14em", textTransform: "uppercase", color: "var(--ink-faint)" }}>filter</span>

        {chips.map(c => (
          <span
            key={c.k}
            className="chip"
            style={c.v ? { background: "var(--accent-2)", borderColor: "rgba(194,65,12,0.2)", color: "var(--ink-2)" } : {}}
          >
            {c.label} <b>{c.v || "any"}</b>
            {c.v && <span className="x" onClick={c.action}>×</span>}
          </span>
        ))}
        {hasFilter && (
          <>
            <span className="sep">·</span>
            <a
              onClick={() => dispatch({ type: "clearRunFilters" })}
              style={{ color: "var(--ink-soft)", cursor: "pointer", textDecoration: "underline" }}
            >
              clear all
            </a>
          </>
        )}
        <span style={{ marginLeft: "auto", fontSize: 11, color: "var(--ink-faint)", fontFamily: '"Geist Mono", monospace' }}>/ to search</span>
      </div>

      <div className="runs-table">
        {days.map(([k, rows]) => {
          const total = rows.reduce((a, r) => a + r.cost, 0);
          return (
            <React.Fragment key={k}>
              <div className="runs-day">
                <span>{fmtDay(new Date(k))}</span>
                <span className="total">{fmt$(total)} · {rows.length} runs</span>
              </div>
              {rows.map(r => {
                const e = evalAll.results.get(r.id);
                const bg = e.decision === "deny" ? "rgba(139,31,31,0.04)" :
                           e.decision === "ask" ? "rgba(194,65,12,0.04)" : undefined;
                return (
                  <div
                    key={r.id}
                    className="run-row"
                    style={bg ? { background: bg } : {}}
                    onClick={() => dispatch({ type: "openRun", id: r.id })}
                  >
                    <span className="when">{fmtClock(r.dt)}</span>
                    <span className={`glyph ${DECISION_CLASSES[e.decision]}`}>{DECISION_GLYPHS[e.decision]}</span>
                    <span className="what">
                      <span className="agent">{r.agent}</span>
                      <span className="purpose">{r.purpose}</span>
                    </span>
                    <span className="meta">{r.project} · {r.tools >= 1000 ? (r.tools/1000).toFixed(1) + "k tools" : r.tools + " tools"}</span>
                    <span className="meta">{r.model}</span>
                    <span className={`cost ${DECISION_CLASSES[e.decision] === "ok" ? "" : DECISION_CLASSES[e.decision]}`}>
                      {e.decision === "deny" ? fmt$(r.cost) : e.decision === "ask" ? "—" : fmt$(r.cost)}
                    </span>
                  </div>
                );
              })}
            </React.Fragment>
          );
        })}

        {filtered.length === 0 && (
          <div style={{ padding: 40, textAlign: "center", color: "var(--ink-faint)" }}>
            No runs match this filter. <a onClick={() => dispatch({ type: "clearRunFilters" })} style={{ color: "var(--ink)", cursor: "pointer", textDecoration: "underline" }}>clear all</a>
          </div>
        )}

        <div className="runs-foot">
          <span><b style={{ color: "var(--ink)" }}>{totals.n}</b> runs · <b style={{ color: "var(--ink)" }}>{(filtered.reduce((a, r) => a + r.tokens, 0) / 1e6).toFixed(1)}M</b> tokens · {totals.hits} limits hit</span>
          <span className="grow" />
          <button className="btn">Export ledger (CSV)</button>
        </div>
      </div>

      {ruleFocus && (
        <div className="cross">
          <span className="label">next</span>
          <span className="grow">
            Filtering to <span className="mono" style={{ fontSize: 12 }}>{ruleFocus}</span>.
            Want to propose changing the rule that caused this?
          </span>
          <button className="btn" onClick={() => dispatch({ type: "go", mode: "policy" })}>→ Open in Policy</button>
          <button className="btn primary" onClick={() => dispatch({ type: "go", mode: "replay" })}>→ Replay with new rule</button>
        </div>
      )}
    </section>
  );
}

window.RunsSurface = RunsSurface;
