# Handoff: Noether — local-first agent-governance UI

This bundle is everything needed to implement Noether's interface in production.

---

## 1. What this is

Noether is a **local-first governance layer for agent work** — it observes
agent activity, attributes it, gates it via a small auditable policy file,
and lets users **simulate** policy changes against real history before
enforcing them.

The current product surface tries to be a dashboard of everything. This
handoff redefines it around **three first-class surfaces** and one core loop:

```
   Policy (home)        Runs               Replay
   ─────────────        ──────             ────────────
   the rules file   ⇄   what happened  ⇄   what would change
        │                                       │
        └──── edit a rule → simulate → adopt ───┘
```

**Policy is the home.** Not a generic dashboard. Not a request log. The
policy file is the artifact that gets shipped and the thing that gives
Noether a recognizable identity (LiteLLM and proxy/log products don't have
this center of gravity).

The other two surfaces exist **in service of** Policy:
- **Runs** is the evidence — every decision attributed to a rule.
- **Replay** is the rehearsal — what a proposed policy would have done.

---

## 2. About these files

The files in `prototype/` are **design references created in HTML/React**
(via `<script type="text/babel">` for fast iteration). They are not
production code to ship verbatim. The task is to **recreate the same
design and interaction model in Noether's real codebase**, using its
existing framework, style system, and state-management conventions.

If Noether currently has no UI codebase, pick the most appropriate stack
(React + Vite + Tailwind / CSS Modules is a reasonable default for what's
shown here) and recreate the surfaces.

---

## 3. Fidelity

**High-fidelity.** Colors, typography, spacing, and copy in `prototype/`
are final. Every visible number is computed from a fixture dataset, so
real-data wiring is also specified by `prototype/data.js` (see §9).
Interactions specified in §6 are the contract — replicate them.

---

## 4. Folder layout

```
design_handoff_noether/
├── README.md                  ← this file
├── prototype/                 ← live, runnable React prototype
│   ├── Noether.html             entry point — open in a browser
│   ├── app.jsx                  state + routing + key shortcuts
│   ├── data.js                  fixture dataset + policy evaluator
│   ├── logo.jsx                 brand mark + lockup
│   ├── policy.jsx               Policy surface
│   ├── runs.jsx                 Runs surface
│   ├── replay.jsx               Replay surface
│   ├── modals.jsx               Run detail · Ask · Diff inspector
│   ├── cmdk.jsx                 ⌘K command palette
│   ├── noether.css              tokens + base
│   ├── noether-surfaces.css     surface-specific layouts
│   └── noether-polish.css       modals, palette, animations
├── logo/
│   ├── noether-mark.svg         22×22 monogram
│   ├── noether-mark-on-dark.svg dark-bg variant
│   ├── noether-lockup.svg       mark + wordmark
│   └── favicon.svg              32×32 favicon w/ paper background
├── screenshots/                 reference images, see §10
└── explorations/                earlier directions (context only)
    ├── 01-wireframes-5-directions.html   five identity options
    └── 02-hifi-static.html               static hi-fi mock
```

To run the prototype, open `prototype/Noether.html` in a browser. No
build step. Data is generated client-side from a deterministic seed.

---

## 5. Surfaces

### 5.1 · Policy (home)

> *What's allowed here.*

**Purpose.** Show the live policy and how it's behaving right now. Edit
in place. Edits are dry-run until enforced.

**Layout** (12-col, max-width 1320px, gutters 24px):

