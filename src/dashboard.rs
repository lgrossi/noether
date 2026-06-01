use std::fmt::Write as _;

use crate::ledger::{TraceReport, TraceReportItem, UsageReport};

pub(crate) fn render_dashboard(
    usage: &UsageReport,
    decisions: &[TraceReportItem],
    trace: Option<&TraceReport>,
    observations: &[TraceReportItem],
) -> String {
    let totals = usage_totals(usage);
    let latest_decision = decisions.first();
    let activity = dashboard_activity(trace, observations);
    let decision_stats = decision_stats(decisions);
    let tool_count = activity
        .iter()
        .filter(|item| is_tool_kind(&item.kind))
        .count();
    let agent_count = activity
        .iter()
        .filter(|item| is_agent_kind(&item.kind))
        .count();
    let skill_context_count = activity
        .iter()
        .filter(|item| is_skill_context_kind(&item.kind))
        .count();
    let lifecycle_limits = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("limit.report_only."))
                .count()
        })
        .unwrap_or_default();
    let token_hint = token_hint(&totals);
    let latest_decision_hint = latest_decision
        .map(latest_decision_hint)
        .unwrap_or_else(|| "no authorization decisions yet".to_owned());
    let (story_title, story_lead, story_points) = run_story(
        usage,
        decisions,
        &decision_stats,
        &activity,
        latest_decision,
    );

    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether dashboard</title>");
    html.push_str(dashboard_styles());
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether run dashboard</h1>");
    html.push_str("<div class=\"sub\">Readable local view of decisions, cost, usage, and trace events. Raw hook logs are not needed here.</div>");

    html.push_str("<section class=\"overview\">");
    let _ = write!(
        html,
        "<article class=\"panel story\"><div class=\"eyebrow\">Outcome summary</div><h2>{}</h2><p class=\"lead\">{}</p>",
        escape_html(&story_title),
        escape_html(&story_lead)
    );
    if !story_points.is_empty() {
        html.push_str("<ul class=\"insight-list\">");
        for point in story_points {
            let _ = write!(html, "<li>{}</li>", escape_html(&point));
        }
        html.push_str("</ul>");
    }
    html.push_str("</article>");

    html.push_str("<section class=\"metric-grid\">");
    metric_card(
        &mut html,
        "Finalized spend",
        &format_money(usage.total_cost_usd),
        "what actually landed in the local ledger",
    );
    metric_card(
        &mut html,
        "Tokens",
        &compact_number(totals.total_tokens),
        &token_hint,
    );
    metric_card(
        &mut html,
        "Latest decision",
        latest_decision
            .map(|item| decision_label(&item.kind))
            .unwrap_or("none"),
        &latest_decision_hint,
    );
    metric_card(
        &mut html,
        "Decision mix",
        &format!(
            "{} allow · {} warn · {} deny",
            decision_stats.allow, decision_stats.warn, decision_stats.deny
        ),
        "how often policy allowed, warned, or blocked work",
    );
    if tool_count > 0 || agent_count > 0 || skill_context_count > 0 {
        metric_card(
            &mut html,
            "Run evidence",
            &format!(
                "{} tools · {} agent · {} context",
                tool_count, agent_count, skill_context_count
            ),
            "activity surfaced alongside decisions and budget outcomes",
        );
    } else {
        metric_card(
            &mut html,
            "Visible spend rows",
            &usage.rows.len().to_string(),
            "finalized usage rows that explain where cost landed",
        );
    }
    if let Some(adoption) = &usage.protected_adoption {
        metric_card(
            &mut html,
            "Protected opportunity",
            &format_money(adoption.unused_protected_opportunity_usd),
            "unused current protected grant this window",
        );
        metric_card(
            &mut html,
            "Adoption health",
            &format!(
                "{} low / {} high",
                adoption.low_adopters.len(),
                adoption.high_adopters.len()
            ),
            "simple view of underuse versus heavy protected-budget use",
        );
    } else {
        metric_card(
            &mut html,
            "Limit hits",
            &decision_stats.limit_hits.to_string(),
            "budget limits that fired across recent decisions",
        );
    }
    html.push_str("</section>");
    html.push_str("</section>");

    let has_policy_story =
        !decisions.is_empty() || decision_stats.limit_hits > 0 || lifecycle_limits > 0;
    let has_spend_breakdown = usage.rows.iter().any(|row| row.total_cost_usd > 0.0);
    let has_spend_story = has_spend_breakdown
        || totals.total_tokens > 0
        || !usage.rows.is_empty()
        || usage.protected_adoption.is_some();
    let has_run_evidence = trace.is_some()
        || !observations.is_empty()
        || tool_count > 0
        || agent_count > 0
        || skill_context_count > 0;

    if has_policy_story {
        html.push_str("<section class=\"section-block\">");
        section_header(
            &mut html,
            "Policy",
            "Policy decisions",
            "This section shows how Noether routed work, what it blocked, and the policy evidence behind each outcome.",
        );
        if !decisions.is_empty() {
            html.push_str("<section class=\"split\">");
            decision_flow_panel(&mut html, &decision_stats);
            decisions_panel(&mut html, decisions);
            html.push_str("</section>");
            budget_routing_panel(&mut html, decisions);
        }
        if decision_stats.limit_hits > 0 || lifecycle_limits > 0 {
            html.push_str("<section class=\"split\">");
            if decision_stats.limit_hits > 0 {
                risky_runs_panel(&mut html, decisions);
            }
            if lifecycle_limits > 0 {
                lifecycle_limits_panel(&mut html, trace);
            }
            html.push_str("</section>");
        }
        html.push_str("</section>");
    }

    if has_spend_story {
        html.push_str("<section class=\"section-block\">");
        section_header(
            &mut html,
            "Spend",
            "Spend and adoption",
            "Visual-first cost and adoption views show where finalized usage landed and who still has room to use protected budget.",
        );
        if has_spend_breakdown || totals.total_tokens > 0 {
            html.push_str("<section class=\"split\">");
            if has_spend_breakdown {
                spend_breakdown_panel(&mut html, usage);
            }
            if totals.total_tokens > 0 {
                token_mix_panel(&mut html, &totals);
            }
            html.push_str("</section>");
        }
        if usage.protected_adoption.is_some() {
            adoption_snapshot_panel(&mut html, usage);
            protected_adoption_panel(&mut html, usage);
        }
        if !usage.rows.is_empty() {
            usage_rows_panel(&mut html, usage);
        }
        html.push_str("</section>");
    }

    if has_run_evidence {
        html.push_str("<section class=\"section-block\">");
        section_header(
            &mut html,
            "Evidence",
            "Run evidence",
            "Trace events, tool activity, and agent lifecycle signals explain how the run unfolded without exposing raw prompt logs.",
        );
        html.push_str("<section class=\"split\">");
        if trace.is_some() || !observations.is_empty() {
            timeline_panel(&mut html, trace, observations);
        }
        if tool_count > 0 || agent_count > 0 || skill_context_count > 0 {
            html.push_str("<div class=\"stack-panels\">");
            if tool_count > 0 {
                tools_panel(&mut html, &activity);
            }
            if agent_count > 0 {
                agent_activity_panel(&mut html, &activity);
            }
            if skill_context_count > 0 {
                skill_context_panel(&mut html, &activity);
            }
            html.push_str("</div>");
        }
        html.push_str("</section>");
        html.push_str("</section>");
    }

    html.push_str("</main></body></html>");
    html
}

