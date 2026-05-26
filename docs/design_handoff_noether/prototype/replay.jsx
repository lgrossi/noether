/* global React */

function ReplaySurface({ state, dispatch }) {
  const { runs, policy, proposed, evalCurrent, evalProposed, now } = state;
  const { fmt$ } = window.Noet;

  const isDirty = state.dirty;
  const recommendedWin = evalProposed.totals.totalSpend + evalProposed.totals.prevented * 0.4
                       < evalCurrent.totals.totalSpend + evalCurrent.totals.prevented * 0.4;

  // diff rows: runs whose decision changed between current and proposed
  const diffs = React.useMemo(() => {
    const out = [];
    for (const r of runs) {
      const a = evalCurrent.results.get(r.id);
      const b = evalProposed.results.get(r.id);
      if (a.decision !== b.decision || a.rule !== b.rule) {
        out.push({ run: r, from: a, to: b });
      }
    }
    return out.slice(0, 6);
  }, [runs, evalCurrent, evalProposed]);

  const diffSummary = React.useMemo(() => {
    return [
      { k: "cap_usd", from: policy.budgets[0].cap_usd, to: proposed.budgets[0].cap_usd, label: "budget cap" },
      { k: "request_cost_usd", from: policy.limits.request_cost_usd, to: proposed.limits.request_cost_usd, label: "per-request cost" },
      { k: "tools_per_turn", from: policy.limits.tools_per_turn, to: proposed.limits.tools_per_turn, label: "tools/turn" },
      { k: "retries", from: policy.limits.retries, to: proposed.limits.retries, label: "retries" },
    ].filter(d => d.from !== d.to);
  }, [policy, proposed]);

  const noChanges = diffSummary.length === 0;

  return (
    <section className="surface active" data-screen-label="03 Replay">
      <div className="page-head">
        <div>
          <div className="eyebrow">replay</div>
          <h1>What would change.</h1>
          <p className="lede">
            Run the proposed policy against your real history (or a synthetic scenario) before flipping enforce.
          </p>
        </div>
        <div className="right">
          <div className="big">last 7 days</div>
          <div className="sub">{runs.length} real runs · {fmt$(evalCurrent.totals.totalSpend)} baseline</div>
        </div>
      </div>

      <div className="replay-head">
        <div className="pickers">
          <div className="picker"><span className="k">against</span><span className="v">real · last 7d</span></div>
          <div className="picker"><span className="k">strategies</span><span className="v">current · proposed</span></div>
          <div className="picker"><span className="k">scope</span><span className="v">all projects</span></div>
          <button className="btn ghost">+ add strategy</button>
          <button className="btn">Change scenario</button>
        </div>
        <div>
          <button className="btn primary" onClick={() => dispatch({ type: "rerun" })}>Re-run</button>
        </div>
      </div>

      {noChanges ? (
        <div style={{
          background: "var(--surface)", border: "1px dashed var(--rule-2)", borderRadius: 10,
          padding: 28, textAlign: "center", marginBottom: 18,
        }}>
          <div style={{ fontFamily: "Newsreader, serif", fontStyle: "italic", fontSize: 26, color: "var(--ink-2)" }}>
            No proposed changes yet.
          </div>
          <p style={{ color: "var(--ink-faint)", marginTop: 8 }}>
            Edit a rule in <a onClick={() => dispatch({ type: "go", mode: "policy" })} style={{ cursor: "pointer", color: "var(--accent)", textDecoration: "underline" }}>Policy</a> or apply the suggestion to see what would change.
          </p>
        </div>
      ) : (
        <>
          <div className="scenarios">
            <ScenarioCard
              title="current"
              tag="policy @ HEAD"
              ribbon={<span className="pill"><span className="ball" style={{ background: "var(--ink-faint)" }}></span>baseline</span>}
              desc={`cap_usd $${policy.budgets[0].cap_usd} · request_cost_usd $${policy.limits.request_cost_usd} · tools_per_turn ${policy.limits.tools_per_turn}`}
              stats={evalCurrent.totals}
              policy={policy}
            />
            <ScenarioCard
              title="proposed"
              tag={diffSummary.map(d => `${d.k.replace(/_/g, " ")} ${d.from}→${d.to}`).join(" · ")}
              ribbon={recommendedWin ? <span className="pill allow"><span className="ball"></span>recommended</span> : <span className="pill warn"><span className="ball"></span>trade-offs</span>}
              desc="Lifts the cap, tightens retries, narrows shell-ish tools to ask."
              stats={evalProposed.totals}
              policy={proposed}
              isProposed
              vs={evalCurrent.totals}
              diffSummary={diffSummary}
              winner={recommendedWin}
            />
          </div>

          <div className="reco">
            <div>
              <div className="verdict">
                {recommendedWin ? <>Proposed <b>wins</b> on spend and prevents more runaway cost.</> : <>Proposed has <b>trade-offs</b>.</>}
              </div>
              <div className="sub">
                {diffs.length} run{diffs.length === 1 ? "" : "s"} would have had a different decision.
                Inspect them below before adopting.
              </div>
            </div>
            <div className="right">
              <button className="btn" onClick={() => dispatch({ type: "openDiff" })}>Inspect diffs</button>
              <button className="btn primary" onClick={() => dispatch({ type: "adoptProposed" })}>Adopt &amp; save →</button>
            </div>
          </div>

          <div className="scenario-row">
            {diffs.map(({ run, from, to }) => (
              <DiffRow key={run.id} run={run} from={from} to={to} />
            ))}
          </div>

          <div className="cross">
            <span className="label">next</span>
            <span className="grow">
              Adopting writes {diffSummary.length} line{diffSummary.length === 1 ? "" : "s"} to <span className="mono" style={{ fontSize: 12 }}>policy.noet.yaml</span> and stays in dry-run.
            </span>
            <button className="btn" onClick={() => dispatch({ type: "go", mode: "policy" })}>→ Open the diff in Policy</button>
            <button className="btn primary" onClick={() => dispatch({ type: "adoptProposed" })}>Adopt</button>
          </div>
        </>
      )}
    </section>
  );
}

