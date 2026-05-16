use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tokio::fs;

use crate::contract::DecisionMode;
use crate::error::NoetError;
use crate::fixture::{list_fixture_paths, read_fixture};
use crate::ledger::{BudgetLedger, TraceReport, TraceReportItem, UsageReport};
use crate::policy::load_policy;
use crate::proxy::load_proxy_routes;
use crate::redaction::redaction_findings;
use crate::server::{ServeConfig, serve};

#[derive(Parser)]
#[command(name = "noet")]
#[command(about = "Noether control sidecar tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the local capture and decision server.
    Serve(ServeArgs),
    /// Validate and inspect policy files.
    Policy(PolicyCommand),
    /// Inspect captured fixture files.
    Fixtures(FixturesCommand),
    /// Report persisted decisions, usage, traces, and observations.
    Report(ReportCommand),
}

#[derive(Parser)]
struct ServeArgs {
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:4040")]
    bind: SocketAddr,

    /// Directory where redacted capture fixtures are written.
    #[arg(long, default_value = ".noet/fixtures")]
    fixture_dir: PathBuf,

    /// SQLite ledger path for durable local state.
    #[arg(long, default_value = ".noet/noether.sqlite")]
    db_path: PathBuf,

    /// Optional upstream base URL. When omitted, Noether returns mock responses.
    #[arg(long)]
    upstream: Option<url::Url>,

    /// Optional transparent proxy route config YAML.
    #[arg(long)]
    routes: Option<PathBuf>,

    /// Optional policy.noet.yaml file for decisions and capture enforcement.
    #[arg(long)]
    policy: Option<PathBuf>,

    /// Decision mode for capture proxy requests when a policy is configured.
    #[arg(long, value_enum, default_value_t = DecisionMode::DryRun)]
    decision_mode: DecisionMode,
}

#[derive(Parser)]
struct PolicyCommand {
    #[command(subcommand)]
    command: PolicySubcommand,
}

#[derive(Subcommand)]
enum PolicySubcommand {
    /// Parse and validate a policy.noet.yaml file.
    Check { path: PathBuf },
}

#[derive(Parser)]
struct FixturesCommand {
    #[command(subcommand)]
    command: FixturesSubcommand,
}

#[derive(Subcommand)]
enum FixturesSubcommand {
    /// List fixture JSON files in a directory.
    List {
        #[arg(default_value = ".noet/fixtures")]
        dir: PathBuf,
    },
    /// Pretty-print a fixture JSON file.
    Show { path: PathBuf },
    /// Fail if a fixture contains unredacted credential-like JSON keys.
    RedactCheck { path: PathBuf },
}

#[derive(Parser)]
struct ReportCommand {
    /// SQLite ledger path.
    #[arg(long, default_value = ".noet/noether.sqlite")]
    db_path: PathBuf,

    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    json: bool,

    #[command(subcommand)]
    command: ReportSubcommand,
}

#[derive(Subcommand)]
enum ReportSubcommand {
    /// Summarize finalized usage and cost.
    Usage,
    /// List authorization decisions.
    Decisions,
    /// Show one trace story.
    Trace { trace_id: String },
    /// List tool/eval observations.
    Observations {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        trace: Option<String>,
    },
    /// Write a self-contained visual HTML dashboard.
    Dashboard {
        /// Output HTML path.
        #[arg(long, default_value = ".noet/noether-dashboard.html")]
        out: PathBuf,
        /// Trace to feature. Defaults to the latest decision trace when available.
        #[arg(long)]
        trace: Option<String>,
    },
}

pub async fn run() -> Result<(), NoetError> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve(args) => {
            let policy = match args.policy {
                Some(path) => Some(load_policy(&path).await?),
                None => None,
            };
            let routes = match args.routes {
                Some(path) => load_proxy_routes(&path).await?.routes,
                None => Vec::new(),
            };
            serve(ServeConfig {
                bind: args.bind,
                fixture_dir: args.fixture_dir,
                db_path: args.db_path,
                upstream: args.upstream,
                routes,
                policy,
                decision_mode: args.decision_mode,
            })
            .await
        }
        Command::Policy(command) => run_policy(command).await,
        Command::Fixtures(command) => run_fixtures(command).await,
        Command::Report(command) => run_report(command).await,
    }
}