pub(crate) fn render_simulation_dashboard(
    report: &crate::simulation::SimulationComparisonReport,
) -> String {
    let title = report.name.as_deref().unwrap_or("Simulation comparison");
    let (story_title, story_lead, story_points) = simulation_story(report);
    let spend_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.total_cost_usd,
                format_money(strategy.total_cost_usd),
            )
        })
        .collect();
    let denied_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.denied_requests as f64,
                compact_number(strategy.denied_requests),
            )
        })
        .collect();
    let runaway_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.runaway_spend_prevented_usd,
                format_money(strategy.runaway_spend_prevented_usd),
            )
        })
        .collect();
    let adoption_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.unused_protected_opportunity_usd,
                format_money(strategy.unused_protected_opportunity_usd),
            )
        })
        .collect();
    let fairness_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.fairness_score,
                format!("{:.2}", strategy.fairness_score),
            )
        })
        .collect();
    let highest_spend = report
        .strategies
        .iter()
        .map(|strategy| strategy.total_cost_usd)
        .fold(0.0_f64, f64::max);
    let max_runaway_prevented = report
        .strategies
        .iter()
        .map(|strategy| strategy.runaway_spend_prevented_usd)
        .fold(0.0_f64, f64::max);
    let max_protected_opportunity = report
        .strategies
        .iter()
        .map(|strategy| strategy.unused_protected_opportunity_usd)
        .fold(0.0_f64, f64::max);

    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether dashboard</title>");
    html.push_str(dashboard_styles());
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether dashboard</h1>");
    let _ = write!(
        html,
        "<div class=\"sub\">Comparison view · {} · seed <code>{}</code> over {} simulated day(s).</div>",
        escape_html(title),
        report.seed,
        report.horizon_days
    );

    html.push_str("<section class=\"hero\">");
    let _ = write!(
        html,
        "<article class=\"panel story\"><div class=\"eyebrow\">Comparison summary</div><h2>{}</h2><p class=\"lead\">{}</p>",
        escape_html(&story_title),
        escape_html(&story_lead)
    );
    if !story_points.is_empty() {
        html.push_str("<ul class=\"insight-list\">");
        for point in story_points {
            let _ = write!(html, "<li>{}</li>", escape_html(&point));
        }
        html.push_str("</ul>");
    }
    html.push_str("</article>");

    html.push_str("<section class=\"grid\">");
    metric_card(
        &mut html,
        "Strategies",
        &report.strategies.len().to_string(),
        "policy variants compared over identical demand",
    );
    metric_card(
        &mut html,
        "Total requests",
        &compact_number(report.total_requests),
        "synthetic authorize/finalize opportunities",
    );
    metric_card(
        &mut html,
        "Highest spend",
        &format_money(highest_spend),
        "largest simulated finalized cost among strategies",
    );
    if max_runaway_prevented > 0.0 {
        metric_card(
            &mut html,
            "Runaway prevented",
            &format_money(max_runaway_prevented),
            "best budget-limit outcome across compared strategies",
        );
    } else if max_protected_opportunity > 0.0 {
        metric_card(
            &mut html,
            "Protected opportunity",
            &format_money(max_protected_opportunity),
            "unused adoption budget surfaced by the strongest strategy",
        );
    } else {
        metric_card(
            &mut html,
            "Best fairness",
            &format!(
                "{:.2}",
                report
                    .strategies
                    .iter()
                    .map(|strategy| strategy.fairness_score)
                    .fold(0.0_f64, f64::max)
            ),
            "highest fairness score across compared strategies",
        );
    }
    html.push_str("</section>");
    html.push_str("</section>");

    strategy_scorecards_panel(&mut html, report);

    html.push_str("<section class=\"panel\"><h2>Strategy comparison</h2><p class=\"summary\">These comparisons use the same simulated demand. The bars make the tradeoffs visible before the evidence table.</p>");
    metric_compare_block(
        &mut html,
        "Finalized spend",
        "How much budget actually landed.",
        &spend_values,
        ComparisonEmphasis::Neutral,
    );
    metric_compare_block(
        &mut html,
        "Denied requests",
        "How restrictive each strategy became.",
        &denied_values,
        ComparisonEmphasis::Neutral,
    );
    if runaway_values.iter().any(|(_, value, _)| *value > 0.0) {
        metric_compare_block(
            &mut html,
            "Runaway prevented",
            "Higher means the strategy intercepted more risky spend before it landed.",
            &runaway_values,
            ComparisonEmphasis::HigherBetter,
        );
    }
    if adoption_values.iter().any(|(_, value, _)| *value > 0.0) {
        metric_compare_block(
            &mut html,
            "Protected opportunity",
            "Higher means the strategy surfaced more explicit room for low adopters.",
            &adoption_values,
            ComparisonEmphasis::HigherBetter,
        );
    }
    metric_compare_block(
        &mut html,
        "Fairness score",
        "Higher means spend was distributed more evenly across the simulated users.",
        &fairness_values,
        ComparisonEmphasis::HigherBetter,
    );
    html.push_str("</section>");

    simulation_evidence_table(&mut html, report);
    html.push_str("<section class=\"panel\"><h2>Model mix</h2><div class=\"table-wrap\"><table><thead><tr><th>Strategy</th><th>Model</th><th>Requests</th><th>Cost</th></tr></thead><tbody>");
    for strategy in &report.strategies {
        for mix in &strategy.model_mix {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&strategy.id),
                escape_html(&mix.model_id),
                mix.requests,
                format_money(mix.total_cost_usd)
            );
        }
    }
    html.push_str("</tbody></table></div></section>");
    html.push_str("</main></body></html>");
    html
}

#[derive(Default)]
struct UsageTotals {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
    reservations: u64,
    active_reservations: u64,
    finalized_reservations: u64,
}

fn usage_totals(usage: &UsageReport) -> UsageTotals {
    let mut totals = UsageTotals::default();
    for row in &usage.rows {
        totals.input_tokens += row.input_tokens;
        totals.output_tokens += row.output_tokens;
        totals.cache_read_tokens += row.cache_read_tokens;
        totals.cache_write_tokens += row.cache_write_tokens;
        totals.total_tokens += row.total_tokens;
        totals.reservations += row.reservations;
        totals.active_reservations += row.active_reservations;
        totals.finalized_reservations += row.finalized_reservations;
    }
    totals
}

fn dashboard_activity<'a>(
    trace: Option<&'a TraceReport>,
    observations: &'a [TraceReportItem],
) -> Vec<&'a TraceReportItem> {
    trace
        .map(|trace| trace.items.iter().collect())
        .unwrap_or_else(|| observations.iter().collect())
}

#[derive(Default)]
struct DecisionStats {
    allow: u64,
    warn: u64,
    deny: u64,
    limit_hits: u64,
}

impl DecisionStats {
    fn total(&self) -> u64 {
        self.allow + self.warn + self.deny
    }
}

enum ComparisonEmphasis {
    Neutral,
    HigherBetter,
}