```
┌────────────────────────────────────────────────────────────────┐
│  topbar  (sticky)                                              │
├────────────────────────────────────────────────────────────────┤
│  page-head                              7 rules                │
│  What's allowed here.                   dry-run · decisions    │
├────────────────────────────┬───────────────────────────────────┤
│  policy.noet.yaml          │  Decisions · live                 │
│  (editor, inline-editable) │  tail of last 24h                 │
│  ───── rule tally column ──┤  ⌁ suggestion card                │
│                            │                                   │
│  [observe|warn|enforce]    │                                   │
│  [Replay] [Enforce]        │                                   │
├────────────────────────────┴───────────────────────────────────┤
│  How each rule played out · last 7 days                        │
│  • models.allow         ▓▓▓▓▓▓▓▓░       2 081 allow ...        │
│  • cap_usd              ▓▓▓▓▓░░░░       0 deny (suggestion)    │
│  • request_cost_usd     ▓▓▓░░░░░░       2 warn                 │
│  • tools_per_turn       ▓▓░░░░░░░       24 warn                │
│  • retries              ▓░░░░░░░░       8 deny                 │
│  • tools.ask_for        ▓▓▓░░░░░░       9 ask                  │
├────────────────────────────────────────────────────────────────┤
│  next · 1 pending change · [Replay before enforcing] [Enforce] │
└────────────────────────────────────────────────────────────────┘
```

**Editor** (`prototype/policy.jsx` → `PolicyEditor`):
- Renders YAML as a 3-column grid `[gutter] [code] [tally]`.
- **Inline-editable values** for `cap_usd`, `request_cost_usd`,
  `tools_per_turn`, `retries`. Tab/click to focus, `Enter` to commit,
  `Esc` to cancel.
- Right-side **tally column** shows decision counts driven by the
  current policy: `0 allow`, `13 deny`, `8 deny`, etc. Counts update
  live as the user edits.
- Clicking a rule line **focuses** the tail to only show decisions
  that touched that rule.
- Lines that have any `deny` get a `.has-issue` class — the gutter
  number renders in accent orange.

**Live tail** (`prototype/policy.jsx` → tail card):
- Most recent 14 decisions, newest first. Animates a `slip` on insert.
- `12:04 ? pi · shell.exec("…") req-1a4b · ask_for           waiting`
- Click a row → opens **Run detail** or **Ask** modal depending on
  `status`.
- Summary footer: `312 in window · 298 allow · 11 warn · 2 deny · 1 ask`.

**Suggestion card** (`prototype/policy.jsx` → suggestion logic):
- Generated by Noether based on the current tally:
  - If `limits.retries` denies ≥ 2 → "Retries are doing real work…"
  - Else if `cap_usd` denies ≥ 1 → "Cap is too tight…"
  - Else: no card.
- Each suggestion has `Apply` (writes to proposed policy, marks dirty)
  and `Simulate first` (apply then navigate to Replay).

**Rule strip** (`prototype/policy.jsx` → `RuleStrip`):
- One row per rule. Bar shows allow/warn/deny/ask proportions. Click
  a row to jump to Runs filtered by that rule.

### 5.2 · Runs

> *What actually happened.*

**Purpose.** Browse every agent run, attributed and decided. Used for
investigation and to turn findings into policy edits.

**Layout:**
```
[ page-head: $148.32 · 114 runs · last 7 days · 4 limits hit ]
[ filter bar: chips for project, agent, decision, rule ]
[ runs-table:
    FRI MAY 22                              $16.68 · 8 runs
    12:04  ?  pi · shell.exec(…)              incident · 18 tools
    12:29  ●  claude-code · release notes     api · 24 tools          $0.61
    11:17  ✕  pi · rollback dry-run           incident · 8 tools      $8.88
    ...
]
[ footer: 114 runs · 53.8M tokens · 4 limits hit  [Export CSV] ]
[ cross-link: → Open in Policy / → Replay with new rule  (if filtered) ]
```

**Behavior:**
- Click any row → **Run detail modal** (or **Ask modal** if pending).
- Filter chips visually invert when active (orange-tinted background).
- "Clear all" link appears once any filter is set.
- Day headers separate days; day totals shown.
- Hovering a row reveals row actions (placeholder; design lives in CSS).

### 5.3 · Replay

> *What would change.*

