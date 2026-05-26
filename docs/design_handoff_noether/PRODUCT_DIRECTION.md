# Noether — product direction

A short narrative companion to the handoff. This is the *why* behind the
interface decisions in `README.md`. Optional reading.

---

## The problem we landed on

Noether's docs describe a powerful, multi-layered system: a control
contract for agents, attribution + budgets, simulation, observation that
runs locally before it runs in the cloud. All of that is real value.

But the existing interface tried to **expose all of it at once**. The
result felt like every other AI ops dashboard — KPI tiles + a queue + a
log table + a sidebar of equally-weighted sections. It was *covering the
right needs* and *showing none of the identity*.

When users ask "what does this thing do?", the answer should fall out of
the home screen in two seconds. The old home didn't deliver one.

---

## The five identities we explored

Early on we sketched five distinct centers of gravity for Noether
(`explorations/01-wireframes-5-directions.html`). Each picks a different
first verb:

1. **The Policy** — Noether is a *linter for agent work*. One file is
   the home. (Edit · watch)
2. **The Receipt** — Noether is a *bank statement* for agent runs. The
   ledger is the home. (Read history)
3. **The Gate** — Noether is an *approval inbox*. The next decision is
   the home. (Approve · deny)
4. **The Lab** — Noether is a *strategy lab*. Side-by-side scenarios
   are the home. (Compare strategies)
5. **The Console** — Noether is a *command surface*. An input is the
   home. (Ask)

Two principles fell out of comparing them:

- **The Policy** has the steepest competitive cliff. LiteLLM, Sentry,
  Datadog, Stripe — none of them put a small auditable governance file
  at the center. That makes "policy as home" the most distinctive move
  we can make.
- **The Lab** is the killer demo. The thing you can show in a meeting
  that no proxy product can: "let's try a stricter policy against last
  week's real data, no risk." Replay is irreplaceable.

---

## What we built

The direction is **a mix of Policy, Runs, and Replay**, with Policy as
home and the other two reachable both via the mode switch and via
purposeful cross-links at the bottom of each surface.

- **The Gate** (approval inbox) survives as the **Ask modal** — pending
  decisions show inline in Runs and the Policy tail; clicking opens the
  same approval card the inbox direction had as a homepage. It's a
  *feature*, not a destination.
- **The Console** (command surface) survives as **⌘K** — globally
  available, opens a palette that mixes nav with canned questions that
  read real data. Identity affordance, not a homepage.

This matches the user's framing exactly: *"a mix of policy, replay
(lab) and runs (receipt) [...] CLI is useful as a tool, not a product."*

---

## The core loop

The loop is what holds the three surfaces together. Without it, they're
three side-by-side dashboards, which is what we were avoiding.

```
                       ┌──────────────────────────────┐
                       │            POLICY            │
                       │     (live tail, rule tally)  │
                       └──────────────┬───────────────┘
                                      │ "this rule denied 2 runs"
                                      ▼
   ┌──────────────────────────────────────────────────────────┐
   │                          RUNS                            │
   │  filtered to that rule — every run it touched, in time   │
   └──────────────────────────────────────────────────────────┘
                                      │ "this run was unfair"
                                      ▼
                       ┌──────────────────────────────┐
                       │            REPLAY            │
                       │   what if I changed the rule │
                       │   diff against real history  │
                       └──────────────┬───────────────┘
                                      │ "looks good"
                                      ▼
                       ┌──────────────────────────────┐
                       │      Adopt → POLICY          │
                       │   (still dry-run by default) │
                       └──────────────────────────────┘
```

Every cross-link bar at the bottom of each surface is a pointer along
this loop. The story-of-use is: **a finding becomes a proposed rule
becomes a rehearsed change becomes an enforced policy.**

---

## What identity *not* to import

Looking at the docs (`product-vision.md`, `core-decisions.md`,
`solution-design.md`) and the existing UI, the identity tropes we are
explicitly **not** taking:

- **Not a chart wall.** No "spend over time" line graph as a hero. Real
  data wants a *list of decisions*, not aggregate plots. We show numbers
  only where they explain a decision.
- **Not a wizard.** The policy file is the source of truth. There is no
  "Set up Noether" multi-step. The first time a user opens Noether with
  no policy yet, the editor shows a starter file with `decision_mode:
  observe` and they can edit from there.
- **Not "AI helps you run AI"-flavored.** No copilot panel writing
  policy for you. The suggestion card is calm: "X rule denied 2 runs.
  Y would cover them." One suggestion at a time, dismissable, never
  pushed by a chatbot.

---

## Voice & visual identity

- **Newsreader italic** for headlines — feels like a paper, not an app.
  Page headings are short clauses with periods. ("What's allowed here.")
- **Geist Mono** for everything that comes from the runtime — rule
  names, IDs, costs. The mono is *what comes from the system*; the sans
  is what you (the human) say to it.
- **Paper-warm neutrals** (`#f6f3ec` ground, `#fbf9f3` paper, `#14110d`
  ink). No cyber-dashboard dark mode. No glassmorphism.
- **One accent: burnt orange.** Used sparingly — brand mark dot, the
  one "dry-run" pill, the suggestion card, the ⌘K input glyph. Never
  decorative. Never gradient.
- **Calm motion.** A `slip` animation on new tail rows, a soft fade-in
  on modals, no parallax, no spring overshoot. The product should never
  *demand* attention.

---

## What I'd push the next round on

If the implementing team wants to extend, here's what's worth doing
**next**:

1. **Team mode.** The current design assumes one user. Multi-user
   attribution is in the policy (`attribute_to`), and the architecture
   supports it, but the UI doesn't surface "who" yet. A small `who`
   column in Runs + a `people` projection in Replay would do it.
2. **Diff in Policy.** When `dirty: true`, the editor itself should
   render the diff (red strikethrough → green) inline. The footer
   suggestion card already shows this; the editor doesn't yet.
3. **Pattern engine.** The empty-inbox state for Gate hinted at
   "Noether noticed patterns" (`You allowed shell.exec on incident 3
   times this week — make it a permanent rule?`). That belongs as a
   non-modal card in Policy, alongside the suggestion.
4. **Replay against a scenario.** Right now Replay diffs against real
   last-7d. The exploration also had synthetic scenarios (runaway
   pressure, adoption pressure, model denial). Wire those into the
   `Change scenario` button — the data model already supports it.

None of these are blocking the v1 of what's in the handoff.

---

## One-sentence pitch

**Noether is the policy file for agent work — written once, simulated
honestly, enforced quietly, and explained by every decision it makes.**

If a new user remembers one thing 30 seconds after opening Noether, it
should be that sentence.