fn dashboard_styles() -> &'static str {
    r#"<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --good:#22c55e; --warn:#f59e0b; --bad:#ef4444; --blue:#38bdf8; --violet:#a78bfa; --slate:#64748b; }
        * { box-sizing: border-box; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:radial-gradient(circle at top left,#172554,#0f172a 42%); color:var(--text); }
        main { max-width:1180px; margin:0 auto; padding:32px 20px 48px; }
        h1 { margin:0 0 4px; font-size:34px; letter-spacing:-0.04em; }
        h2 { margin:0 0 12px; font-size:24px; letter-spacing:-0.03em; }
        h3 { margin:20px 0 10px; font-size:16px; }
        code { color:var(--blue); }
        .sub, .summary, .hint { color:var(--muted); }
        .sub { margin-bottom:24px; }
        .overview { display:grid; gap:14px; grid-template-columns:1fr; align-items:start; margin-bottom:14px; }
        .metric-grid { display:grid; gap:14px; grid-template-columns:repeat(3,minmax(0,1fr)); align-content:start; }
        .split { display:grid; gap:14px; grid-template-columns:repeat(2,minmax(0,1fr)); align-items:start; }
        .stack-panels { display:grid; gap:14px; }
        .section-block { margin-top:28px; }
        .section-header { margin:0 0 14px; }
        .section-name { font-size:24px; font-weight:800; letter-spacing:-0.03em; color:#f8fbff; }
        .section-header .summary { margin:4px 0 0; max-width:72ch; }
        .story { padding:22px; }
        .eyebrow, .label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .lead { font-size:18px; margin:0; color:#dbe7f4; }
        .grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(210px,1fr)); }
        .card, .panel { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; box-shadow:0 18px 55px rgba(0,0,0,.22); }
        .card { padding:18px; }
        .panel { padding:18px; margin-top:14px; overflow:hidden; }
        .overview > .panel, .overview > .metric-grid > .card, .overview > .metric-grid > .panel, .split > .panel, .split > .stack-panels { margin-top:0; }
        .value { font-size:30px; font-weight:800; margin-top:6px; letter-spacing:-0.03em; }
        .value.small { font-size:24px; }
        .insight-list { margin:14px 0 0 18px; padding:0; }
        .insight-list li { margin:6px 0; }
        .bar { height:12px; display:flex; overflow:hidden; border-radius:999px; background:#1e293b; margin:10px 0; }
        .track { height:12px; width:100%; border-radius:999px; background:#1e293b; overflow:hidden; }
        .fill { height:100%; border-radius:999px; }
        .fill.good, .dot.good, .segment.good { background:var(--good); }
        .fill.warn, .dot.warn, .segment.warn { background:var(--warn); }
        .fill.bad, .dot.bad, .segment.bad { background:var(--bad); }
        .fill.blue, .dot.blue, .segment.blue, .in { background:var(--blue); }
        .fill.violet, .dot.violet, .segment.violet, .out { background:var(--violet); }
        .fill.slate, .dot.slate, .segment.slate, .cache { background:var(--slate); }
        .legend { display:flex; gap:16px; flex-wrap:wrap; color:var(--muted); font-size:13px; }
        .dot { display:inline-block; width:9px; height:9px; border-radius:50%; margin-right:6px; }
        .table-wrap { overflow:auto; }
        table { width:100%; border-collapse:collapse; }
        th, td { text-align:left; padding:10px 8px; border-top:1px solid var(--line); vertical-align:top; }
        th { color:var(--muted); font-weight:600; font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .pill { display:inline-flex; align-items:center; border-radius:999px; padding:4px 9px; background:#1e293b; border:1px solid var(--line); font-size:13px; }
        .meta-pill { display:inline-flex; align-items:center; gap:6px; border-radius:999px; padding:4px 10px; background:rgba(30,41,59,.8); border:1px solid rgba(148,163,184,.18); color:#dbe7f4; font-size:12px; }
        .ok { color:var(--good); } .warn { color:var(--warn); } .bad { color:var(--bad); }
        .compare-group { margin-top:18px; }
        .compare-title { margin-bottom:4px; font-weight:700; }
        .compare-row { display:grid; grid-template-columns:minmax(0,220px) minmax(0,1fr) auto; gap:12px; align-items:center; padding:10px 0; border-top:1px solid var(--line); }
        .compare-row:first-of-type { border-top:0; }
        .compare-label strong { display:block; }
        .metric-value { font-weight:700; white-space:nowrap; }
        .score-grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(250px,1fr)); }
        .score-list { list-style:none; margin:12px 0 0; padding:0; }
        .score-list li { margin:6px 0; color:var(--muted); }
        .section-intro { margin-top:4px; color:var(--muted); }
        details.evidence { margin-top:8px; }
        details.evidence summary { cursor:pointer; color:var(--muted); }
        .entry-list { display:grid; gap:12px; }
        .entry-card { padding:16px; border-radius:16px; border:1px solid rgba(148,163,184,.15); background:rgba(15,23,42,.45); }
        .entry-top { display:flex; justify-content:space-between; gap:12px; align-items:flex-start; flex-wrap:wrap; }
        .entry-title { margin-top:8px; font-size:18px; font-weight:700; color:#eef6ff; letter-spacing:-0.02em; }
        .meta-row { display:flex; gap:8px; flex-wrap:wrap; margin-top:10px; }
        .fact-grid { display:grid; gap:10px; grid-template-columns:repeat(auto-fit,minmax(140px,1fr)); margin-top:12px; }
        .fact { padding:10px 12px; border-radius:12px; border:1px solid rgba(148,163,184,.12); background:rgba(30,41,59,.45); }
        .fact-label { display:block; margin-bottom:3px; color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.08em; }
        .fact-value { color:#f8fbff; font-weight:700; }
        .entity-grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(250px,1fr)); }
        .entity-card { padding:18px; border-radius:16px; border:1px solid rgba(148,163,184,.15); background:rgba(15,23,42,.45); }
        .entity-card.accent-good { box-shadow:inset 0 0 0 1px rgba(34,197,94,.18); }
        .entity-card.accent-violet { box-shadow:inset 0 0 0 1px rgba(167,139,250,.18); }
        .inline-stats { display:flex; gap:14px; flex-wrap:wrap; margin-top:10px; color:var(--muted); font-size:13px; }
        .timeline { list-style:none; margin:0; padding:0; }
        .event { display:grid; grid-template-columns:165px 210px 1fr; gap:12px; padding:13px 0; border-top:1px solid var(--line); align-items:start; }
        .event:first-child { border-top:0; }
        .time { color:var(--muted); }
        .kind { font-weight:700; }
        .stack { height:14px; display:flex; border-radius:999px; overflow:hidden; background:#1e293b; margin:12px 0; }
        @media (max-width:1100px) { .metric-grid { grid-template-columns:repeat(2,minmax(0,1fr)); } }
        @media (max-width:900px) { .overview, .split, .metric-grid { grid-template-columns:1fr; } }
        @media (max-width:760px) { .event, .compare-row { grid-template-columns:1fr; gap:6px; } h1 { font-size:28px; } .section-name { font-size:22px; } }
        </style>"#
}

fn section_header(html: &mut String, eyebrow: &str, title: &str, summary: &str) {
    let _ = write!(
        html,
        "<div class=\"section-header\"><div class=\"eyebrow\">{}</div><div class=\"section-name\">{}</div><p class=\"summary\">{}</p></div>",
        escape_html(eyebrow),
        escape_html(title),
        escape_html(summary)
    );
}

fn fact_block(html: &mut String, label: &str, value: &str) {
    let _ = write!(
        html,
        "<div class=\"fact\"><span class=\"fact-label\">{}</span><span class=\"fact-value\">{}</span></div>",
        escape_html(label),
        escape_html(value)
    );
}

fn decision_stats(decisions: &[TraceReportItem]) -> DecisionStats {
    let mut stats = DecisionStats::default();
    for item in decisions {
        if item.kind.ends_with(".deny") {
            stats.deny += 1;
        } else if item.kind.ends_with(".warn") {
            stats.warn += 1;
        } else if item.kind.ends_with(".allow") {
            stats.allow += 1;
        }
        stats.limit_hits += item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0);
    }
    stats
}

fn run_story(
    usage: &UsageReport,
    decisions: &[TraceReportItem],
    stats: &DecisionStats,
    activity: &[&TraceReportItem],
    latest_decision: Option<&TraceReportItem>,
) -> (String, String, Vec<String>) {
    let title = if stats.deny > 0 && stats.limit_hits > 0 {
        "Risky spend was blocked before it landed".to_owned()
    } else if usage.total_cost_usd > 0.0 {
        format!("This run finalized {}", format_money(usage.total_cost_usd))
    } else if stats.allow + stats.warn > 0 {
        "Work was authorized, but no finalized spend landed yet".to_owned()
    } else {
        "Noether is waiting for meaningful run evidence".to_owned()
    };
    let lead = if stats.deny > 0 && stats.limit_hits > 0 {
        format!(
            "{} request(s) were denied and {} limit hit(s) fired. Finalized spend stayed at {}.",
            stats.deny,
            stats.limit_hits,
            format_money(usage.total_cost_usd)
        )
    } else if usage.total_cost_usd > 0.0 {
        format!(
            "{} decision(s) produced {} finalized reservation(s) across {} visible spend row(s).",
            stats.total(),
            usage
                .rows
                .iter()
                .map(|row| row.finalized_reservations)
                .sum::<u64>(),
            usage.rows.len()
        )
    } else if let Some(item) = latest_decision {
        format!("Latest decision: {}.", item.summary)
    } else {
        "No authorization decisions, trace events, or finalized usage have been captured yet."
            .to_owned()
    };

    let mut points = Vec::new();
    if let Some(top_row) = usage
        .rows
        .iter()
        .max_by(|left, right| left.total_cost_usd.total_cmp(&right.total_cost_usd))
    {
        points.push(format!(
            "Most visible spend went to {} for {} / {}.",
            top_row
                .project
                .as_deref()
                .unwrap_or("an unattributed project"),
            top_row.provider.as_deref().unwrap_or("unknown provider"),
            top_row.model.as_deref().unwrap_or("unknown model")
        ));
    }
    if let Some(item) = latest_decision
        && let Some(detail) = decision_supporting_line(item)
    {
        points.push(detail);
    }
    if stats.warn > 0 {
        points.push(format!(
            "{} decision(s) were warned instead of blocked, which means work continued under policy pressure.",
            stats.warn
        ));
    }
    if let Some(adoption) = &usage.protected_adoption
        && adoption.unused_protected_opportunity_usd > 0.0
    {
        points.push(format!(
            "{} of protected opportunity is still available across {} low adopters.",
            format_money(adoption.unused_protected_opportunity_usd),
            adoption.low_adopters.len()
        ));
    }
    let tool_events = activity
        .iter()
        .filter(|item| is_tool_kind(&item.kind))
        .count();
    if tool_events > 0 {
        points.push(format!(
            "{} tool event(s) were captured, so this view includes actual workflow evidence beyond model billing.",
            tool_events
        ));
    }
    if points.is_empty() && !decisions.is_empty() {
        points.push(
            "Recent decision cards carry the routing, model, and limit evidence for this run."
                .to_owned(),
        );
    }
    (title, lead, points)
}

fn simulation_story(
    report: &crate::simulation::SimulationComparisonReport,
) -> (String, String, Vec<String>) {
    let mut notes = Vec::new();
    for strategy in &report.strategies {
        if let Some(day) = strategy.exhaustion_day {
            notes.push(format!(
                "{} exhausted shared budget on day {}.",
                strategy.id, day
            ));
        }
        if strategy.limit_hit_count > 0 {
            notes.push(format!(
                "{} blocked {} limit-hit requests, prevented {}, and left {} unused.",
                strategy.id,
                strategy.limit_hit_count,
                format_money(strategy.runaway_spend_prevented_usd),
                format_money(strategy.unused_budget_usd)
            ));
        }
        if strategy.unused_protected_opportunity_usd > 0.0
            || strategy.low_adopter_count > 0
            || strategy.high_adopter_count > 0
        {
            notes.push(format!(
                "{} surfaced {} of unused protected opportunity across {} low adopters and {} high adopters.",
                strategy.id,
                format_money(strategy.unused_protected_opportunity_usd),
                strategy.low_adopter_count,
                strategy.high_adopter_count
            ));
        }
    }

    let title = if report
        .strategies
        .iter()
        .any(|strategy| strategy.limit_hit_count > 0)
    {
        "Budget limits changed the spend story".to_owned()
    } else if report
        .strategies
        .iter()
        .any(|strategy| strategy.unused_protected_opportunity_usd > 0.0)
    {
        "Adoption policy changed what the team could see".to_owned()
    } else {
        "Policy choices changed the outcome under identical demand".to_owned()
    };
    let lead = format!(
        "{} strategy variants processed {} simulated requests with the same synthetic demand.",
        report.strategies.len(),
        compact_number(report.total_requests)
    );
    (title, lead, notes)
}

fn metric_card(html: &mut String, label: &str, value: &str, hint: &str) {
    let _ = write!(
        html,
        "<article class=\"card\"><div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"hint\">{}</div></article>",
        escape_html(label),
        escape_html(value),
        escape_html(hint)
    );
}

fn meta_pill(html: &mut String, value: &str) {
    let _ = write!(
        html,
        "<span class=\"meta-pill\">{}</span>",
        escape_html(value)
    );
}

fn fact_block_if_some(html: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        fact_block(html, label, value);
    }
}

fn details_block(html: &mut String, summary: &str, evidence: &str) {
    let _ = write!(
        html,
        "<details class=\"evidence\"><summary>{}</summary><div class=\"summary\">{}</div></details>",
        escape_html(summary),
        escape_html(evidence)
    );
}

fn routing_evidence_present(item: &TraceReportItem) -> bool {
    decision_budget(item).is_some()
        || decision_request(item).is_some()
        || decision_remaining_budget(item).is_some()
        || decision_estimated_cost(item).is_some()
        || decision_matched_entity(item).is_some()
        || decision_model_check(item).is_some()
        || item
            .limit_hits
            .as_ref()
            .is_some_and(|hits| !hits.is_empty())
}

fn compare_row(
    html: &mut String,
    label: &str,
    detail: &str,
    value: &str,
    ratio: f64,
    fill_class: &str,
) {
    let _ = write!(
        html,
        "<div class=\"compare-row\"><div class=\"compare-label\"><strong>{}</strong><div class=\"summary\">{}</div></div><div class=\"track\"><div class=\"fill {}\" style=\"width:{:.2}%\"></div></div><div class=\"metric-value\">{}</div></div>",
        escape_html(label),
        escape_html(detail),
        fill_class,
        ratio.clamp(0.0, 100.0),
        escape_html(value)
    );
}

fn decision_flow_panel(html: &mut String, stats: &DecisionStats) {
    let total = stats.total();
    if total == 0 {
        return;
    }
    let allow = percent(stats.allow, total);
    let warn = percent(stats.warn, total);
    let deny = percent(stats.deny, total);
    let posture = if stats.deny > 0 {
        "Budget limits actively stopped risky work."
    } else if stats.warn > 0 {
        "Policy allowed work to continue under pressure."
    } else {
        "Policy stayed in an allow-first posture."
    };
    html.push_str("<section class=\"panel\"><h2>Budget posture</h2><p class=\"summary\">Start here for the policy shape of the run: what continued, what continued under warning, and what was blocked before spend landed.</p>");
    let _ = write!(
        html,
        "<div class=\"entry-title\">{}</div>",
        escape_html(posture)
    );
    let _ = write!(
        html,
        "<div class=\"stack\"><div class=\"segment good\" style=\"width:{allow:.2}%\"></div><div class=\"segment warn\" style=\"width:{warn:.2}%\"></div><div class=\"segment bad\" style=\"width:{deny:.2}%\"></div></div>"
    );
    let _ = write!(
        html,
        "<div class=\"legend\"><span><span class=\"dot good\"></span>allow {}</span><span><span class=\"dot warn\"></span>warn {}</span><span><span class=\"dot bad\"></span>deny {}</span><span>limit hits {}</span></div>",
        stats.allow, stats.warn, stats.deny, stats.limit_hits
    );
    html.push_str("<div class=\"fact-grid\">");
    fact_block(html, "Decisions observed", &total.to_string());
    fact_block(html, "Limit hits", &stats.limit_hits.to_string());
    fact_block(html, "Allowed share", &format!("{allow:.0}%"));
    fact_block(html, "Blocked share", &format!("{deny:.0}%"));
    html.push_str("</div>");
    html.push_str("</section>");
}

fn spend_breakdown_panel(html: &mut String, usage: &UsageReport) {
    let max_cost = usage
        .rows
        .iter()
        .map(|row| row.total_cost_usd)
        .fold(0.0_f64, f64::max);
    if max_cost <= 0.0 {
        return;
    }
    html.push_str("<section class=\"panel\"><h2>Where the spend went</h2><p class=\"summary\">The tallest bars show where finalized cost concentrated, so you can see whether one project, model, or subject dominated the run.</p>");
    for row in usage.rows.iter().take(6) {
        let ratio = if max_cost == 0.0 {
            0.0
        } else {
            (row.total_cost_usd / max_cost) * 100.0
        };
        compare_row(
            html,
            row.project.as_deref().unwrap_or("-"),
            &format!(
                "{} / {} · {}",
                row.provider.as_deref().unwrap_or("-"),
                row.model.as_deref().unwrap_or("-"),
                row.subject.as_deref().unwrap_or("-")
            ),
            &format_money(row.total_cost_usd),
            ratio,
            "blue",
        );
    }
    html.push_str("</section>");
}

fn metric_compare_block(
    html: &mut String,
    title: &str,
    hint: &str,
    values: &[(String, f64, String)],
    emphasis: ComparisonEmphasis,
) {
    let max_value = values
        .iter()
        .map(|(_, value, _)| *value)
        .fold(0.0_f64, f64::max);
    let best_value = values
        .iter()
        .map(|(_, value, _)| *value)
        .fold(0.0_f64, f64::max);
    html.push_str("<div class=\"compare-group\">");
    let _ = write!(
        html,
        "<div class=\"compare-title\">{}</div><div class=\"summary\">{}</div>",
        escape_html(title),
        escape_html(hint)
    );
    for (label, value, display) in values {
        let ratio = if max_value == 0.0 {
            0.0
        } else {
            (*value / max_value) * 100.0
        };
        let fill = match emphasis {
            ComparisonEmphasis::Neutral => "blue",
            ComparisonEmphasis::HigherBetter if (*value - best_value).abs() < 1e-9 => "good",
            ComparisonEmphasis::HigherBetter => "violet",
        };
        compare_row(html, label, "", display, ratio, fill);
    }
    html.push_str("</div>");
}

fn strategy_scorecards_panel(
    html: &mut String,
    report: &crate::simulation::SimulationComparisonReport,
) {
    html.push_str("<section class=\"panel\"><h2>Strategy scorecards</h2><p class=\"summary\">Each card tells the story of one strategy before you drop into the evidence table.</p><div class=\"score-grid\">");
    for strategy in &report.strategies {
        let exhaustion = strategy
            .exhaustion_day
            .map(|day| format!("budget exhausted on day {day}"))
            .unwrap_or_else(|| "budget stayed available through the horizon".to_owned());
        let _ = write!(
            html,
            "<article class=\"card\"><div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"hint\">{} allowed · {} denied · fairness {:.2}</div><ul class=\"score-list\"><li>{}</li><li>Unused budget: {}</li><li>Runaway prevented: {}</li>",
            escape_html(&strategy.id),
            format_money(strategy.total_cost_usd),
            strategy.allowed_requests,
            strategy.denied_requests,
            strategy.fairness_score,
            escape_html(&exhaustion),
            format_money(strategy.unused_budget_usd),
            format_money(strategy.runaway_spend_prevented_usd),
        );
        if strategy.unused_protected_opportunity_usd > 0.0
            || strategy.low_adopter_count > 0
            || strategy.high_adopter_count > 0
        {
            let _ = write!(
                html,
                "<li>Protected opportunity: {} across {} low adopters and {} high adopters.</li>",
                format_money(strategy.unused_protected_opportunity_usd),
                strategy.low_adopter_count,
                strategy.high_adopter_count
            );
        }
        html.push_str("</ul></article>");
    }
    html.push_str("</div></section>");
}

fn simulation_evidence_table(
    html: &mut String,
    report: &crate::simulation::SimulationComparisonReport,
) {
    html.push_str("<section class=\"panel\"><h2>Detailed comparison</h2><div class=\"table-wrap\"><table><thead><tr><th>Strategy</th><th>Spend</th><th>Denied</th><th>Runaway prevented</th><th>Protected opportunity</th><th>Fairness</th><th>Exhaustion</th></tr></thead><tbody>");
    for strategy in &report.strategies {
        let exhaustion = strategy
            .exhaustion_day
            .map(|day| day.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.2}</td><td>{}</td></tr>",
            escape_html(&strategy.id),
            format_money(strategy.total_cost_usd),
            strategy.denied_requests,
            format_money(strategy.runaway_spend_prevented_usd),
            format_money(strategy.unused_protected_opportunity_usd),
            strategy.fairness_score,
            escape_html(&exhaustion)
        );
    }
    html.push_str("</tbody></table></div></section>");
}

fn token_mix_panel(html: &mut String, totals: &UsageTotals) {
    html.push_str("<section class=\"panel\"><h2>Token mix</h2>");
    if totals.total_tokens == 0 {
        html.push_str("<div class=\"empty\">No finalized token usage yet.</div></section>");
        return;
    }
    if totals.input_tokens == 0
        && totals.output_tokens == 0
        && totals.cache_read_tokens == 0
        && totals.cache_write_tokens == 0
    {
        let _ = write!(
            html,
            "<p class=\"summary\">Only the total token count was finalized for this run, so Noether cannot yet split it into input, output, or cache categories.</p><div class=\"bar\"><div class=\"fill slate\" style=\"width:100%\"></div></div><div class=\"legend\"><span><span class=\"dot slate\"></span>total {}</span></div>",
            compact_number(totals.total_tokens)
        );
        html.push_str("</section>");
        return;
    }
    let input = percent(totals.input_tokens, totals.total_tokens);
    let output = percent(totals.output_tokens, totals.total_tokens);
    let cache = percent(
        totals.cache_read_tokens + totals.cache_write_tokens,
        totals.total_tokens,
    );
    let _ = write!(
        html,
        "<div class=\"bar\"><div class=\"in\" style=\"width:{input:.2}%\"></div><div class=\"out\" style=\"width:{output:.2}%\"></div><div class=\"cache\" style=\"width:{cache:.2}%\"></div></div>"
    );
    let _ = write!(
        html,
        "<div class=\"legend\"><span><span class=\"dot in\"></span>input {}</span><span><span class=\"dot out\"></span>output {}</span><span><span class=\"dot cache\"></span>cache {}</span></div>",
        compact_number(totals.input_tokens),
        compact_number(totals.output_tokens),
        compact_number(totals.cache_read_tokens + totals.cache_write_tokens)
    );
    html.push_str("</section>");
}

fn adoption_snapshot_panel(html: &mut String, usage: &UsageReport) {
    let Some(adoption) = &usage.protected_adoption else {
        return;
    };
    html.push_str("<section class=\"panel\"><h2>Adoption snapshot</h2><p class=\"summary\">Protected budget only matters if it reaches under-users without hiding where heavy protected usage is already concentrated.</p><div class=\"grid\">");
    metric_card(
        html,
        "Protected opportunity remaining",
        &format_money(adoption.unused_protected_opportunity_usd),
        "current protected grant that still has room to be used",
    );
    metric_card(
        html,
        "Carryover liability",
        &format_money(adoption.carryover_liability_usd),
        "unused protected grant that can roll forward into the next window",
    );
    metric_card(
        html,
        "Low adopters",
        &adoption.low_adopters.len().to_string(),
        "people or teams with meaningful protected room left",
    );
    metric_card(
        html,
        "Top consumers",
        &adoption.high_adopters.len().to_string(),
        "people or teams already consuming most of the protected pool",
    );
    html.push_str("</div></section>");
}

fn usage_rows_panel(html: &mut String, usage: &UsageReport) {
    html.push_str("<section class=\"panel\"><h2>Spend evidence</h2><p class=\"summary\">These cards keep row-level billing evidence readable without forcing a wide ledger table.</p>");
    if usage.rows.is_empty() {
        html.push_str("<div class=\"empty\">No usage has been finalized yet.</div></section>");
        return;
    }
    html.push_str("<div class=\"entry-list\">");
    for row in usage.rows.iter().take(8) {
        let title = row
            .project
            .as_deref()
            .or(row.subject.as_deref())
            .unwrap_or("Unattributed spend");
        let summary = format!(
            "{} finalized {} on {} / {} across {} token(s).",
            row.subject.as_deref().unwrap_or("This row"),
            format_money(row.total_cost_usd),
            row.provider.as_deref().unwrap_or("unknown provider"),
            row.model.as_deref().unwrap_or("unknown model"),
            compact_number(row.total_tokens)
        );
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">Finalized spend row</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(title),
            escape_html(&summary)
        );
        html.push_str("<div class=\"meta-row\">");
        meta_pill(html, &format_money(row.total_cost_usd));
        meta_pill(
            html,
            &format!("{} tokens", compact_number(row.total_tokens)),
        );
        html.push_str("</div></div><div class=\"fact-grid\">");
        fact_block_if_some(html, "Project", row.project.as_deref());
        fact_block_if_some(html, "Subject", row.subject.as_deref());
        fact_block_if_some(html, "Provider", row.provider.as_deref());
        fact_block_if_some(html, "Model", row.model.as_deref());
        fact_block(html, "Finalized", &row.finalized_reservations.to_string());
        fact_block(html, "Active", &row.active_reservations.to_string());
        html.push_str("</div></article>");
    }
    html.push_str("</div></section>");
}

fn protected_adoption_cards_panel(
    html: &mut String,
    title: &str,
    summary: &str,
    entries: &[crate::ledger::ProtectedAdoptionEntityReport],
    accent: &str,
    opportunity_label: &str,
) {
    if entries.is_empty() {
        return;
    }
    let _ = write!(
        html,
        "<section class=\"panel\"><h2>{}</h2><p class=\"summary\">{}</p><div class=\"entity-grid\">",
        escape_html(title),
        escape_html(summary)
    );
    for entity in entries {
        let _ = write!(
            html,
            "<article class=\"entity-card {}\"><div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div>",
            escape_html(accent),
            escape_html(&entity.budget_id),
            escape_html(&entity.entity_key)
        );
        let lead = if accent == "accent-violet" {
            format!(
                "{} still available from the current protected grant after only {} of visible use.",
                format_money(entity.current_grant_usd),
                format_money(entity.used_current_grant_usd)
            )
        } else {
            format!(
                "{} has already been used from a protected amount of {}.",
                format_money(entity.used_current_grant_usd),
                format_money(entity.protected_amount_usd)
            )
        };
        let _ = write!(html, "<p class=\"summary\">{}</p>", escape_html(&lead));
        html.push_str("<div class=\"fact-grid\">");
        fact_block(
            html,
            opportunity_label,
            &format_money(entity.current_grant_usd),
        );
        fact_block(html, "Carryover", &format_money(entity.carryover_usd));
        fact_block(
            html,
            "Current usage",
            &format_money(entity.used_current_grant_usd),
        );
        fact_block(
            html,
            "Protected amount",
            &format_money(entity.protected_amount_usd),
        );
        html.push_str("</div></article>");
    }
    html.push_str("</div></section>");
}

fn protected_adoption_panel(html: &mut String, usage: &UsageReport) {
    let Some(adoption) = &usage.protected_adoption else {
        return;
    };
    protected_adoption_cards_panel(
        html,
        "Protected opportunity remaining",
        "These are the people or teams who still have meaningful protected budget available and may need enablement rather than stricter caps.",
        &adoption.low_adopters,
        "accent-violet",
        "Opportunity left",
    );
    protected_adoption_cards_panel(
        html,
        "Top consumers",
        "These are the heaviest protected-budget consumers in the current window, which helps separate healthy adoption from concentrated usage.",
        &adoption.high_adopters,
        "accent-good",
        "Current grant left",
    );
}

fn event_entries_panel(
    html: &mut String,
    title: &str,
    summary: &str,
    items: &[&TraceReportItem],
    empty_message: &str,
) {
    let _ = write!(
        html,
        "<section class=\"panel\"><h2>{}</h2><p class=\"summary\">{}</p>",
        escape_html(title),
        escape_html(summary)
    );
    if items.is_empty() {
        let _ = write!(
            html,
            "<div class=\"empty\">{}</div></section>",
            escape_html(empty_message)
        );
        return;
    }
    html.push_str("<div class=\"entry-list\">");
    for item in items.iter().take(8) {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(&short_time(item)),
            escape_html(&item.kind),
            escape_html(&item.summary)
        );
        html.push_str("<div class=\"meta-row\">");
        meta_pill(html, &item.kind);
        html.push_str("</div></div>");
        details_block(html, "Show exact event evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn tools_panel(html: &mut String, activity: &[&TraceReportItem]) {
    let tools: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_tool_kind(&item.kind))
        .collect();
    event_entries_panel(
        html,
        "Tool usage",
        "Tool cards show what Pi invoked and what landed back in the trace without exposing raw prompt logs.",
        &tools,
        "No tool calls or tool results were observed for this run yet. If Pi did not use tools, this is expected.",
    );
}

fn agent_activity_panel(html: &mut String, activity: &[&TraceReportItem]) {
    let agent_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_agent_kind(&item.kind))
        .collect();
    event_entries_panel(
        html,
        "Agent activity",
        "Lifecycle cards show how the agent progressed through provider calls, turn boundaries, and final completion.",
        &agent_events,
        "No Pi agent lifecycle events were observed yet. This usually means the run came from the vertical demo or Pi did not emit lifecycle hooks for this trace.",
    );
}