**Purpose.** Compare the **current** policy to a **proposed** policy
against the same real 7 days. The recommendation is plain English; the
numbers back it up.

**Empty state.** When `policy === proposed`, show:
```
              No proposed changes yet.
   Edit a rule in Policy or apply the suggestion to see what would change.
```

**Diff state.**
```
[ page-head ]                                last 7 days
                                             101 real runs · $86.10 baseline
[ pickers: against | strategies | scope ]    [Re-run]

┌───────────────────────────────┬───────────────────────────────┐
│  current     policy @ HEAD    │  proposed     retries 3→2     │
│  cap_usd $100 · request $3    │  (winner ribbon)              │
│  — baseline                   │  Lifts the cap, tightens …    │
│                               │                               │
│  spent      $86.10            │  spent     $81.16  −$4.94     │
│  prevented  $30.10            │  prevented $35.04  +$4.94     │
│  denied     21                │  denied    25      +4         │
│  asked      9                 │  asked     9                  │
│  cap headroom $13.90          │  cap headroom $17.83          │
│  runaway exposure med         │  runaway exposure med         │
└───────────────────────────────┴───────────────────────────────┘

[ recommendation strip ]
Proposed wins on spend and prevents more runaway cost.
8 runs would have had a different decision. Inspect them below.
                                       [Inspect diffs] [Adopt & save →]

[ diff rows: per-run before/after bars ]

[ cross-link: → Open the diff in Policy / [Adopt] ]
```

**Behavior:**
- Both scenario cards drive their stats from `evaluateAll(runs, policy)`
  and `evaluateAll(runs, proposed)` in `data.js`.
- "Inspect diffs" → **Diff modal**.
- "Adopt & save" → `policy = proposed`, navigates back to Policy, shows
  toast `"Adopted to policy. Still in dry-run."` with action `Enforce now`.

---

## 6. Interactions & state

Every interaction below is wired in the prototype. Treat this as the spec.

### 6.1 · Editing a rule
1. User clicks a value in the editor (e.g. `cap_usd: "$100"`).
2. Inline `<input type="number">` appears.
3. On commit, dispatch `patchPolicy {kind: "cap_usd", value: 120}`.
4. State updates `state.proposed` (NOT `state.policy`).
5. `dirty = true`. Editor shows `● modified`. Footer cross-link shows
   `[Revert] [Replay diff] [Adopt & enforce]`.
6. Tail + rule strip recompute from `evalProposed`.
7. **Baseline is preserved** so Replay can diff against the saved policy.

### 6.2 · Applying a suggestion
- `Apply` → same as above plus `appliedSuggestion: true` (hides card)
  and toast `"Applied. Replay to see the impact."`.
- `Simulate first` → same, plus navigates to Replay.

### 6.3 · Enforcing
- `Enforce` button (top-bar wedge or footer button) → `policy = proposed`,
  `enforced: true`. Toast `"Policy enforced. Decisions now block."`.
- Top-bar wedge flips from accent-orange "dry-run" to green "enforced".

### 6.4 · Approval (Ask modal)
- Pending runs (`status: "pending"`) sit at the top of Runs.
- Clicking opens **Ask modal** with four canonical actions:
  - `Allow once`
  - `Allow shell.* on incident` *(turn this single allow into a rule)*
  - `Deny & let agent continue`
  - `Deny & stop run`
- Each dispatches `decideAsk {id, decision}`; the run is marked settled
  and the appropriate toast surfaces.

### 6.5 · Run detail
- Any non-pending row click. Modal shows: stats (cost, tokens, tool calls,
  retries), the rule that fired (colored callout), a synthetic timeline
  of cost over the run's segments, and tool tags.
- Footer: `Open rule in Policy` (deep-links to that rule line),
  `Replay with looser rule`, `Close`.

### 6.6 · Diff inspector
- From Replay → `Inspect diffs`. Lists every run whose decision changed,
  showing `before → after` pills (e.g. `deny → warn`).
