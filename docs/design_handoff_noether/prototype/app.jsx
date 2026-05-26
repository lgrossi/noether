/* global React, ReactDOM */
const { useReducer, useState, useEffect, useMemo } = React;

// ---------- reducer ----------
function clonePolicy(p) { return JSON.parse(JSON.stringify(p)); }

function initialState() {
  const D = window.NOETHER_DATA;
  const E = window.NOETHER_EVAL;
  const policy = clonePolicy(D.DEFAULT_POLICY);
  const proposed = clonePolicy(D.DEFAULT_POLICY);
  const runs = D.RUNS;
  const evalAll = E.evaluateAll(runs, policy);
  const evalCurrent = evalAll;
  const evalProposed = E.evaluateAll(runs, proposed);
  return {
    mode: "policy",
    policy,
    proposed,
    runs,
    evalAll,
    evalCurrent,
    evalProposed,
    dirty: false,
    appliedSuggestion: false,
    ruleFocus: null,
    projectFocus: null,
    agentFocus: null,
    decisionFocus: null,
    openRunId: null,
    openAskId: null,
    diffOpen: false,
    cmdkOpen: false,
    toast: null,
    now: window.NOETHER_NOW,
  };
}

function reducer(state, action) {
  const E = window.NOETHER_EVAL;
  switch (action.type) {
    case "go":
      return { ...state, mode: action.mode };
    case "patchPolicy": {
      const proposed = clonePolicy(state.proposed);
      const p = action.patch;
      if (p.kind === "cap_usd") proposed.budgets[0].cap_usd = p.value;
      else if (p.kind === "request_cost_usd") proposed.limits.request_cost_usd = p.value;
      else if (p.kind === "tools_per_turn") proposed.limits.tools_per_turn = p.value;
      else if (p.kind === "retries") proposed.limits.retries = p.value;
      const evalProposed = E.evaluateAll(state.runs, proposed);
      return {
        ...state,
        proposed,
        evalProposed,
        // Editor + tail + rule strip all visualize the *proposed* state so
        // edits feel live. The baseline `policy` is preserved so Replay can
        // show a real diff. dirty=true marks unsaved.
        evalAll: evalProposed,
        dirty: true,
      };
    }
    case "applySuggestion": {
      const proposed = clonePolicy(state.proposed);
      const s = action.suggestion;
      if (s.type === "cap_raise") proposed.budgets[0].cap_usd = s.to;
      else if (s.type === "retries_lower") proposed.limits.retries = s.to;
      const evalProposed = E.evaluateAll(state.runs, proposed);
      const next = {
        ...state,
        proposed,
        evalProposed,
        evalAll: evalProposed,
        dirty: true,
        appliedSuggestion: true,
        toast: { text: "Applied. Replay to see the impact.", action: { label: "Open Replay", mode: "replay" } },
      };
      if (action.thenGo) next.mode = action.thenGo;
      return next;
    }
    case "enforce": {
      // Adopt proposed (if any) and flip enforce on.
      const policy = clonePolicy(state.proposed);
      policy.enforced = true;
      const evalAll = E.evaluateAll(state.runs, policy);
      return {
        ...state,
        policy,
        proposed: clonePolicy(policy),
        evalAll,
        evalCurrent: evalAll,
        evalProposed: evalAll,
        dirty: false,
        toast: { text: "Policy enforced. Decisions now block.", tone: "ok" },
      };
    }
    case "unenforce": {
      const policy = clonePolicy(state.policy);
      policy.enforced = false;
      const proposed = clonePolicy(state.proposed);
      proposed.enforced = false;
      const evalAll = E.evaluateAll(state.runs, proposed);
      return {
        ...state,
        policy,
        proposed,
        evalAll,
        evalCurrent: E.evaluateAll(state.runs, policy),
        evalProposed: evalAll,
        toast: { text: "Back to dry-run.", tone: "warn" },
      };
    }
    case "adoptProposed": {
      const policy = clonePolicy(state.proposed);
      const evalAll = E.evaluateAll(state.runs, policy);
      return {
        ...state,
        policy,
        proposed: clonePolicy(policy),
        evalAll,
        evalCurrent: evalAll,
        evalProposed: evalAll,
        dirty: false,
        mode: "policy",
        toast: { text: "Adopted to policy. Still in dry-run.", action: { label: "Enforce now", do: "enforce" } },
      };
    }
    case "revertChanges": {
      const proposed = clonePolicy(state.policy);
      const evalProposed = E.evaluateAll(state.runs, proposed);
      return {
        ...state,
        proposed,
        evalProposed,
        evalAll: evalProposed,
        dirty: false,
        appliedSuggestion: false,
        toast: { text: "Reverted to saved policy.", tone: "warn" },
      };
    }
    case "ruleFocus":
      return { ...state, ruleFocus: action.rule };
    case "projectFocus":
      return { ...state, projectFocus: action.v };
    case "agentFocus":
      return { ...state, agentFocus: action.v };
    case "decisionFocus":
      return { ...state, decisionFocus: action.v };
    case "clearRunFilters":
      return { ...state, ruleFocus: null, projectFocus: null, agentFocus: null, decisionFocus: null };
    case "openRun": {
      const run = state.runs.find(r => r.id === action.id);
      if (run && run.status === "pending") return { ...state, openAskId: run.id };
      return { ...state, openRunId: action.id };
    }
    case "closeRun":
      return { ...state, openRunId: null };
    case "closeAsk":
      return { ...state, openAskId: null };
    case "decideAsk": {
      // mark the run resolved
      const runs = state.runs.map(r => r.id === action.id ? { ...r, status: "settled" } : r);
      let toast = { text: "Allowed. Agent can continue.", tone: "ok" };
      if (action.decision === "allow-rule") {
        toast = { text: "Allowed and rule updated.", action: { label: "View policy", mode: "policy" } };
      } else if (action.decision.startsWith("deny")) {
        toast = { text: "Denied.", tone: "warn" };
      }
      return { ...state, runs, openAskId: null, toast };
    }
    case "openDiff":
      return { ...state, diffOpen: true };
    case "closeDiff":
      return { ...state, diffOpen: false };
    case "openCmdK":
      return { ...state, cmdkOpen: true };
    case "closeCmdK":
      return { ...state, cmdkOpen: false };
    case "rerun":
      return { ...state, toast: { text: "Replay refreshed.", tone: "ok" } };
    case "clearToast":
      return { ...state, toast: null };
    default:
      return state;
  }
}