fn skill_context_panel(html: &mut String, activity: &[&TraceReportItem]) {
    let context_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_skill_context_kind(&item.kind))
        .collect();
    event_entries_panel(
        html,
        "Skills and context",
        "Context cards show the skills, tools, and repo context Pi carried into the run without leaking prompt content.",
        &context_events,
        "No skill/context event was observed yet. When Pi provides agent context, this section will show selected tools, skills, and context-file summaries without prompt text.",
    );
}

fn decisions_panel(html: &mut String, decisions: &[TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Decision narrative</h2><p class=\"summary\">Readable cards come first; exact ledger fields stay collapsed underneath each decision.</p>");
    if decisions.is_empty() {
        html.push_str("<div class=\"empty\">No authorization decisions yet.</div></section>");
        return;
    }
    html.push_str("<div class=\"entry-list\">");
    for item in decisions.iter().take(8) {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div>{}<div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&decision_headline(item)),
            escape_html(&latest_decision_hint(item))
        );
        html.push_str("<div class=\"meta-row\">");
        if let Some(budget) = decision_budget(item) {
            meta_pill(html, &budget);
        }
        if let Some(model) = decision_model(item) {
            meta_pill(html, &model);
        }
        if let Some(request) = decision_request(item) {
            meta_pill(html, &request);
        }
        html.push_str("</div></div><div class=\"fact-grid\">");
        fact_block_if_some(html, "Budget", decision_budget(item).as_deref());
        fact_block_if_some(
            html,
            "Matched entity",
            decision_matched_entity(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Budget-window remaining",
            decision_remaining_budget(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Estimated cost",
            decision_estimated_cost(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Model check",
            decision_model_check_label(item).as_deref(),
        );
        html.push_str("</div>");
        if let Some(hits) = &item.limit_hits {
            html.push_str("<ul class=\"score-list\">");
            for hit in hits {
                let _ = write!(
                    html,
                    "<li><strong>{}</strong> - {}</li>",
                    escape_html(&hit.rule_id),
                    escape_html(&hit.reason)
                );
            }
            html.push_str("</ul>");
        }
        details_block(html, "Show exact decision evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn budget_routing_panel(html: &mut String, decisions: &[TraceReportItem]) {
    let routed: Vec<_> = decisions
        .iter()
        .filter(|item| routing_evidence_present(item))
        .take(6)
        .collect();
    if routed.is_empty() {
        return;
    }
    html.push_str("<section class=\"panel\"><h2>Budget routing</h2><p class=\"summary\">This layer explains why Noether chose a budget, how much room was left, and what fallback or model checks shaped the decision.</p><div class=\"entry-list\">");
    for item in routed {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let title = decision_budget(item)
            .map(|budget| {
                format!(
                    "{} landed on {}",
                    decision_model(item).unwrap_or_else(|| "Request".to_owned()),
                    budget
                )
            })
            .unwrap_or_else(|| decision_headline(item));
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(&short_time(item)),
            escape_html(&title),
            escape_html(&decision_supporting_line(item).unwrap_or_else(|| {
                "No additional routing explanation was recorded.".to_owned()
            }))
        );
        html.push_str("<div class=\"meta-row\">");
        if let Some(request) = decision_request(item) {
            meta_pill(html, &request);
        }
        if let Some(entity) = decision_matched_entity(item) {
            meta_pill(html, &entity);
        }
        html.push_str("</div></div><div class=\"fact-grid\">");
        fact_block_if_some(html, "Budget", decision_budget(item).as_deref());
        fact_block_if_some(
            html,
            "Matched entity",
            decision_matched_entity(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Estimated cost",
            decision_estimated_cost(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Budget-window remaining",
            decision_remaining_budget(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Model check",
            decision_model_check_label(item).as_deref(),
        );
        html.push_str("</div>");
        details_block(html, "Show exact routing evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn risky_runs_panel(html: &mut String, decisions: &[TraceReportItem]) {
    let risky: Vec<_> = decisions
        .iter()
        .filter(|item| {
            item.limit_hits
                .as_ref()
                .is_some_and(|hits| !hits.is_empty())
        })
        .collect();
    if risky.is_empty() {
        return;
    }
    html.push_str("<section class=\"panel\"><h2>Risky runs</h2><p class=\"summary\">These decisions hit budget limits. Read the plain-language reason first, then expand the exact ledger evidence if needed.</p><div class=\"entry-list\">");
    for item in risky {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div>{}<div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div></div>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&decision_headline(item)),
            escape_html(&latest_decision_hint(item))
        );
        html.push_str("<ul class=\"score-list\">");
        for hit in item.limit_hits.as_ref().into_iter().flatten() {
            let _ = write!(
                html,
                "<li><strong>{}</strong> - {}</li>",
                escape_html(&limit_hit_name(hit)),
                escape_html(&hit.reason)
            );
        }
        html.push_str("</ul>");
        details_block(html, "Show exact limit evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn lifecycle_limits_panel(html: &mut String, trace: Option<&TraceReport>) {
    let items: Vec<&TraceReportItem> = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("limit.report_only."))
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return;
    }
    html.push_str(
        "<section class=\"panel\"><h2>Lifecycle limits (report-only)</h2><p class=\"summary\">These lifecycle signals were detected after the run emitted events. They are audit evidence, not proof that Noether blocked the action before it happened.</p><div class=\"entry-list\">",
    );
    for item in items {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div><div class=\"meta-row\">",
            escape_html(&short_time(item)),
            escape_html(&item.kind),
            escape_html(&item.summary)
        );
        meta_pill(html, "report-only");
        meta_pill(html, &item.kind);
        html.push_str("</div></div>");
        details_block(html, "Show exact lifecycle evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn timeline_panel(
    html: &mut String,
    trace: Option<&TraceReport>,
    observations: &[TraceReportItem],
) {
    html.push_str("<section class=\"panel\"><h2>Run timeline</h2>");
    let items: Vec<&TraceReportItem> = trace
        .map(|trace| trace.items.iter().collect())
        .unwrap_or_else(|| observations.iter().take(12).collect());
    if items.is_empty() {
        html.push_str("<div class=\"empty\">No trace or observation events yet.</div></section>");
        return;
    }
    if let Some(trace) = trace {
        let _ = write!(
            html,
            "<div class=\"hint\">Featured trace: <code>{}</code></div>",
            escape_html(&trace.trace_id)
        );
    }
    html.push_str("<ol class=\"timeline\">");
    for item in items {
        let _ = write!(
            html,
            "<li class=\"event\"><div class=\"time\">{}</div><div class=\"kind\">{}</div><div class=\"summary\">{}</div></li>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</ol></section>");
}

fn is_tool_kind(kind: &str) -> bool {
    kind == "tool.observed" || kind == "pi.tool_call"
}

fn is_agent_kind(kind: &str) -> bool {
    matches!(
        kind,
        "pi.provider_call.started"
            | "pi.message_end"
            | "pi.stream_summary"
            | "pi.turn_end"
            | "pi.agent_end"
            | "pi.authorize"
            | "pi.authorize_error"
    )
}

fn is_skill_context_kind(kind: &str) -> bool {
    kind == "pi.agent_context"
}

fn outcome_pill(kind: &str) -> String {
    let class = if kind.ends_with(".deny") {
        "bad"
    } else if kind.ends_with(".warn") {
        "warn"
    } else {
        "ok"
    };
    format!(
        "<span class=\"pill {class}\">{}</span>",
        escape_html(decision_label(kind))
    )
}

fn event_pill(kind: &str) -> String {
    format!("<span class=\"pill\">{}</span>", escape_html(kind))
}

fn decision_label(kind: &str) -> &'static str {
    if kind.ends_with(".deny") {
        "deny"
    } else if kind.ends_with(".warn") {
        "warn"
    } else if kind.ends_with(".allow") {
        "allow"
    } else {
        "unknown"
    }
}

pub(crate) fn summary_value(summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    summary
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix).map(ToOwned::to_owned))
}

fn formatted_summary_money(summary: &str, key: &str) -> Option<String> {
    summary_value(summary, key).map(|raw| raw.parse::<f64>().map(format_money).unwrap_or(raw))
}

fn decision_budget(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.selected_budget_id.clone())
        .or_else(|| summary_value(&item.summary, "selected_budget"))
        .or_else(|| {
            item.routing
                .as_ref()
                .and_then(|routing| routing.matched_entity.clone())
        })
        .or_else(|| summary_value(&item.summary, "matched_entity"))
}

fn decision_model(item: &TraceReportItem) -> Option<String> {
    summary_value(&item.summary, "model")
}

fn decision_request(item: &TraceReportItem) -> Option<String> {
    summary_value(&item.summary, "request")
}

fn decision_action(item: &TraceReportItem) -> Option<String> {
    summary_value(&item.summary, "action")
}

fn decision_remaining_budget(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.budget_window_remaining_usd)
        .map(format_money)
        .or_else(|| formatted_summary_money(&item.summary, "budget_window_remaining"))
}

fn decision_estimated_cost(item: &TraceReportItem) -> Option<String> {
    formatted_summary_money(&item.summary, "estimated_cost")
}

fn decision_matched_entity(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.matched_entity.clone())
        .or_else(|| summary_value(&item.summary, "matched_entity"))
}

fn decision_model_check(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.model_check.clone())
        .or_else(|| summary_value(&item.summary, "model_check"))
}