- `Adopt proposed` from here also works.

### 6.7 · Command palette (⌘K)
- Keyboard: `⌘K` / `Ctrl+K` opens. `↑↓` to navigate, `Enter` to run,
  `Esc` to close.
- Items mix **navigation** (`Open Policy`, `Open Runs`, `Open Replay`,
  `Toggle enforce`) with **asks** (canned questions that produce inline
  answers reading real data: spend by project, why did pi cost $58
  yesterday, what would tighter caps cost me, show pending asks).
- Asks render an inline answer card below the input.

### 6.8 · Keyboard shortcuts
- `⌘K` / `Ctrl+K` → command palette
- `g p` → Policy · `g r` → Runs · `g l` → Replay
- `/` → focus filter (placeholder shown; not yet wired)
- `Esc` → close any open modal/palette

### 6.9 · Toasts
- Pill at bottom-center. Single-line text + optional inline action link.
- Auto-dismiss after 2.4s (6s if it has an action).
- Used for: enforced, reverted, suggestion applied, ask decided, adopted.

---

## 7. Design tokens

All defined in `prototype/noether.css` as `:root` custom properties.

### Color

| Token | Hex | Use |
|---|---|---|
| `--bg`        | `#f6f3ec` | Page background |
| `--paper`     | `#fbf9f3` | Surface variant (filter bars, footers, editor footer) |
| `--surface`   | `#ffffff` | Cards, editor body, modals |
| `--ink`       | `#14110d` | Primary text |
| `--ink-2`     | `#3a342b` | Secondary text |
| `--ink-soft`  | `#6b6558` | Tertiary text |
| `--ink-faint` | `#9a9485` | Hints, meta, eyebrows |
| `--rule`      | `#e6e0cf` | Default border |
| `--rule-2`    | `#d6cfb9` | Strong border |
| `--accent`    | `#c2410c` | Brand accent (orange) — ask, brand mark, dry-run |
| `--accent-2`  | `#f5e9d9` | Accent background tint |
| `--ok`        | `#15724a` | Allow, enforced, recovered |
| `--ok-bg`     | `#e6efe2` | Allow background tint |
| `--warn`      | `#a5611a` | Warn |
| `--warn-bg`   | `#f7ecd6` | Warn background tint |
| `--deny`      | `#8b1f1f` | Deny |
| `--deny-bg`   | `#f3e0dc` | Deny background tint |
| `--info`      | `#2a4d8f` | Info pill |
| `--info-bg`   | `#e3e9f3` | Info background tint |

### Typography

| Family | Use | Source |
|---|---|---|
| **Newsreader** italic 500 | Brand wordmark, page headings, modal titles, scenario names | Google Fonts |
| **Geist** 400/500/600 | UI body, labels, buttons, controls | Google Fonts |
| **Geist Mono** 400/500/600 | Code, IDs, numbers, eyebrows, kbd | Google Fonts |

Scale:
- Page H1: `40px / 1.0` Newsreader italic 400, tracking `-0.02em`
- Modal H2: `28px / 1.0` Newsreader italic 500, tracking `-0.02em`
- Scenario H4: `22px / 1.0` Newsreader italic
- Section eyebrow: `10.5px` Geist Mono, tracking `0.16em`, upper, `ink-faint`
- UI body: `14px / 1.5` Geist 400, tracking `-0.005em`
- Small/meta: `12px` Geist or Geist Mono
- Pills/badges: `11px` Geist Mono 500, tracking `0.02em`

### Spacing & shape

- Radii: `--radius: 10px`, `--radius-sm: 6px`, pills `999px`
- Modal radius: `14px`
- Shadows:
  - `--shadow`: `0 1px 2px rgba(20,17,13,0.04), 0 8px 24px -10px rgba(20,17,13,0.10)`
  - `--shadow-pop`: `0 4px 14px rgba(20,17,13,0.08), 0 28px 60px -20px rgba(20,17,13,0.28)` (modals, palette)

