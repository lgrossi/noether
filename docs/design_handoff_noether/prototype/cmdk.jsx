/* global React */

const CMDK_ITEMS = [
  { id: "policy",  icon: "›", label: "Open Policy", hint: "the rules file", kbd: "G P", do: (d) => d({ type: "go", mode: "policy" }) },
  { id: "runs",    icon: "›", label: "Open Runs", hint: "what actually happened", kbd: "G R", do: (d) => d({ type: "go", mode: "runs" }) },
  { id: "replay",  icon: "›", label: "Open Replay", hint: "compare strategies on real history", kbd: "G L", do: (d) => d({ type: "go", mode: "replay" }) },
  { id: "enforce", icon: "⇒", label: "Toggle enforce", hint: "dry-run ↔ enforced", kbd: "⇧E", do: (d, s) => d({ type: s.policy.enforced ? "unenforce" : "enforce" }) },
  { id: "ask-q1",  icon: "?", label: "Why did pi cost $58 yesterday?", hint: "ask · answer below", kind: "ask" },
  { id: "ask-q2",  icon: "?", label: "Spend by project, last 7d", hint: "ask · answer below", kind: "ask" },
  { id: "ask-q3",  icon: "?", label: "What would tighter caps cost me?", hint: "ask · answer below", kind: "ask" },
  { id: "ask-q4",  icon: "?", label: "Show pending asks", hint: "ask · answer below", kind: "ask" },
];

function CmdKPalette({ open, onClose, state, dispatch }) {
  const [q, setQ] = React.useState("");
  const [sel, setSel] = React.useState(0);
  const [answer, setAnswer] = React.useState(null);
  const inputRef = React.useRef(null);

  React.useEffect(() => {
    if (open) {
      setQ(""); setSel(0); setAnswer(null);
      setTimeout(() => inputRef.current?.focus(), 30);
    }
  }, [open]);

  React.useEffect(() => {
    if (!open) return;
    const onKey = (e) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowDown") { e.preventDefault(); setSel(s => Math.min(items.length - 1, s + 1)); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setSel(s => Math.max(0, s - 1)); }
      else if (e.key === "Enter") { e.preventDefault(); runItem(items[sel]); }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  });

  const items = React.useMemo(() => {
    const all = CMDK_ITEMS;
    if (!q.trim()) return all;
    const lq = q.toLowerCase();
    return all.filter(i => i.label.toLowerCase().includes(lq) || (i.hint || "").includes(lq));
  }, [q]);

  React.useEffect(() => { setSel(0); }, [items.length]);

  function runItem(item) {
    if (!item) return;
    if (item.kind === "ask") {
      setAnswer(answerFor(item.id, state));
    } else if (item.do) {
      item.do(dispatch, state);
      onClose();
    }
  }

  if (!open) return null;

  return (
    <div className="cmdk-scrim" onClick={onClose}>
      <div className="cmdk" onClick={(e) => e.stopPropagation()}>
        <input
          ref={inputRef}
          placeholder="Ask Noether or jump to a surface…"
          value={q}
          onChange={(e) => { setQ(e.target.value); setAnswer(null); }}
        />
        <div className="results">
          {items.map((it, i) => (
            <div
              key={it.id}
              className={`result ${i === sel ? "sel" : ""}`}
              onMouseEnter={() => setSel(i)}
              onClick={() => runItem(it)}
            >
              <span className="icon">{it.icon}</span>
              <span>
                <div className="label">{it.label}</div>
                <div className="hint">{it.hint}</div>
              </span>
              {it.kbd && <span className="kbd">{it.kbd}</span>}
            </div>
          ))}
        </div>
        {answer && (
          <div className="cmdk-answer">
            {answer}
            <div className="small">Press <span className="mono">esc</span> to close.</div>
          </div>
        )}
      </div>
    </div>
  );
}

function answerFor(id, state) {
  const { fmt$ } = window.Noet;
  const { runs, evalAll, policy } = state;

  if (id === "ask-q1") {
    const big = runs.find(r => r.id === "req-1a49");
    if (!big) return <>No runaway run found.</>;
    return (
      <>
        <b>{big.id}</b> ran on <b>{big.project}</b> at {big.dt.toLocaleString()}.
        It hit {big.retries} retries, prompt grew {(big.tokens / 1e6).toFixed(1)}M tokens, and the run cost {fmt$(big.cost)}.
        The rule that <b>would have caught it sooner</b> is{" "}
        <span className="mono">limits.retries</span> — lower it to 2 and re-run in Replay.
      </>
    );
  }
  if (id === "ask-q2") {
    const byProj = {};
    for (const r of runs) byProj[r.project] = (byProj[r.project] || 0) + r.cost;
    const sorted = Object.entries(byProj).sort((a, b) => b[1] - a[1]);
    return (
      <div>
        {sorted.map(([p, v]) => (
          <div key={p} style={{ display: "grid", gridTemplateColumns: "90px 1fr 70px", gap: 8, alignItems: "center", padding: "3px 0" }}>
            <span className="mono" style={{ fontSize: 12 }}>{p}</span>
            <span style={{ height: 8, borderRadius: 3, background: "var(--rule)", overflow: "hidden" }}>
              <span style={{ display: "block", height: "100%", width: (v / sorted[0][1] * 100) + "%", background: "var(--warn)" }}></span>
            </span>
            <span className="mono" style={{ fontSize: 12, textAlign: "right" }}>{fmt$(v)}</span>
          </div>
        ))}
        <div style={{ borderTop: "1px solid var(--rule)", marginTop: 6, paddingTop: 6 }} className="mono">
          total {fmt$(sorted.reduce((a, [, v]) => a + v, 0))}
        </div>
      </div>
    );
  }
  if (id === "ask-q3") {
    // synthetic projection: stricter caps would save approx prevented value
    const saved = evalAll.totals.prevented * 0.6;
    return (
      <>
        Running a stricter <span className="mono">cap_usd = $35</span> &amp; <span className="mono">retries = 2</span> against last 7d would have:
        <div style={{ marginTop: 6 }}>
          <div>· saved <b>{fmt$(saved)}</b> in blocked spend</div>
          <div>· added <b>{Math.round(evalAll.totals.warned * 0.4)}</b> warnings to runs near the cap</div>
          <div>· asked <b>{evalAll.totals.asked + 4}</b> times (today: {evalAll.totals.asked})</div>
        </div>
        <div className="small">Open <span className="mono">Replay</span> to try it for real.</div>
      </>
    );
  }
  if (id === "ask-q4") {
    const pending = runs.filter(r => r.status === "pending");
    if (pending.length === 0) return <>No pending asks — all clear.</>;
    return (
      <>
        {pending.length} pending: <b>{pending[0].id}</b> · {pending[0].agent} on {pending[0].project} — {pending[0].purpose}
      </>
    );
  }
  return null;
}

window.CmdKPalette = CmdKPalette;