fn decision_rejected_budget(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.rejected_budget_id.clone())
        .or_else(|| summary_value(&item.summary, "rejected_budget"))
}

fn decision_rejected_reason(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.rejected_budget_reason.clone())
}

fn decision_is_model_denial(item: &TraceReportItem) -> bool {
    decision_model_check(item).as_deref() == Some("denied")
        && decision_rejected_reason(item)
            .as_deref()
            .is_some_and(|reason| reason.contains("provider/model is not allowed"))
}

pub(crate) fn decision_model_check_label(item: &TraceReportItem) -> Option<String> {
    let raw = decision_model_check(item)?;
    if decision_is_model_denial(item) {
        return Some("blocked by model allowlist".to_owned());
    }
    if let Some(budget) = raw.strip_prefix("allowed:") {
        return Some(format!("allowed on {budget}"));
    }
    Some(raw)
}

fn limit_hit_name(hit: &crate::ledger::DecisionLimitHitReport) -> String {
    if let Some(window_id) = &hit.window_id {
        return match hit.window_mode.as_deref() {
            Some(mode) => format!("{window_id} {mode} limit"),
            None => format!("{window_id} limit"),
        };
    }

    match hit
        .rule_id
        .rsplit('.')
        .next()
        .unwrap_or(hit.rule_id.as_str())
    {
        "context_tokens" => "context limit".to_owned(),
        "request_cost" => "request-cost limit".to_owned(),
        "tool_calls" => "tool-call limit".to_owned(),
        "agent_steps" => "agent-step limit".to_owned(),
        "retries" => "retry limit".to_owned(),
        _ => format!("{} limit", hit.rule_id),
    }
}