### Decision palette

The four decisions are **always** rendered with the same color + glyph:

| Decision | Glyph | Color | Pill class |
|---|---|---|---|
| allow | `●` | `--ok`     | `.pill.allow` |
| warn  | `▲` | `--warn`   | `.pill.warn`  |
| deny  | `✕` | `--deny`   | `.pill.deny`  |
| ask   | `?` | `--accent` | `.pill.ask`   |

---

## 8. Brand

### Logomark

22×22 viewBox. Conceptual reference: **Noether's theorem** — every continuous
symmetry corresponds to a conservation law.

- A thin, rotated square outline (the "rule")
- Intersected by a horizontal line (decisions flowing through the rule)
- An accent-orange dot at the intersection (the moment of decision)

The mark works at sizes from 16px (favicon) up.

### Wordmark

"noether" in **Newsreader italic 500**, letter-spacing `-0.02em`, sized
to match the mark's height (~`0.95em`).

Lockup spacing: 8px between mark and wordmark.

See `logo/noether-mark.svg`, `logo/noether-lockup.svg`,
`logo/noether-mark-on-dark.svg`, `logo/favicon.svg`.

### Voice

- **Quiet, sharp, technical.** Page headings are short clauses ending
  in a period: "What's allowed here." · "What actually happened." ·
  "What would change."
- **Mathematical**, not enterprise-y. Numbers and ID strings everywhere.
- No emoji. No exclamation marks. No "Get started!" CTAs.

---

## 9. Data model

`prototype/data.js` is **the spec** for the dataset and the policy
evaluator. It defines:

### Run shape

```ts
type Run = {
  id: string;                     // "req-1a49"
  dt: Date;                       // ISO timestamp
  agent: "pi" | "claude-code" | "codex";
  project: "api" | "search" | "editor" | "incident" | "labs";
  purpose: string;                // human-readable summary
  model: "claude-sonnet-4" | "claude-opus-4" | "gpt-4.1" | "gpt-3.5";
  tools: number;                  // total tool calls
  tokens: number;
  toolsPerTurn: number;
  retries: number;
  cost: number;                   // USD
  runaway: boolean;
  toolCalls: string[];            // tool names invoked
  status: "settled" | "pending";  // pending = awaiting human ask
};
```

### Policy shape

```ts
type Policy = {
  defaults: { decision_mode: "observe" | "warn" | "enforce"; attribute_to: "project" | "user" | "session" };
  models: { allow: string[]; deny: string[] };                  // glob patterns
  budgets: Array<{ id: string; window: string; cap_usd: number; on_exhaust: "block" | "warn" }>;
  limits: { request_cost_usd: number; tools_per_turn: number; retries: number };
  tools: { ask_for: string[] };                                  // glob patterns
  enforced: boolean;
};
```

### Evaluator contract

```ts
evaluateRun(run, policy, runningSpend) → {
  decision: "allow" | "warn" | "deny" | "ask";
  rule: string | null;            // dotted path, e.g. "limits.retries"
  line: number | null;            // line in the YAML
  reason: string;                 // short human reason
  blockedCost: number;            // cost prevented (deny only)
}

evaluateAll(runs, policy) → {
  results: Map<runId, evalResult>;
  tally:   Record<rule, { allow, warn, deny, ask, blockedCost }>;
  totals:  { totalSpend, prevented, denied, warned, asked, allowed, spend30d };
}
```

The order of checks (mirroring the policy file top-to-bottom):
1. `models.deny` → deny
2. `models.allow` → warn if not matched (otherwise continue)
3. `tools.ask_for` → ask
4. `budgets[*].cap_usd` (with `on_exhaust: block`) → deny if running spend would exceed cap
5. `limits.retries` → deny if exceeded
6. `limits.request_cost_usd` → warn if exceeded
7. `limits.tools_per_turn` → warn if exceeded
8. Otherwise → allow