function ScenarioCard({ title, tag, ribbon, desc, stats, policy, isProposed, vs, diffSummary, winner }) {
  const { fmt$ } = window.Noet;
  const diff = (a, b) => {
    if (a == null || b == null) return null;
    const d = a - b;
    if (Math.abs(d) < 0.005) return null;
    return d > 0 ? `+${d.toFixed(d > 1 ? 0 : 2)}` : `${d.toFixed(d < -1 ? 0 : 2)}`;
  };
  const $diff = (a, b) => {
    if (vs == null) return null;
    const d = a - b;
    if (Math.abs(d) < 0.005) return null;
    return d > 0 ? `+${fmt$(d)}` : `−${fmt$(-d)}`;
  };
  return (
    <div className={`scenario ${winner ? "winner" : ""}`}>
      <div className="h">
        <span className="name">{title}</span>
        <span className="tag">{tag}</span>
        <span className="ribbon">{ribbon}</span>
      </div>
      <p className="desc">{desc}</p>

      {isProposed && diffSummary && diffSummary.length > 0 && (
        <div className="diff">
          {diffSummary.map(d => (
            <React.Fragment key={d.k}>
              <span className="minus">- {d.k}: {d.from}</span>
              <span className="plus">+ {d.k}: {d.to}</span>
            </React.Fragment>
          ))}
        </div>
      )}

      <div className="stats">
        <div>
          <div className="k">spent</div>
          <div className={`v ${vs && stats.totalSpend < vs.totalSpend ? "ok" : (vs && stats.totalSpend > vs.totalSpend ? "bad" : "")}`}>
            {fmt$(stats.totalSpend)}
            {vs && $diff(stats.totalSpend, vs.totalSpend) && <span className="delta">{$diff(stats.totalSpend, vs.totalSpend)}</span>}
          </div>
        </div>
        <div>
          <div className="k">prevented</div>
          <div className={`v ${vs && stats.prevented > vs.prevented ? "ok" : ""}`}>
            {fmt$(stats.prevented)}
            {vs && $diff(stats.prevented, vs.prevented) && <span className="delta">{$diff(stats.prevented, vs.prevented)}</span>}
          </div>
        </div>
        <div>
          <div className="k">denied</div>
          <div className="v">
            {stats.denied}
            {vs && diff(stats.denied, vs.denied) && <span className="delta">{diff(stats.denied, vs.denied)}</span>}
          </div>
        </div>
        <div>
          <div className="k">asked</div>
          <div className={`v ${vs && stats.asked > vs.asked ? "bad" : ""}`}>
            {stats.asked}
            {vs && diff(stats.asked, vs.asked) && <span className="delta">{diff(stats.asked, vs.asked)}</span>}
          </div>
        </div>
        <div>
          <div className="k">cap headroom</div>
          <div className={`v ${policy.budgets[0].cap_usd - stats.spend30d > 5 ? "ok" : (policy.budgets[0].cap_usd - stats.spend30d <= 0 ? "bad" : "")}`}>
            {fmt$(Math.max(0, policy.budgets[0].cap_usd - stats.spend30d))}
          </div>
        </div>
        <div>
          <div className="k">runaway exposure</div>
          <div className={`v ${(stats.prevented > 50) ? "ok" : "bad"}`}>{stats.prevented > 50 ? "low" : stats.prevented > 20 ? "med" : "high"}</div>
        </div>
      </div>
    </div>
  );
}