async fn run_policy(command: PolicyCommand) -> Result<(), NoetError> {
    match command.command {
        PolicySubcommand::Check { path } => {
            let policy = load_policy(&path).await?;
            println!(
                "policy ok: version={}, budgets={}, policies={}",
                policy.version,
                policy.budgets.len(),
                policy.policies.len()
            );
            Ok(())
        }
    }
}

async fn run_fixtures(command: FixturesCommand) -> Result<(), NoetError> {
    match command.command {
        FixturesSubcommand::List { dir } => {
            for path in list_fixture_paths(&dir).await? {
                println!("{}", path.display());
            }
            Ok(())
        }
        FixturesSubcommand::Show { path } => {
            let fixture = read_fixture(&path).await?;
            println!("{}", serde_json::to_string_pretty(&fixture)?);
            Ok(())
        }
        FixturesSubcommand::RedactCheck { path } => {
            let bytes = fs::read(&path).await?;
            let value: Value = serde_json::from_slice(&bytes)?;
            let findings = redaction_findings(&value);
            if findings.is_empty() {
                println!("redaction ok: {}", path.display());
                Ok(())
            } else {
                Err(NoetError::InvalidPolicy(format!(
                    "unredacted credential-like keys in {}: {}",
                    path.display(),
                    findings.join(", ")
                )))
            }
        }
    }
}

async fn run_report(command: ReportCommand) -> Result<(), NoetError> {
    let ledger = BudgetLedger::open_sqlite(&command.db_path)?;
    match command.command {
        ReportSubcommand::Usage => {
            let report = ledger.usage_report()?;
            if command.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for line in render_usage_report_lines(&report) {
                    println!("{line}");
                }
            }
        }
        ReportSubcommand::Decisions => {
            print_items(ledger.decisions_report()?, command.json)?;
        }
        ReportSubcommand::Trace { trace_id } => {
            let report = ledger.trace_report(&trace_id)?;
            if command.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("trace\t{}", report.trace_id);
                for item in report.items {
                    println!(
                        "{}\t{}\t{}",
                        item.occurred_at.to_rfc3339(),
                        item.kind,
                        item.summary
                    );
                }
            }
        }
        ReportSubcommand::Observations { kind, trace } => {
            let prefix = match kind.as_deref() {
                Some("tool") => Some("tool."),
                Some("eval") => Some("eval."),
                Some(value) => Some(value),
                None => None,
            };
            print_items(
                ledger.observations_report(prefix, trace.as_deref())?,
                command.json,
            )?;
        }
        ReportSubcommand::Dashboard { out, trace } => {
            let usage = ledger.usage_report()?;
            let decisions = ledger.decisions_report()?;
            let trace_id = trace.or_else(|| {
                decisions
                    .iter()
                    .find_map(|item| summary_value(&item.summary, "trace"))
            });
            let trace_report = trace_id
                .as_deref()
                .map(|trace_id| ledger.trace_report(trace_id))
                .transpose()?;
            let observations = ledger.observations_report(None, trace_id.as_deref())?;
            let html = render_dashboard(&usage, &decisions, trace_report.as_ref(), &observations);
            if let Some(parent) = out.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent).await?;
            }
            fs::write(&out, html).await?;
            println!("dashboard\t{}", out.display());
            if let Some(trace_id) = trace_id {
                println!("featured_trace\t{trace_id}");
            }
        }
    }
    Ok(())
}

fn render_usage_report_lines(report: &UsageReport) -> Vec<String> {
    let mut lines = vec![
        format!("total_cost_usd\t{:.6}", report.total_cost_usd),
        "project\tprovider\tmodel\tsubject\tinput_tokens\toutput_tokens\tcache_read_tokens\tcache_write_tokens\ttotal_tokens\tcost_usd\tcache_read_cost_usd\tcache_write_cost_usd\treservations\tactive\tfinalized".to_owned(),
    ];
    for row in &report.rows {
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{}\t{}\t{}",
            row.project.as_deref().unwrap_or("-"),
            row.provider.as_deref().unwrap_or("-"),
            row.model.as_deref().unwrap_or("-"),
            row.subject.as_deref().unwrap_or("-"),
            row.input_tokens,
            row.output_tokens,
            row.cache_read_tokens,
            row.cache_write_tokens,
            row.total_tokens,
            row.total_cost_usd,
            row.cache_read_cost_usd,
            row.cache_write_cost_usd,
            row.reservations,
            row.active_reservations,
            row.finalized_reservations
        ));
    }
    if let Some(adoption) = &report.protected_adoption {
        lines.push(format!(
            "unused_protected_opportunity_usd\t{:.6}",
            adoption.unused_protected_opportunity_usd
        ));
        lines.push(format!(
            "carryover_liability_usd\t{:.6}",
            adoption.carryover_liability_usd
        ));
        lines.push(
            "adoption_level\tbudget_id\tentity_key\tprotected_amount_usd\tcurrent_grant_usd\tcarryover_usd\tused_current_grant_usd"
                .to_owned(),
        );
        for entity in &adoption.low_adopters {
            lines.push(format!(
                "low\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}",
                entity.budget_id,
                entity.entity_key,
                entity.protected_amount_usd,
                entity.current_grant_usd,
                entity.carryover_usd,
                entity.used_current_grant_usd
            ));
        }
        for entity in &adoption.high_adopters {
            lines.push(format!(
                "high\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}",
                entity.budget_id,
                entity.entity_key,
                entity.protected_amount_usd,
                entity.current_grant_usd,
                entity.carryover_usd,
                entity.used_current_grant_usd
            ));
        }
    }
    lines
}