When implementing for real, the evaluator should run on a server (or
locally if Noether stays local-first); the UI just renders its output.

---

## 10. Screenshots

Reference renders of the prototype. Filenames map to the surface/state.

| File | What it shows |
|---|---|
| `01-policy-home.png` | Policy as home. Top of the screen — chrome, page-head, beginning of editor. |
| `02-policy-editor.png` | Editor mid-scroll showing live tally column. |
| `03-policy-tail.png` | Live decisions tail and summary footer. |
| `04-policy-suggestion-rules.png` | Suggestion card + rule-by-rule strip. |
| `05-runs.png` | Runs surface unfiltered. |
| `06-runs-filtered.png` | Runs filtered to `rule: limits.retries`. |
| `07-replay-empty.png` | Replay with no proposed changes (empty state). |
| `08-replay-with-diff.png` | Replay with proposed scenario, winner ribbon, diff. |
| `09-modal-run-detail.png` | Run detail modal. |
| `10-modal-ask.png` | Ask modal — shell.exec approval. |
| `11-cmdk-palette.png` | Command palette. |
| `12-modal-diff.png` | Diff inspector modal. |
| `13-enforced-state.png` | Top bar in enforced state (green pill, "enforced · decisions block"). |

> The narrow-viewport screenshots come from a constrained iframe. The
> intended layout is two-column above ~980px (editor + tail side by side).

---

## 11. What this product is NOT

To keep identity tight, here's what the design intentionally rejects:

- **Not a dashboard of everything.** No KPI tile grid. No "spend over
  time" line as the hero. No 5-tab sidebar.
- **Not a generic request log explorer.** Runs is a list of *decisions*,
  not raw API logs. Every row carries a decision pill + rule.
- **Not a proxy / gateway.** Noether does not stand in front of the model
  call. Harnesses query Noether for a decision (or pre-record). LiteLLM
  is not a competitor and not a replacement.
- **Not enterprise SaaS.** Voice is calm and mathematical. No
  exclamation marks, no upsell, no "schedule a demo" surface anywhere.

---

## 12. Open questions for the implementing team

1. **Where do real `runs` come from?** OpenTelemetry spans? A local
   SQLite of decisions? The evaluator in `data.js` is framework-free; it
   just wants a `Run[]` and a `Policy`.
2. **Policy storage.** The prototype keeps `policy` in memory. In real
   life this is a YAML file at `.noether/policy.noet.yaml` (local-first)
   plus optional team sync.
3. **Inline-edit semantics.** Saving the editor should write to disk.
   "Adopt" overwrites the file; "Revert" reverts the buffer.
4. **Pending asks transport.** How does an agent ask Noether and wait
   for a human? The UI shape (4 buttons) is right; the runtime contract
   is open.
5. **Multi-project.** The prototype's top bar shows `~/work/api · main`.
   For teams, we likely need a project switcher; the design leaves room
   to the left of the modes.

---

## Quick-start for the developer

```bash
# 1) Eyeball the prototype
open prototype/Noether.html

# 2) Read in this order:
#    data.js          → understand the run + policy model
#    app.jsx          → state shape + reducer + keyboard
#    policy.jsx       → home surface (most complex)
#    runs.jsx         → filtering + day grouping
#    replay.jsx       → scenario cards + diff math
#    modals.jsx       → run detail / ask / diff inspector
#    cmdk.jsx         → command palette + canned answers

# 3) Walk through this end-to-end loop in the prototype
#    Policy → edit cap_usd to 50 → tail recounts → suggestion appears
#    Apply → Replay shows diff → Adopt → toast: "still in dry-run"
#    Top-bar Enforce → green pill, "enforced · decisions block"
```

Build what you see. Numbers and copy are real; layout decisions
encoded in CSS are intentional; the interaction loop in §6 is the
contract.