// ---------- root app ----------
function App() {
  const [state, dispatch] = useReducer(reducer, undefined, initialState);

  // expose dispatch for debug + headless screenshots
  React.useEffect(() => { window.__noet_dispatch = dispatch; }, [dispatch]);

  // auto-clear toast
  useEffect(() => {
    if (!state.toast) return;
    const t = setTimeout(() => dispatch({ type: "clearToast" }), state.toast.action ? 6000 : 2400);
    return () => clearTimeout(t);
  }, [state.toast]);

  // ⌘K + g-prefix nav
  useEffect(() => {
    let lastG = 0;
    const onKey = (e) => {
      // ignore typing in inputs
      const inField = ["INPUT", "TEXTAREA"].includes(document.activeElement?.tagName);
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        dispatch({ type: "openCmdK" });
        return;
      }
      if (inField) return;
      const t = Date.now();
      if (e.key === "g") { lastG = t; return; }
      if (t - lastG < 700) {
        if (e.key === "p") dispatch({ type: "go", mode: "policy" });
        if (e.key === "r") dispatch({ type: "go", mode: "runs" });
        if (e.key === "l") dispatch({ type: "go", mode: "replay" });
        lastG = 0;
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  return (
    <>
      <TopBar state={state} dispatch={dispatch} />
      <main>
        {state.mode === "policy" && <PolicySurface state={state} dispatch={dispatch} />}
        {state.mode === "runs" && <RunsSurface state={state} dispatch={dispatch} />}
        {state.mode === "replay" && <ReplaySurface state={state} dispatch={dispatch} />}
      </main>

      {state.openRunId && (
        <RunDetailModal
          run={state.runs.find(r => r.id === state.openRunId)}
          evaluation={state.evalAll.results.get(state.openRunId)}
          onClose={() => dispatch({ type: "closeRun" })}
          dispatch={dispatch}
        />
      )}
      {state.openAskId && (
        <AskModal
          run={state.runs.find(r => r.id === state.openAskId)}
          evaluation={state.evalAll.results.get(state.openAskId)}
          onClose={() => dispatch({ type: "closeAsk" })}
          dispatch={dispatch}
        />
      )}
      {state.diffOpen && (
        <DiffModal
          runs={state.runs}
          evalCurrent={state.evalCurrent}
          evalProposed={state.evalProposed}
          onClose={() => dispatch({ type: "closeDiff" })}
          dispatch={dispatch}
        />
      )}
      <CmdKPalette
        open={state.cmdkOpen}
        onClose={() => dispatch({ type: "closeCmdK" })}
        state={state}
        dispatch={dispatch}
      />

      {state.toast && (
        <div className="toast">
          {state.toast.text}
          {state.toast.action && (
            <>
              {" · "}
              <a onClick={() => {
                if (state.toast.action.mode) dispatch({ type: "go", mode: state.toast.action.mode });
                else if (state.toast.action.do === "enforce") dispatch({ type: "enforce" });
                dispatch({ type: "clearToast" });
              }}>{state.toast.action.label}</a>
            </>
          )}
        </div>
      )}
    </>
  );
}

// ---------- top bar ----------
function TopBar({ state, dispatch }) {
  return (
    <header className="topbar" data-screen-label="00 chrome">
      <NoetherLogo size={22} color="var(--ink)" accent="var(--accent)" />
      <span className="crumb mono">~/work/api <span className="branch">· main</span></span>

      <nav className="modes" role="tablist" aria-label="Surface">
        <button data-mode="policy" aria-current={state.mode === "policy"} onClick={() => dispatch({ type: "go", mode: "policy" })}>Policy</button>
        <button data-mode="runs" aria-current={state.mode === "runs"} onClick={() => dispatch({ type: "go", mode: "runs" })}>Runs</button>
        <button data-mode="replay" aria-current={state.mode === "replay"} onClick={() => dispatch({ type: "go", mode: "replay" })}>Replay</button>
      </nav>

      <div className="cmd" onClick={() => dispatch({ type: "openCmdK" })}>
        <span className="glyph">›</span>
        <span className="placeholder">ask Noether…</span>
        <span className="kbd">⌘K</span>
      </div>

      <span
        className={`enforce-wedge ${state.policy.enforced ? "on" : ""}`}
        onClick={() => dispatch({ type: state.policy.enforced ? "unenforce" : "enforce" })}
        title="Toggle dry-run / enforced"
      >
        <span className="pip"></span>
        {state.policy.enforced ? "enforced" : "dry-run"}
      </span>
    </header>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<App />);

// Expose a debug API for screenshot capture — opens specific UI states.
window.__noet_debug = {
  openRun(id) { window.__noet_dispatch?.({ type: "openRun", id }); },
  openCmdK() { window.__noet_dispatch?.({ type: "openCmdK" }); },
  openDiff() { window.__noet_dispatch?.({ type: "openDiff" }); },
  enforce() { window.__noet_dispatch?.({ type: "enforce" }); },
  go(mode) { window.__noet_dispatch?.({ type: "go", mode }); },
};