fn print_items(items: Vec<crate::ledger::TraceReportItem>, json: bool) -> Result<(), NoetError> {
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        println!("occurred_at\tkind\tsummary");
        for item in items {
            println!(
                "{}\t{}\t{}",
                item.occurred_at.to_rfc3339(),
                item.kind,
                item.summary
            );
        }
    }
    Ok(())
}

fn render_dashboard(
    usage: &UsageReport,
    decisions: &[TraceReportItem],
    trace: Option<&TraceReport>,
    observations: &[TraceReportItem],
) -> String {
    let totals = usage_totals(usage);
    let latest_decision = decisions.first();
    let activity = dashboard_activity(trace, observations);
    let tool_count = activity
        .iter()
        .filter(|item| is_tool_kind(&item.kind))
        .count();
    let agent_count = activity
        .iter()
        .filter(|item| is_agent_kind(&item.kind))
        .count();
    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether dashboard</title>");
    html.push_str(
        "<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --good:#22c55e; --warn:#f59e0b; --bad:#ef4444; --blue:#38bdf8; --violet:#a78bfa; }
        * { box-sizing: border-box; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:radial-gradient(circle at top left,#172554,#0f172a 42%); color:var(--text); }
        main { max-width:1180px; margin:0 auto; padding:32px 20px 48px; }
        h1 { margin:0 0 4px; font-size:34px; letter-spacing:-0.04em; }
        h2 { margin:28px 0 12px; font-size:20px; }
        .sub { color:var(--muted); margin-bottom:24px; }
        .grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(210px,1fr)); }
        .card, .panel { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; box-shadow:0 18px 55px rgba(0,0,0,.22); }
        .card { padding:18px; }
        .label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .value { font-size:30px; font-weight:800; margin-top:6px; letter-spacing:-0.03em; }
        .hint { color:var(--muted); margin-top:4px; }
        .panel { padding:18px; margin-top:14px; overflow:hidden; }
        .bar { height:12px; display:flex; overflow:hidden; border-radius:999px; background:#1e293b; margin:10px 0; }
        .in { background:var(--blue); }
        .out { background:var(--violet); }
        .cache { background:var(--warn); }
        .legend { display:flex; gap:16px; flex-wrap:wrap; color:var(--muted); font-size:13px; }
        .dot { display:inline-block; width:9px; height:9px; border-radius:50%; margin-right:6px; }
        .timeline { list-style:none; margin:0; padding:0; }
        .event { display:grid; grid-template-columns:165px 210px 1fr; gap:12px; padding:13px 0; border-top:1px solid var(--line); align-items:start; }
        .event:first-child { border-top:0; }
        .time, .summary { color:var(--muted); }
        .kind { font-weight:700; }
        .pill { display:inline-flex; align-items:center; border-radius:999px; padding:4px 9px; background:#1e293b; border:1px solid var(--line); font-size:13px; }
        .ok { color:var(--good); } .warn { color:var(--warn); } .bad { color:var(--bad); }
        table { width:100%; border-collapse:collapse; }
        th, td { text-align:left; padding:10px 8px; border-top:1px solid var(--line); vertical-align:top; }
        th { color:var(--muted); font-weight:600; font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .empty { color:var(--muted); padding:18px; border:1px dashed var(--line); border-radius:14px; }
        @media (max-width:760px) { .event { grid-template-columns:1fr; gap:2px; } h1 { font-size:28px; } }
        </style>",
    );
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether run dashboard</h1>");
    html.push_str("<div class=\"sub\">Readable local view of decisions, cost, usage, and trace events. Raw hook logs are not needed here.</div>");

    html.push_str("<section class=\"grid\">");
    metric_card(
        &mut html,
        "Spend",
        &format_money(usage.total_cost_usd),
        "finalized local ledger cost",
    );
    metric_card(
        &mut html,
        "Tokens",
        &compact_number(totals.total_tokens),
        &format!(
            "{} input / {} output",
            compact_number(totals.input_tokens),
            compact_number(totals.output_tokens)
        ),
    );
    metric_card(
        &mut html,
        "Reservations",
        &totals.reservations.to_string(),
        &format!(
            "{} finalized, {} active",
            totals.finalized_reservations, totals.active_reservations
        ),
    );
    metric_card(
        &mut html,
        "Latest decision",
        latest_decision
            .map(|item| decision_label(&item.kind))
            .unwrap_or("none"),
        latest_decision
            .map(|item| item.summary.as_str())
            .unwrap_or("no authorization decisions yet"),
    );
    metric_card(
        &mut html,
        "Tools",
        &tool_count.to_string(),
        "tool calls/results observed for the featured run",
    );
    metric_card(
        &mut html,
        "Agent activity",
        &agent_count.to_string(),
        "provider, message, turn, and agent lifecycle events",
    );
    if let Some(adoption) = &usage.protected_adoption {
        metric_card(
            &mut html,
            "Protected opportunity",
            &format_money(adoption.unused_protected_opportunity_usd),
            "remaining current protected grant this window",
        );
        metric_card(
            &mut html,
            "Carryover liability",
            &format_money(adoption.carryover_liability_usd),
            "carryover reserved for future protected use",
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
    }
    html.push_str("</section>");

    token_mix_panel(&mut html, &totals);
    usage_rows_panel(&mut html, usage);
    protected_adoption_panel(&mut html, usage);
    tools_panel(&mut html, &activity);
    agent_activity_panel(&mut html, &activity);
    skill_context_panel(&mut html, &activity);
    decisions_panel(&mut html, decisions);
    risky_runs_panel(&mut html, decisions);
    lifecycle_guardrails_panel(&mut html, trace);
    timeline_panel(&mut html, trace, observations);

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

fn metric_card(html: &mut String, label: &str, value: &str, hint: &str) {
    let _ = write!(
        html,
        "<article class=\"card\"><div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"hint\">{}</div></article>",
        escape_html(label),
        escape_html(value),
        escape_html(hint)
    );
}

fn token_mix_panel(html: &mut String, totals: &UsageTotals) {
    html.push_str("<section class=\"panel\"><h2>Token mix</h2>");
    if totals.total_tokens == 0 {
        html.push_str("<div class=\"empty\">No finalized token usage yet.</div></section>");
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

fn usage_rows_panel(html: &mut String, usage: &UsageReport) {
    html.push_str("<section class=\"panel\"><h2>Where the spend went</h2>");
    if usage.rows.is_empty() {
        html.push_str("<div class=\"empty\">No usage has been finalized yet.</div></section>");
        return;
    }
    html.push_str("<table><thead><tr><th>Project</th><th>Provider / model</th><th>Subject</th><th>Cost</th><th>Tokens</th><th>Status</th></tr></thead><tbody>");
    for row in &usage.rows {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}<br><span class=\"summary\">{}</span></td><td>{}</td><td>{}</td><td>{}</td><td>{} finalized / {} active</td></tr>",
            escape_html(row.project.as_deref().unwrap_or("-")),
            escape_html(row.provider.as_deref().unwrap_or("-")),
            escape_html(row.model.as_deref().unwrap_or("-")),
            escape_html(row.subject.as_deref().unwrap_or("-")),
            format_money(row.total_cost_usd),
            compact_number(row.total_tokens),
            row.finalized_reservations,
            row.active_reservations
        );
    }
    html.push_str("</tbody></table></section>");
}

fn protected_adoption_panel(html: &mut String, usage: &UsageReport) {
    let Some(adoption) = &usage.protected_adoption else {
        return;
    };
    html.push_str("<section class=\"panel\"><h2>Adoption health</h2>");
    let _ = write!(
        html,
        "<div class=\"legend\"><span>Protected opportunity {}</span><span>Carryover liability {}</span><span>Low adopters {}</span><span>Top consumers {}</span></div>",
        format_money(adoption.unused_protected_opportunity_usd),
        format_money(adoption.carryover_liability_usd),
        adoption.low_adopters.len(),
        adoption.high_adopters.len()
    );

    html.push_str("<h3>Low adopters</h3>");
    if adoption.low_adopters.is_empty() {
        html.push_str(
            "<div class=\"empty\">No low adopters were detected in the protected adoption buckets.</div>",
        );
    } else {
        html.push_str("<table><thead><tr><th>Budget</th><th>Entity</th><th>Protected opportunity</th><th>Carryover</th><th>Current usage</th></tr></thead><tbody>");
        for entity in &adoption.low_adopters {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&entity.budget_id),
                escape_html(&entity.entity_key),
                format_money(entity.current_grant_usd),
                format_money(entity.carryover_usd),
                format_money(entity.used_current_grant_usd)
            );
        }
        html.push_str("</tbody></table>");
    }

    html.push_str("<h3>Top consumers</h3>");
    if adoption.high_adopters.is_empty() {
        html.push_str(
            "<div class=\"empty\">No high protected-budget consumers were detected yet.</div>",
        );
    } else {
        html.push_str("<table><thead><tr><th>Budget</th><th>Entity</th><th>Used current grant</th><th>Carryover</th><th>Protected amount</th></tr></thead><tbody>");
        for entity in &adoption.high_adopters {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&entity.budget_id),
                escape_html(&entity.entity_key),
                format_money(entity.used_current_grant_usd),
                format_money(entity.carryover_usd),
                format_money(entity.protected_amount_usd)
            );
        }
        html.push_str("</tbody></table>");
    }
    html.push_str("</section>");
}

fn tools_panel(html: &mut String, activity: &[&TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Tool usage</h2>");
    let tools: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_tool_kind(&item.kind))
        .collect();
    if tools.is_empty() {
        html.push_str(
            "<div class=\"empty\">No tool calls or tool results were observed for this run yet. If Pi did not use tools, this is expected.</div></section>",
        );
        return;
    }
    html.push_str("<table><thead><tr><th>When</th><th>Tool event</th><th>What happened</th></tr></thead><tbody>");
    for item in tools {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn agent_activity_panel(html: &mut String, activity: &[&TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Agent activity</h2>");
    let agent_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_agent_kind(&item.kind))
        .collect();
    if agent_events.is_empty() {
        html.push_str(
            "<div class=\"empty\">No Pi agent lifecycle events were observed yet. This usually means the run came from the vertical demo or Pi did not emit lifecycle hooks for this trace.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Agent event</th><th>Signal</th></tr></thead><tbody>",
    );
    for item in agent_events {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn skill_context_panel(html: &mut String, activity: &[&TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Skills and context</h2>");
    let context_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_skill_context_kind(&item.kind))
        .collect();
    if context_events.is_empty() {
        html.push_str(
            "<div class=\"empty\">No skill/context event was observed yet. When Pi provides agent context, this section will show selected tools, skills, and context-file summaries without prompt text.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Context event</th><th>Summary</th></tr></thead><tbody>",
    );
    for item in context_events {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn decisions_panel(html: &mut String, decisions: &[TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Recent decisions</h2>");
    if decisions.is_empty() {
        html.push_str("<div class=\"empty\">No authorization decisions yet.</div></section>");
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Outcome</th><th>Summary</th></tr></thead><tbody>",
    );
    for item in decisions.iter().take(8) {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn risky_runs_panel(html: &mut String, decisions: &[TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Risky runs</h2>");
    let risky: Vec<_> = decisions
        .iter()
        .filter(|item| {
            item.guard_hits
                .as_ref()
                .is_some_and(|hits| !hits.is_empty())
        })
        .collect();
    if risky.is_empty() {
        html.push_str(
            "<div class=\"empty\">No guard hits were recorded for recent decisions.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Outcome</th><th>Guard hits</th></tr></thead><tbody>",
    );
    for item in risky {
        let hits = item
            .guard_hits
            .as_ref()
            .into_iter()
            .flatten()
            .map(|hit| format!("{}: {}", hit.rule_id, hit.reason))
            .collect::<Vec<_>>()
            .join("; ");
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&hits)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn lifecycle_guardrails_panel(html: &mut String, trace: Option<&TraceReport>) {
    html.push_str("<section class=\"panel\"><h2>Lifecycle guardrails</h2>");
    let items: Vec<&TraceReportItem> = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("guard.report_only."))
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        html.push_str(
            "<div class=\"empty\">No lifecycle-backed report-only guard detections were recorded.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Lifecycle guard</th><th>Detection</th></tr></thead><tbody>",
    );
    for item in items {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
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

fn summary_value(summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    summary
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix).map(ToOwned::to_owned))
}

fn short_time(item: &TraceReportItem) -> String {
    item.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string()
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn dashboard_renders_budget_routing_explanation_markers() {
        let decisions = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.allow".to_owned(),
            summary: "decision_id=dec_1 selected_budget=project-budget matched_entity=project:noether selection_reason=selected fallback budget for project:noether rejected_budget=missing-budget rejected_reason=requested budget does not exist model_check=allowed:project-budget remaining_budget=0.750000".to_owned(),
            routing: None,
            guard_hits: None,
        }];
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };

        let html = render_dashboard(&usage, &decisions, None, &[]);

        assert!(html.contains("selected_budget=project-budget"));
        assert!(html.contains("matched_entity=project:noether"));
        assert!(html.contains("rejected_budget=missing-budget"));
        assert!(html.contains("model_check=allowed:project-budget"));
    }

    #[test]
    fn usage_report_human_output_includes_protected_adoption_summary() {
        let usage = UsageReport {
            total_cost_usd: 30.0,
            rows: Vec::new(),
            protected_adoption: Some(crate::ledger::ProtectedAdoptionReport {
                unused_protected_opportunity_usd: 25.0,
                carryover_liability_usd: 5.0,
                low_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:alice".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 24.0,
                    carryover_usd: 0.0,
                    used_current_grant_usd: 1.0,
                }],
                high_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:bob".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 1.0,
                    carryover_usd: 5.0,
                    used_current_grant_usd: 24.0,
                }],
            }),
        };

        let lines = render_usage_report_lines(&usage);

        assert!(
            lines
                .iter()
                .any(|line| line == "unused_protected_opportunity_usd\t25.000000")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "carryover_liability_usd\t5.000000")
        );
        assert!(lines.iter().any(|line| line.contains("user:alice")));
        assert!(lines.iter().any(|line| line.contains("user:bob")));
    }

    #[test]
    fn dashboard_renders_protected_adoption_sections() {
        let usage = UsageReport {
            total_cost_usd: 30.0,
            rows: Vec::new(),
            protected_adoption: Some(crate::ledger::ProtectedAdoptionReport {
                unused_protected_opportunity_usd: 25.0,
                carryover_liability_usd: 5.0,
                low_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:alice".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 24.0,
                    carryover_usd: 0.0,
                    used_current_grant_usd: 1.0,
                }],
                high_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:bob".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 1.0,
                    carryover_usd: 5.0,
                    used_current_grant_usd: 24.0,
                }],
            }),
        };

        let html = render_dashboard(&usage, &[], None, &[]);

        assert!(html.contains("Protected opportunity"));
        assert!(html.contains("Carryover liability"));
        assert!(html.contains("Low adopters"));
        assert!(html.contains("Top consumers"));
        assert!(html.contains("Adoption health"));
    }

    #[test]
    fn dashboard_renders_risky_run_section_for_guard_hits() {
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };
        let decisions = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.deny".to_owned(),
            summary: "decision_id=dec_guard guard_hits=dev-budget.max_context_tokens".to_owned(),
            routing: None,
            guard_hits: Some(vec![crate::ledger::DecisionGuardHitReport {
                rule_id: "dev-budget.max_context_tokens".to_owned(),
                reason: "estimated context tokens 1200 exceed enforced guard max 1000".to_owned(),
                severity: crate::contract::DecisionSeverity::Deny,
            }]),
        }];

        let html = render_dashboard(&usage, &decisions, None, &[]);

        assert!(html.contains("Risky runs"));
        assert!(html.contains("dev-budget.max_context_tokens"));
    }

    #[test]
    fn dashboard_renders_lifecycle_guardrail_section() {
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };
        let trace = TraceReport {
            trace_id: "trace-lifecycle".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "guard.report_only.tool_calls".to_owned(),
                summary: "tool_calls=12 max_tool_calls=10 reporting_only=true source=pi.tool_call"
                    .to_owned(),
                routing: None,
                guard_hits: None,
            }],
        };

        let html = render_dashboard(&usage, &[], Some(&trace), &[]);

        assert!(html.contains("Lifecycle guardrails"));
        assert!(html.contains("guard.report_only.tool_calls"));
    }
}