fn decision_binding_limit(
    item: &TraceReportItem,
) -> Option<&crate::ledger::DecisionLimitHitReport> {
    item.limit_hits
        .as_deref()
        .and_then(crate::ledger::binding_limit_hit)
}

pub(crate) fn decision_headline(item: &TraceReportItem) -> String {
    let model = decision_model(item).unwrap_or_else(|| "the requested model".to_owned());
    if let Some(hit) = decision_binding_limit(item) {
        let limit_name = limit_hit_name(hit);
        if hit.severity == crate::contract::DecisionSeverity::Deny {
            format!("{model} was blocked by {limit_name}")
        } else if let Some(budget) = decision_budget(item) {
            format!("{model} continued on {budget} under {limit_name}")
        } else {
            format!("{model} continued under {limit_name}")
        }
    } else if decision_action(item).as_deref() == Some("ask") {
        if let Some(budget) = decision_budget(item).or_else(|| decision_rejected_budget(item)) {
            format!("{model} required approval on {budget}")
        } else {
            format!("{model} required approval")
        }
    } else if item.kind.ends_with(".deny") {
        if decision_is_model_denial(item) {
            if let Some(budget) = decision_rejected_budget(item) {
                format!("{model} was blocked by {budget}'s model allowlist")
            } else {
                format!("{model} was blocked by the model allowlist")
            }
        } else if let Some(budget) =
            decision_budget(item).or_else(|| decision_rejected_budget(item))
        {
            format!("{model} was blocked on {budget}")
        } else {
            format!("{model} was blocked")
        }
    } else if item.kind.ends_with(".warn") {
        if let Some(budget) = decision_budget(item) {
            format!("{model} continued on {budget} with a warning")
        } else {
            format!("{model} continued with a warning")
        }
    } else if let Some(budget) = decision_budget(item) {
        format!("{model} was approved on {budget}")
    } else {
        format!("{model} was approved")
    }
}

