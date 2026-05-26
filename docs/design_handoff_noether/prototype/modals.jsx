/* global React */

function Scrim({ onClose, children }) {
  React.useEffect(() => {
    const onKey = (e) => { if (e.key === "Escape") onClose(); };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="scrim" onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      {children}
    </div>
  );
}

// ---------- RUN DETAIL ----------
function RunDetailModal({ run, evaluation, onClose, dispatch }) {
  const { fmt$, fmtClock, fmtDay, DECISION_GLYPHS, DECISION_CLASSES } = window.Noet;
  if (!run) return null;
  const e = evaluation;

  // synthesize a timeline (4 segments)
  const segs = [
    { t: 0, w: 18, cost: run.cost * 0.06, color: "var(--ok)", label: "init" },
    { t: 1, w: 32, cost: run.cost * 0.22, color: "var(--ok)", label: "search" },
    { t: 2, w: 30, cost: run.cost * 0.32, color: run.retries > 2 ? "var(--warn)" : "var(--ok)", label: "tool loop" },
    { t: 3, w: 20, cost: run.cost * 0.40, color: e.decision === "deny" ? "var(--deny)" : "var(--ok)", label: e.decision === "deny" ? "blocked" : "complete" },
  ];

  return (
    <Scrim onClose={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div>
            <div style={{ display: "flex", alignItems: "baseline", gap: 10 }}>
              <h2>{run.purpose}</h2>
              <span className={`pill ${DECISION_CLASSES[e.decision] === "ok" ? "allow" : DECISION_CLASSES[e.decision]}`}>
                <span className="ball"></span>{e.decision}
              </span>
            </div>
            <div className="meta" style={{ marginTop: 6 }}>
              <span className="mono" style={{ marginRight: 8 }}>{run.id}</span>
              · {run.agent} · {run.project} · {run.model} · {fmtDay(run.dt)} {fmtClock(run.dt)}
            </div>
          </div>
          <button className="close" onClick={onClose} aria-label="close">×</button>
        </div>

        <div className="modal-body">
          <div className="rd-stats">
            <div className="rd-stat">
              <div className="k">cost</div>
              <div className={`v ${e.decision === "deny" ? "bad" : ""}`}>{fmt$(run.cost)}</div>
            </div>
            <div className="rd-stat">
              <div className="k">tokens</div>
              <div className="v">{(run.tokens / 1e6).toFixed(1)}M</div>
            </div>
            <div className="rd-stat">
              <div className="k">tool calls</div>
              <div className={`v ${run.tools > 1000 ? "warn" : ""}`}>{run.tools}</div>
            </div>
            <div className="rd-stat">
              <div className="k">retries</div>
              <div className={`v ${run.retries > 3 ? "bad" : run.retries > 2 ? "warn" : ""}`}>{run.retries}</div>
            </div>
          </div>

          {e.rule && (
            <div style={{
              padding: "12px 14px",
              background: e.decision === "deny" ? "var(--deny-bg)" : "var(--warn-bg)",
              borderLeft: `3px solid var(--${e.decision === "deny" ? "deny" : "warn"})`,
              borderRadius: "0 6px 6px 0",
              marginBottom: 16,
            }}>
              <div className="mono" style={{ fontSize: 10.5, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--ink-faint)", marginBottom: 4 }}>
                rule fired
              </div>
              <div>
                <span className="mono" style={{ fontSize: 13, fontWeight: 600 }}>{e.rule}</span>
                <span style={{ color: "var(--ink-soft)", fontSize: 13, marginLeft: 8 }}>— {e.reason}</span>
              </div>
            </div>
          )}

          <div className="eyebrow" style={{ marginBottom: 8 }}>Timeline</div>
          <div className="rd-timeline">
            {segs.map((s, i) => {
              const t = new Date(run.dt.getTime() + i * 15000);
              return (
                <div className="rd-row" key={i}>
                  <span>{fmtClock(t)}:{String(t.getSeconds()).padStart(2,"0")}</span>
                  <span className="bar">
                    <span style={{ left: segs.slice(0, i).reduce((a, x) => a + x.w, 0) + "%", width: s.w + "%", background: s.color }}></span>
                  </span>
                  <span style={{ textAlign: "right" }}>{fmt$(s.cost)}</span>
                </div>
              );
            })}
            <div style={{ borderTop: "1px dashed var(--rule-2)", marginTop: 8, paddingTop: 8, fontSize: 12, color: "var(--ink-soft)" }}>
              {run.toolCalls && run.toolCalls.slice(0, 6).map((t, i) => (
                <span key={i} className="mono" style={{
                  display: "inline-block",
                  padding: "2px 8px",
                  background: "var(--surface)",
                  border: "1px solid var(--rule)",
                  borderRadius: 12,
                  marginRight: 6,
                  fontSize: 11,
                }}>{t}</span>
              ))}
            </div>
          </div>
        </div>

        <div className="modal-foot">
          {e.rule && (
            <button className="btn" onClick={() => { dispatch({ type: "ruleFocus", rule: e.rule }); dispatch({ type: "go", mode: "policy" }); onClose(); }}>
              Open rule in Policy
            </button>
          )}
          <button className="btn">Attribute differently</button>
          <span className="grow" />
          {e.decision === "deny" && (
            <button className="btn" onClick={() => { dispatch({ type: "go", mode: "replay" }); onClose(); }}>
              Replay with looser rule
            </button>
          )}
          <button className="btn primary" onClick={onClose}>Close</button>
        </div>
      </div>
    </Scrim>
  );
}

// ---------- ASK MODAL ----------
function AskModal({ run, evaluation, onClose, dispatch }) {
  const { fmtDay, fmtClock } = window.Noet;
  if (!run) return null;
  const cmd = run.toolCalls.find(t => t.startsWith("shell.") || t.startsWith("fs.") || t.startsWith("net.")) || run.toolCalls[0];

  return (
    <Scrim onClose={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 620 }}>
        <div className="modal-head">
          <div>
            <h2>Approve this action?</h2>
            <div className="meta" style={{ marginTop: 6 }}>
              <span className="mono" style={{ marginRight: 8 }}>{run.id}</span>
              · {run.agent} on <b>{run.project}</b> · {fmtClock(run.dt)}
            </div>
          </div>
          <button className="close" onClick={onClose}>×</button>
        </div>

        <div className="modal-body ask-mod">
          <div className="command">
            <span className="prompt">›</span>
            <span>shell.exec("rm -rf node_modules &amp;&amp; pnpm i")</span>
          </div>

          <div className="why">
            <b>The agent asked itself first:</b> "I want to reinstall node_modules because the build failed
            with a missing dependency. I judged this destructive enough to ask before running."
          </div>

          <div className="grid">
            <span className="k">rule fired</span><span className="v">tools.ask_for (L22)</span>
            <span className="k">step</span><span className="v">18 / 20</span>
            <span className="k">retries so far</span><span className="v">{run.retries}</span>
            <span className="k">prior asks</span><span className="v">2 this hour · both allowed</span>
            <span className="k">spend so far</span><span className="v">${run.cost.toFixed(2)}</span>
            <span className="k">model</span><span className="v">{run.model}</span>
          </div>

          <div className="actions">
            <button className="btn" style={{ borderColor: "var(--ok)", color: "var(--ok)" }} onClick={() => { dispatch({ type: "decideAsk", id: run.id, decision: "allow" }); onClose(); }}>
              Allow once
            </button>
            <button className="btn" onClick={() => { dispatch({ type: "decideAsk", id: run.id, decision: "allow-rule" }); onClose(); }}>
              Allow shell.* on incident
            </button>
            <button className="btn" style={{ borderColor: "var(--deny)", color: "var(--deny)" }} onClick={() => { dispatch({ type: "decideAsk", id: run.id, decision: "deny-continue" }); onClose(); }}>
              Deny &amp; let agent continue
            </button>
            <button className="btn" onClick={() => { dispatch({ type: "decideAsk", id: run.id, decision: "deny-stop" }); onClose(); }}>
              Deny &amp; stop run
            </button>
          </div>
        </div>
      </div>
    </Scrim>
  );
}

// ---------- DIFF INSPECTOR ----------
function DiffModal({ runs, evalCurrent, evalProposed, onClose, dispatch }) {
  const diffs = React.useMemo(() => {
    const out = [];
    for (const r of runs) {
      const a = evalCurrent.results.get(r.id);
      const b = evalProposed.results.get(r.id);
      if (a.decision !== b.decision || a.rule !== b.rule) {
        out.push({ run: r, from: a, to: b });
      }
    }
    return out;
  }, [runs, evalCurrent, evalProposed]);

  const { fmt$ } = window.Noet;

  return (
    <Scrim onClose={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div>
            <h2>What changes</h2>
            <div className="meta" style={{ marginTop: 6 }}>
              {diffs.length} run{diffs.length === 1 ? "" : "s"} would have had a different decision
            </div>
          </div>
          <button className="close" onClick={onClose}>×</button>
        </div>
        <div className="modal-body">
          {diffs.length === 0 ? (
            <p style={{ color: "var(--ink-faint)", textAlign: "center", padding: 24 }}>
              No decisions would change.
            </p>
          ) : (
            <div style={{ display: "grid", gap: 8 }}>
              {diffs.map(({ run, from, to }) => (
                <div key={run.id} style={{
                  border: "1px solid var(--rule)",
                  borderRadius: 8,
                  padding: "10px 14px",
                  display: "grid",
                  gridTemplateColumns: "1fr auto auto auto",
                  gap: 14,
                  alignItems: "baseline",
                }}>
                  <div>
                    <b className="mono" style={{ fontSize: 12 }}>{run.id}</b>
                    {" — "}
                    <span style={{ color: "var(--ink-2)" }}>{run.agent} · {run.purpose.slice(0, 40)}{run.purpose.length > 40 ? "…" : ""}</span>
                  </div>
                  <span className={`pill ${from.decision === "allow" ? "allow" : from.decision}`}><span className="ball"></span>{from.decision}</span>
                  <span style={{ color: "var(--ink-faint)" }}>→</span>
                  <span className={`pill ${to.decision === "allow" ? "allow" : to.decision}`}><span className="ball"></span>{to.decision}</span>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="modal-foot">
          <span className="grow" />
          <button className="btn" onClick={onClose}>Close</button>
          <button className="btn primary" onClick={() => { dispatch({ type: "adoptProposed" }); onClose(); }}>Adopt proposed</button>
        </div>
      </div>
    </Scrim>
  );
}

window.RunDetailModal = RunDetailModal;
window.AskModal = AskModal;
window.DiffModal = DiffModal;