function DiffRow({ run, from, to }) {
  const { fmt$, DECISION_CLASSES } = window.Noet;
  const change =
    from.decision === "deny" && to.decision !== "deny" ? "recovered" :
    to.decision === "deny" && from.decision !== "deny" ? "newly denied" :
    from.decision === "ask" && to.decision !== "ask" ? "auto-handled" :
    to.decision === "ask" && from.decision !== "ask" ? "+1 ask" :
    `${from.decision} → ${to.decision}`;
  const tone = change === "recovered" ? "allow" : change.includes("ask") ? "ask" : change.includes("denied") ? "deny" : "warn";

  return (
    <div className="row-card">
      <div className="label">
        <b>{run.id} · {run.agent} · {run.purpose.slice(0, 38)}{run.purpose.length > 38 ? "…" : ""}</b>
        {run.dt.toLocaleDateString("en-US", { month: "short", day: "numeric" })} · {fmt$(run.cost)}
      </div>
      <div className="bar-cmp">
        <div className="row">
          <span className="l">cur</span>
          <span className="seg">
            <span style={{ width: from.decision === "deny" ? "100%" : from.decision === "warn" ? "60%" : from.decision === "ask" ? "70%" : "92%",
              background: `var(--${from.decision === "allow" ? "ok" : from.decision === "deny" ? "deny" : from.decision === "warn" ? "warn" : "accent"})` }}></span>
          </span>
          <span className="n">{from.decision}</span>
        </div>
        <div className="row">
          <span className="l">new</span>
          <span className="seg">
            <span style={{ width: to.decision === "deny" ? "100%" : to.decision === "warn" ? "60%" : to.decision === "ask" ? "70%" : "92%",
              background: `var(--${to.decision === "allow" ? "ok" : to.decision === "deny" ? "deny" : to.decision === "warn" ? "warn" : "accent"})` }}></span>
          </span>
          <span className="n">{to.decision}</span>
        </div>
      </div>
      <span className={`pill ${tone}`}><span className="ball"></span>{change}</span>
    </div>
  );
}

window.ReplaySurface = ReplaySurface;