pub(crate) fn decision_supporting_line(item: &TraceReportItem) -> Option<String> {
    if let Some(hit) = decision_binding_limit(item) {
        return Some(format!(
            "Binding limit: {}. {}",
            limit_hit_name(hit),
            hit.reason
        ));
    }

    if decision_is_model_denial(item) {
        let model = decision_model(item).unwrap_or_else(|| "the requested model".to_owned());
        let mut line = match decision_rejected_budget(item) {
            Some(budget) => format!("Attempted model {model} is not allowed on budget {budget}."),
            None => format!("Attempted model {model} is not allowed by the active budget policy."),
        };
        if item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.as_ref())
            .is_none()
        {
            line.push_str(" No fallback budget could satisfy the request.");
        }
        return Some(line);
    }

    if decision_action(item).as_deref() == Some("ask") {
        let mut line = "Noether required approval before this request could proceed.".to_owned();
        if let Some(reason) = decision_rejected_reason(item) {
            line.push_str(&format!(" {reason}."));
        }
        return Some(line);
    }

    if item.kind.ends_with(".deny")
        && let Some(reason) = decision_rejected_reason(item)
    {
        let mut line = match decision_rejected_budget(item) {
            Some(budget) => format!("Budget {budget} rejected the request: {reason}."),
            None => format!("Noether blocked the request: {reason}."),
        };
        if let Some(remaining) = item
            .routing
            .as_ref()
            .and_then(|routing| routing.budget_window_remaining_usd)
        {
            line.push_str(&format!(
                " Recorded budget-window remaining at evaluation time: {}.",
                format_money(remaining)
            ));
        }
        return Some(line);
    }

    item.routing.as_ref().map(|routing| {
        let mut line = routing.selection_reason.clone().unwrap_or_else(|| {
            "Noether selected the best available budget for this request.".to_owned()
        });
        if let Some(entity) = &routing.matched_entity {
            line.push_str(&format!(" Matched entity: {entity}."));
        }
        if let Some(remaining) = routing.budget_window_remaining_usd {
            line.push_str(&format!(
                " Selected budget-window remaining: {}.",
                format_money(remaining)
            ));
            line.push_str(" Tighter limits can still bind sooner.");
        }
        line
    })
}

fn short_time(item: &TraceReportItem) -> String {
    item.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn token_hint(totals: &UsageTotals) -> String {
    if totals.total_tokens == 0 {
        "no finalized token usage".to_owned()
    } else if totals.input_tokens == 0
        && totals.output_tokens == 0
        && totals.cache_read_tokens == 0
        && totals.cache_write_tokens == 0
    {
        "finalized token total was recorded without an input/output split".to_owned()
    } else {
        format!(
            "{} input / {} output",
            compact_number(totals.input_tokens),
            compact_number(totals.output_tokens)
        )
    }
}

fn latest_decision_hint(item: &TraceReportItem) -> String {
    decision_supporting_line(item).unwrap_or_else(|| decision_headline(item))
}

fn compact_number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn format_money(value: f64) -> String {
    if value == 0.0 {
        "$0".to_owned()
    } else if value < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

fn percent(value: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (value as f64 / total as f64) * 100.0
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
