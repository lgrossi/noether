use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::Serialize;

use crate::error::NoetError;
use crate::ledger::{
    BudgetLedger, ProtectedAdoptionEntityReport, ProtectedAdoptionReport, TraceReport,
    TraceReportItem, UsageActivityRecord, UsageReport,
};
use crate::reporting::{self, DashboardDecisionStats};

#[derive(Debug, Clone, Copy)]
pub struct DashboardViewQuery<'a> {
    pub window: Option<&'a str>,
    pub lens: Option<&'a str>,
    pub entity: Option<&'a str>,
    pub trace: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardWindowOption {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardLensOption {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardEntityOption {
    pub value: String,
    pub label: String,
    pub spend_usd: f64,
    pub matched_records: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardTraceFilterOption {
    pub trace_id: String,
    pub spend_usd: f64,
    pub total_tokens: u64,
    pub latest_at: DateTime<Utc>,
    pub badges: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardFilterModel {
    pub selected_window: String,
    pub selected_lens: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_trace: Option<String>,
    pub windows: Vec<DashboardWindowOption>,
    pub lenses: Vec<DashboardLensOption>,
    pub entities: Vec<DashboardEntityOption>,
    pub traces: Vec<DashboardTraceFilterOption>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardHero {
    pub title: String,
    pub summary: String,
    pub badges: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardKpiCard {
    pub id: &'static str,
    pub label: &'static str,
    pub value: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    pub tone: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardSeriesPoint {
    pub label: String,
    pub spend_usd: f64,
    pub total_tokens: u64,
    pub decisions: u64,
    pub traces: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardBreakdownRow {
    pub label: String,
    pub spend_usd: f64,
    pub total_tokens: u64,
    pub traces: u64,
    pub share_pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardInsightCard {
    pub title: String,
    pub value: String,
    pub summary: String,
    pub tone: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardOverviewData {
    pub filters: DashboardFilterModel,
    pub hero: DashboardHero,
    pub kpis: Vec<DashboardKpiCard>,
    pub spend_trend: Vec<DashboardSeriesPoint>,
    pub spend_distribution: Vec<DashboardBreakdownRow>,
    pub model_mix: Vec<DashboardBreakdownRow>,
    pub policy: DashboardDecisionStats,
    pub insights: Vec<DashboardInsightCard>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardBudgetCard {
    pub budget_id: String,
    pub spend_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_capacity_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_remaining_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressure_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_delta_usd: Option<f64>,
    pub decision_count: u64,
    pub limit_hits: u64,
    pub peak_day_share: f64,
    pub behavior_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardBudgetSeries {
    pub budget_id: String,
    pub points: Vec<DashboardSeriesPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardBudgetsData {
    pub filters: DashboardFilterModel,
    pub budgets: Vec<DashboardBudgetCard>,
    pub budget_trends: Vec<DashboardBudgetSeries>,
    pub concentration: Vec<DashboardBreakdownRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected_adoption: Option<ProtectedAdoptionReport>,
    pub insights: Vec<DashboardInsightCard>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardAdoptionEntityRow {
    pub entity: String,
    pub spend_usd: f64,
    pub total_tokens: u64,
    pub traces: u64,
    pub cache_ratio: f64,
    pub tool_events: u64,
    pub limit_hits: u64,
    pub health_label: String,
    pub opportunity_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardAdoptionData {
    pub filters: DashboardFilterModel,
    pub hero: DashboardHero,
    pub summary_cards: Vec<DashboardKpiCard>,
    pub leaderboard: Vec<DashboardAdoptionEntityRow>,
    pub low_adopters: Vec<ProtectedAdoptionEntityReport>,
    pub high_adopters: Vec<ProtectedAdoptionEntityReport>,
    pub insights: Vec<DashboardInsightCard>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardTraceListItem {
    pub trace_id: String,
    pub spend_usd: f64,
    pub total_tokens: u64,
    pub decisions: u64,
    pub tool_events: u64,
    pub limit_hits: u64,
    pub latest_at: DateTime<Utc>,
    pub badges: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardTraceEvent {
    pub occurred_at: DateTime<Utc>,
    pub kind: String,
    pub category: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardInteropNode {
    pub id: String,
    pub label: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardTraceExplorerData {
    pub filters: DashboardFilterModel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_trace_id: Option<String>,
    pub hero: DashboardHero,
    pub traces: Vec<DashboardTraceListItem>,
    pub timeline: Vec<DashboardTraceEvent>,
    pub interop: Vec<DashboardInteropNode>,
    pub policy: DashboardDecisionStats,
    pub insights: Vec<DashboardInsightCard>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DashboardLens {
    Project,
    User,
    Team,
    Company,
    Workflow,
    Surface,
    Budget,
    Model,
}

impl DashboardLens {
    fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("project") {
            "user" => Self::User,
            "team" => Self::Team,
            "company" => Self::Company,
            "workflow" => Self::Workflow,
            "surface" => Self::Surface,
            "budget" => Self::Budget,
            "model" => Self::Model,
            _ => Self::Project,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
            Self::Team => "team",
            Self::Company => "company",
            Self::Workflow => "workflow",
            Self::Surface => "surface",
            Self::Budget => "budget",
            Self::Model => "model",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::User => "User",
            Self::Team => "Team",
            Self::Company => "Company / org",
            Self::Workflow => "Workflow",
            Self::Surface => "Surface",
            Self::Budget => "Budget / bucket",
            Self::Model => "Model / provider",
        }
    }

    fn options() -> Vec<DashboardLensOption> {
        [
            Self::Project,
            Self::User,
            Self::Team,
            Self::Company,
            Self::Workflow,
            Self::Surface,
            Self::Budget,
            Self::Model,
        ]
        .into_iter()
        .map(|lens| DashboardLensOption {
            id: lens.id(),
            label: lens.label(),
        })
        .collect()
    }
}

#[derive(Debug, Clone, Copy)]
struct DashboardWindow {
    id: &'static str,
    days: Option<i64>,
}

impl DashboardWindow {
    fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("30d") {
            "7d" => Self {
                id: "7d",
                days: Some(7),
            },
            "current" | "30d" => Self {
                id: "30d",
                days: Some(30),
            },
            "90d" => Self {
                id: "90d",
                days: Some(90),
            },
            "all" => Self {
                id: "all",
                days: None,
            },
            _ => Self {
                id: "30d",
                days: Some(30),
            },
        }
    }

    fn options() -> Vec<DashboardWindowOption> {
        vec![
            DashboardWindowOption {
                id: "7d",
                label: "Last 7 days",
            },
            DashboardWindowOption {
                id: "30d",
                label: "Last 30 days",
            },
            DashboardWindowOption {
                id: "90d",
                label: "Last 90 days",
            },
            DashboardWindowOption {
                id: "all",
                label: "All observed data",
            },
        ]
    }
}

struct DashboardContext {
    filters: DashboardFilterModel,
    usage: Vec<UsageActivityRecord>,
    decisions: Vec<TraceReportItem>,
    observations: Vec<TraceReportItem>,
    traces: Vec<DashboardTraceListItem>,
    usage_report: UsageReport,
    selected_trace_id: Option<String>,
    selected_trace: Option<TraceReport>,
}

pub fn filters(
    ledger: &BudgetLedger,
    query: DashboardViewQuery<'_>,
) -> Result<DashboardFilterModel, NoetError> {
    Ok(build_context(ledger, query)?.filters)
}

pub fn overview(
    ledger: &BudgetLedger,
    query: DashboardViewQuery<'_>,
) -> Result<DashboardOverviewData, NoetError> {
    let ctx = build_context(ledger, query)?;
    let policy = decision_stats(&ctx.decisions, ctx.selected_trace.as_ref());
    let spend_trend = series_points(&ctx.usage, &ctx.decisions);
    let spend_distribution = breakdown_for_lens(
        &ctx.usage,
        DashboardLens::parse(Some(&ctx.filters.selected_lens)),
    );
    let model_mix = breakdown_for_lens(&ctx.usage, DashboardLens::Model);
    let budget_cards = budget_cards(&ctx.usage, &ctx.decisions, &ctx.filters.selected_window);
    let total_spend = ctx.usage.iter().map(|row| row.cost_usd).sum::<f64>();
    let total_tokens = ctx.usage.iter().map(|row| row.total_tokens).sum::<u64>();
    let cache_tokens = ctx
        .usage
        .iter()
        .map(|row| row.cache_read_tokens + row.cache_write_tokens)
        .sum::<u64>();
    let cache_ratio = ratio(cache_tokens as f64, total_tokens as f64);
    let capacity = budget_cards
        .iter()
        .filter_map(|card| card.observed_capacity_usd)
        .sum::<f64>();
    let projected_delta = budget_cards
        .iter()
        .filter_map(|card| card.projected_delta_usd)
        .sum::<f64>();
    let hero = DashboardHero {
        title: match ctx.filters.selected_entity.as_deref() {
            Some(entity) => format!("Observed AI work for {entity}"),
            None => "Observed AI work across the selected window".to_owned(),
        },
        summary: if ctx.traces.is_empty() {
            "Noether has not finalized usage or trace evidence in the current slice yet.".to_owned()
        } else {
            format!(
                "{} traces produced {} decisions and {} finalized spend.",
                ctx.traces.len(),
                ctx.decisions.len(),
                money(total_spend)
            )
        },
        badges: vec![
            format!("lens {}", ctx.filters.selected_lens),
            format!("window {}", ctx.filters.selected_window),
            format!("traces {}", ctx.traces.len()),
        ],
    };
    let mut insights = Vec::new();
    if let Some(top) = spend_distribution.first() {
        insights.push(DashboardInsightCard {
            title: "Top spend concentration".to_owned(),
            value: money(top.spend_usd),
            summary: format!(
                "{} accounts for {:.0}% of observed spend in this slice.",
                top.label, top.share_pct
            ),
            tone: "accent",
            trace_id: None,
        });
    }
    if let Some(riskiest) = ctx
        .traces
        .iter()
        .max_by_key(|trace| trace.limit_hits)
        .filter(|trace| trace.limit_hits > 0)
    {
        insights.push(DashboardInsightCard {
            title: "Limit pressure".to_owned(),
            value: riskiest.limit_hits.to_string(),
            summary: format!(
                "{} surfaced the highest limit-hit count.",
                riskiest.trace_id
            ),
            tone: "warn",
            trace_id: Some(riskiest.trace_id.clone()),
        });
    }
    if let Some(adoption) = &ctx.usage_report.protected_adoption {
        insights.push(DashboardInsightCard {
            title: "Protected opportunity".to_owned(),
            value: money(adoption.unused_protected_opportunity_usd),
            summary: format!(
                "{} low adopters still have visible protected room.",
                adoption.low_adopters.len()
            ),
            tone: "good",
            trace_id: None,
        });
    }

    Ok(DashboardOverviewData {
        filters: ctx.filters,
        hero,
        kpis: vec![
            DashboardKpiCard {
                id: "spend",
                label: "Spend to date",
                value: money(total_spend),
                detail: "Finalized observed spend in the selected slice.".to_owned(),
                delta: if capacity > 0.0 {
                    Some(format!(
                        "{:.0}% of observed capacity",
                        ratio(total_spend, capacity)
                    ))
                } else {
                    None
                },
                tone: "accent",
            },
            DashboardKpiCard {
                id: "pace",
                label: "Budget pace",
                value: if capacity > 0.0 {
                    money(projected_delta.abs())
                } else {
                    "No cap".to_owned()
                },
                detail: if projected_delta > 0.0 {
                    "Projected over observed capacity at the current run rate.".to_owned()
                } else if projected_delta < 0.0 {
                    "Projected under observed capacity at the current run rate.".to_owned()
                } else {
                    "No observed budget-capacity signal is available yet.".to_owned()
                },
                delta: None,
                tone: if projected_delta > 0.0 {
                    "warn"
                } else {
                    "good"
                },
            },
            DashboardKpiCard {
                id: "adoption",
                label: "Adoption posture",
                value: match &ctx.usage_report.protected_adoption {
                    Some(adoption) => format!(
                        "{} low · {} high",
                        adoption.low_adopters.len(),
                        adoption.high_adopters.len()
                    ),
                    None => format!("{} active entities", spend_distribution.len()),
                },
                detail: "Concentration and low-adoption signals across the current slice."
                    .to_owned(),
                delta: None,
                tone: "good",
            },
            DashboardKpiCard {
                id: "cache",
                label: "Cache efficiency",
                value: format!("{cache_ratio:.0}%"),
                detail: "Share of observed tokens served through cache read/write accounting."
                    .to_owned(),
                delta: Some(format!("{} cached tokens", compact_number(cache_tokens))),
                tone: if cache_ratio < 5.0 { "warn" } else { "accent" },
            },
        ],
        spend_trend,
        spend_distribution,
        model_mix,
        policy,
        insights,
    })
}

pub fn budgets(
    ledger: &BudgetLedger,
    query: DashboardViewQuery<'_>,
) -> Result<DashboardBudgetsData, NoetError> {
    let ctx = build_context(ledger, query)?;
    let budgets = budget_cards(&ctx.usage, &ctx.decisions, &ctx.filters.selected_window);
    let mut budget_trends = Vec::new();
    for budget_id in budgets.iter().map(|budget| budget.budget_id.clone()) {
        let usage: Vec<_> = ctx
            .usage
            .iter()
            .filter(|row| row.selected_budget_id.as_deref() == Some(budget_id.as_str()))
            .cloned()
            .collect();
        let decisions: Vec<_> = ctx
            .decisions
            .iter()
            .filter(|item| {
                item.routing
                    .as_ref()
                    .and_then(|routing| routing.selected_budget_id.as_deref())
                    == Some(budget_id.as_str())
            })
            .cloned()
            .collect();
        budget_trends.push(DashboardBudgetSeries {
            budget_id,
            points: series_points(&usage, &decisions),
        });
    }
    let concentration = breakdown_for_lens(&ctx.usage, DashboardLens::Budget);
    let mut insights = Vec::new();
    if let Some(peak) = budgets.first() {
        insights.push(DashboardInsightCard {
            title: "Most pressured budget".to_owned(),
            value: peak.budget_id.clone(),
            summary: format!(
                "{} has {} limit hits and a {} usage shape.",
                peak.budget_id, peak.limit_hits, peak.behavior_label
            ),
            tone: if peak.limit_hits > 0 {
                "warn"
            } else {
                "accent"
            },
            trace_id: None,
        });
    }
    Ok(DashboardBudgetsData {
        filters: ctx.filters,
        budgets,
        budget_trends,
        concentration,
        protected_adoption: ctx.usage_report.protected_adoption,
        insights,
    })
}

pub fn adoption(
    ledger: &BudgetLedger,
    query: DashboardViewQuery<'_>,
) -> Result<DashboardAdoptionData, NoetError> {
    let ctx = build_context(ledger, query)?;
    let lens = DashboardLens::parse(Some(&ctx.filters.selected_lens));
    let leaderboard = adoption_rows(&ctx.usage, &ctx.decisions, &ctx.observations, lens);
    let usage = &ctx.usage_report;
    let adoption_report = usage.protected_adoption.clone();
    let hero = DashboardHero {
        title: "Adoption quality and coaching signals".to_owned(),
        summary: "This view highlights underuse, concentration, cache/tool behavior, and which entities need enablement help.".to_owned(),
        badges: vec![
            format!("lens {}", ctx.filters.selected_lens),
            format!("entities {}", leaderboard.len()),
        ],
    };
    let mut insights = Vec::new();
    if let Some(first) = leaderboard.first() {
        insights.push(DashboardInsightCard {
            title: "Most active entity".to_owned(),
            value: first.entity.clone(),
            summary: format!(
                "{} traces, {:.0}% cache ratio, {} limit hits.",
                first.traces, first.cache_ratio, first.limit_hits
            ),
            tone: "accent",
            trace_id: None,
        });
    }
    if let Some(adoption) = &adoption_report {
        if let Some(low) = adoption.low_adopters.first() {
            insights.push(DashboardInsightCard {
                title: "Enablement opportunity".to_owned(),
                value: low.entity_key.clone(),
                summary: format!(
                    "{} still has {} of protected room available.",
                    low.entity_key,
                    money(low.current_grant_usd)
                ),
                tone: "good",
                trace_id: None,
            });
        }
    }

    Ok(DashboardAdoptionData {
        filters: ctx.filters,
        hero,
        summary_cards: vec![
            DashboardKpiCard {
                id: "entities",
                label: "Observed entities",
                value: leaderboard.len().to_string(),
                detail: "Entities with finalized usage in the selected slice.".to_owned(),
                delta: None,
                tone: "accent",
            },
            DashboardKpiCard {
                id: "opportunity",
                label: "Protected opportunity",
                value: adoption_report
                    .as_ref()
                    .map(|report| money(report.unused_protected_opportunity_usd))
                    .unwrap_or_else(|| "No pool".to_owned()),
                detail: if adoption_report.is_some() {
                    "Current protected budget still available for low adopters.".to_owned()
                } else {
                    "No protected adoption pool is configured in the selected slice.".to_owned()
                },
                delta: None,
                tone: "good",
            },
            DashboardKpiCard {
                id: "carryover",
                label: "Carryover liability",
                value: adoption_report
                    .as_ref()
                    .map(|report| money(report.carryover_liability_usd))
                    .unwrap_or_else(|| "No pool".to_owned()),
                detail: if adoption_report.is_some() {
                    "Protected carryover that can roll into the next allocation window.".to_owned()
                } else {
                    "No protected adoption carryover is present in the selected slice.".to_owned()
                },
                delta: None,
                tone: "warn",
            },
            DashboardKpiCard {
                id: "tooling",
                label: "Tool-heavy entities",
                value: leaderboard
                    .iter()
                    .filter(|row| row.tool_events > row.traces.saturating_mul(3))
                    .count()
                    .to_string(),
                detail: "Entities whose traces are especially tool-dense in this slice.".to_owned(),
                delta: None,
                tone: "accent",
            },
        ],
        leaderboard,
        low_adopters: adoption_report
            .as_ref()
            .map(|report| report.low_adopters.clone())
            .unwrap_or_default(),
        high_adopters: adoption_report
            .as_ref()
            .map(|report| report.high_adopters.clone())
            .unwrap_or_default(),
        insights,
    })
}

pub fn traces(
    ledger: &BudgetLedger,
    query: DashboardViewQuery<'_>,
) -> Result<DashboardTraceExplorerData, NoetError> {
    let ctx = build_context(ledger, query)?;
    let selected_trace_id = ctx
        .selected_trace_id
        .clone()
        .or_else(|| ctx.traces.first().map(|trace| trace.trace_id.clone()));
    let selected_trace = selected_trace_id
        .as_deref()
        .map(|trace_id| ledger.trace_report(trace_id))
        .transpose()?;
    let timeline_items: Vec<DashboardTraceEvent> = selected_trace
        .as_ref()
        .map(|trace| {
            trace
                .items
                .iter()
                .map(|item| DashboardTraceEvent {
                    occurred_at: item.occurred_at,
                    kind: item.kind.clone(),
                    category: event_category(&item.kind).to_owned(),
                    summary: item.summary.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let interop = interop_nodes(selected_trace.as_ref());
    let policy = decision_stats(
        &selected_trace
            .as_ref()
            .map(|trace| {
                trace
                    .items
                    .iter()
                    .filter(|item| item.kind.starts_with("decision."))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        selected_trace.as_ref(),
    );
    let hero = if let Some(trace_id) = &selected_trace_id {
        DashboardHero {
            title: format!("Trace explorer for {trace_id}"),
            summary: "Follow the correlation chain from policy decision through usage, tools, agent lifecycle, and observed events.".to_owned(),
            badges: vec![
                format!("events {}", timeline_items.len()),
                format!("systems {}", interop.len()),
            ],
        }
    } else {
        DashboardHero {
            title: "Trace explorer".to_owned(),
            summary: "No trace is available in the current slice yet.".to_owned(),
            badges: Vec::new(),
        }
    };
    let mut insights = Vec::new();
    if let Some(trace) = ctx.traces.first() {
        insights.push(DashboardInsightCard {
            title: "Most expensive trace".to_owned(),
            value: trace.trace_id.clone(),
            summary: format!(
                "{} across {} tokens and {} decisions.",
                money(trace.spend_usd),
                compact_number(trace.total_tokens),
                trace.decisions
            ),
            tone: "accent",
            trace_id: Some(trace.trace_id.clone()),
        });
    }

    Ok(DashboardTraceExplorerData {
        filters: ctx.filters,
        selected_trace_id,
        hero,
        traces: ctx.traces,
        timeline: timeline_items,
        interop,
        policy,
        insights,
    })
}

fn build_context(
    ledger: &BudgetLedger,
    query: DashboardViewQuery<'_>,
) -> Result<DashboardContext, NoetError> {
    let window = DashboardWindow::parse(query.window);
    let lens = DashboardLens::parse(query.lens);
    let usage_report = ledger.usage_report()?;
    let usage_all = ledger.usage_activity_report()?;
    let decisions_all = ledger.decisions_report()?;
    let observations_all =
        ledger.observations_report(reporting::observation_kind_prefix(None), None)?;
    let now = latest_observed_at(&usage_all, &decisions_all, &observations_all);
    let since = window.days.map(|days| now - Duration::days(days));

    let usage_window: Vec<_> = usage_all
        .into_iter()
        .filter(|item| since.is_none_or(|since| item.occurred_at >= since))
        .collect();
    let decisions_window: Vec<_> = decisions_all
        .into_iter()
        .filter(|item| since.is_none_or(|since| item.occurred_at >= since))
        .collect();
    let observations_window: Vec<_> = observations_all
        .into_iter()
        .filter(|item| since.is_none_or(|since| item.occurred_at >= since))
        .collect();

    let entities = entity_options(lens, &usage_window, &decisions_window);
    let selected_entity = query
        .entity
        .filter(|entity| entities.iter().any(|option| option.value == *entity))
        .map(ToOwned::to_owned);
    let mut trace_scope = selected_entity.as_deref().map_or_else(
        || trace_scope_for_lens(lens, &usage_window, &decisions_window),
        |entity| trace_scope_for_entity(lens, entity, &usage_window, &decisions_window),
    );
    if let Some(trace_id) = query.trace {
        trace_scope.insert(trace_id.to_owned());
    }

    let usage_entity: Vec<_> = usage_window
        .into_iter()
        .filter(|item| {
            selected_entity.as_deref().map_or_else(
                || matches_usage_lens(item, lens),
                |entity| matches_usage_entity(item, lens, entity),
            ) || item
                .trace_id
                .as_ref()
                .is_some_and(|trace_id| trace_scope.contains(trace_id))
        })
        .collect();
    let decisions_entity: Vec<_> = decisions_window
        .into_iter()
        .filter(|item| {
            selected_entity.as_deref().map_or_else(
                || matches_decision_lens(item, lens),
                |entity| matches_decision_entity(item, lens, entity),
            ) || item
                .trace_id
                .as_ref()
                .is_some_and(|trace_id| trace_scope.contains(trace_id))
        })
        .collect();
    let observations_entity: Vec<_> = observations_window
        .into_iter()
        .filter(|item| {
            item.trace_id
                .as_ref()
                .is_some_and(|trace_id| trace_scope.contains(trace_id))
        })
        .collect();

    let traces = trace_list_items(&usage_entity, &decisions_entity, &observations_entity);
    let selected_trace = query
        .trace
        .filter(|trace| traces.iter().any(|option| option.trace_id == *trace))
        .map(ToOwned::to_owned);

    let usage = usage_entity
        .into_iter()
        .filter(|item| {
            selected_trace
                .as_deref()
                .is_none_or(|trace_id| item.trace_id.as_deref() == Some(trace_id))
        })
        .collect();
    let decisions = decisions_entity
        .into_iter()
        .filter(|item| {
            selected_trace
                .as_deref()
                .is_none_or(|trace_id| item.trace_id.as_deref() == Some(trace_id))
        })
        .collect();
    let observations = observations_entity
        .into_iter()
        .filter(|item| {
            selected_trace
                .as_deref()
                .is_none_or(|trace_id| item.trace_id.as_deref() == Some(trace_id))
        })
        .collect();

    let filters = DashboardFilterModel {
        selected_window: window.id.to_owned(),
        selected_lens: lens.id().to_owned(),
        selected_entity,
        selected_trace: selected_trace.clone(),
        windows: DashboardWindow::options(),
        lenses: DashboardLens::options(),
        entities,
        traces: traces
            .iter()
            .map(|trace| DashboardTraceFilterOption {
                trace_id: trace.trace_id.clone(),
                spend_usd: trace.spend_usd,
                total_tokens: trace.total_tokens,
                latest_at: trace.latest_at,
                badges: trace.badges.clone(),
            })
            .collect(),
    };

    let selected_trace_report = selected_trace
        .as_deref()
        .map(|trace_id| ledger.trace_report(trace_id))
        .transpose()?;

    Ok(DashboardContext {
        filters,
        usage,
        decisions,
        observations,
        traces,
        usage_report,
        selected_trace_id: selected_trace,
        selected_trace: selected_trace_report,
    })
}

fn latest_observed_at(
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
    observations: &[TraceReportItem],
) -> DateTime<Utc> {
    usage
        .iter()
        .map(|item| item.occurred_at)
        .chain(decisions.iter().map(|item| item.occurred_at))
        .chain(observations.iter().map(|item| item.occurred_at))
        .max()
        .unwrap_or_else(Utc::now)
}

fn entity_options(
    lens: DashboardLens,
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
) -> Vec<DashboardEntityOption> {
    let mut map: HashMap<String, (f64, u64)> = HashMap::new();
    for item in usage {
        if let Some(value) = lens_value_from_usage(item, lens) {
            let entry = map.entry(value).or_insert((0.0, 0));
            entry.0 += item.cost_usd;
            entry.1 += 1;
        }
    }
    for item in decisions {
        if let Some(value) = lens_value_from_decision(item, lens) {
            let entry = map.entry(value).or_insert((0.0, 0));
            entry.1 += 1;
        }
    }
    let mut options: Vec<_> = map
        .into_iter()
        .map(
            |(value, (spend_usd, matched_records))| DashboardEntityOption {
                label: prettify_entity_label(&value),
                value,
                spend_usd,
                matched_records,
            },
        )
        .collect();
    options.sort_by(|left, right| {
        right
            .spend_usd
            .total_cmp(&left.spend_usd)
            .then_with(|| right.matched_records.cmp(&left.matched_records))
            .then_with(|| left.label.cmp(&right.label))
    });
    options.truncate(16);
    options
}

fn trace_scope_for_entity(
    lens: DashboardLens,
    entity: &str,
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
) -> HashSet<String> {
    let mut trace_ids = HashSet::new();
    for item in usage {
        if matches_usage_entity(item, lens, entity)
            && let Some(trace_id) = &item.trace_id
        {
            trace_ids.insert(trace_id.clone());
        }
    }
    for item in decisions {
        if matches_decision_entity(item, lens, entity)
            && let Some(trace_id) = &item.trace_id
        {
            trace_ids.insert(trace_id.clone());
        }
    }
    trace_ids
}

fn trace_scope_for_lens(
    lens: DashboardLens,
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
) -> HashSet<String> {
    let mut trace_ids = HashSet::new();
    for item in usage {
        if matches_usage_lens(item, lens)
            && let Some(trace_id) = &item.trace_id
        {
            trace_ids.insert(trace_id.clone());
        }
    }
    for item in decisions {
        if matches_decision_lens(item, lens)
            && let Some(trace_id) = &item.trace_id
        {
            trace_ids.insert(trace_id.clone());
        }
    }
    trace_ids
}

fn trace_list_items(
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
    observations: &[TraceReportItem],
) -> Vec<DashboardTraceListItem> {
    #[derive(Default)]
    struct Acc {
        spend_usd: f64,
        total_tokens: u64,
        decisions: u64,
        tool_events: u64,
        limit_hits: u64,
        latest_at: Option<DateTime<Utc>>,
    }

    let mut map: HashMap<String, Acc> = HashMap::new();
    for item in usage {
        let Some(trace_id) = &item.trace_id else {
            continue;
        };
        let entry = map.entry(trace_id.clone()).or_default();
        entry.spend_usd += item.cost_usd;
        entry.total_tokens += item.total_tokens;
        entry.latest_at = Some(
            entry
                .latest_at
                .map_or(item.occurred_at, |latest| latest.max(item.occurred_at)),
        );
    }
    for item in decisions {
        let Some(trace_id) = &item.trace_id else {
            continue;
        };
        let entry = map.entry(trace_id.clone()).or_default();
        entry.decisions += 1;
        entry.limit_hits += item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0);
        entry.latest_at = Some(
            entry
                .latest_at
                .map_or(item.occurred_at, |latest| latest.max(item.occurred_at)),
        );
    }
    for item in observations {
        let Some(trace_id) = &item.trace_id else {
            continue;
        };
        let entry = map.entry(trace_id.clone()).or_default();
        if is_tool_kind(&item.kind) {
            entry.tool_events += 1;
        }
        entry.latest_at = Some(
            entry
                .latest_at
                .map_or(item.occurred_at, |latest| latest.max(item.occurred_at)),
        );
    }

    let mut items: Vec<_> = map
        .into_iter()
        .map(|(trace_id, acc)| {
            let mut badges = Vec::new();
            if acc.limit_hits > 0 {
                badges.push(format!("{} limit hits", acc.limit_hits));
            }
            if acc.tool_events > 0 {
                badges.push(format!("{} tools", acc.tool_events));
            }
            if acc.spend_usd > 0.0 {
                badges.push(money(acc.spend_usd));
            }
            DashboardTraceListItem {
                trace_id,
                spend_usd: acc.spend_usd,
                total_tokens: acc.total_tokens,
                decisions: acc.decisions,
                tool_events: acc.tool_events,
                limit_hits: acc.limit_hits,
                latest_at: acc.latest_at.unwrap_or_else(Utc::now),
                badges,
            }
        })
        .collect();
    items.sort_by(|left, right| {
        right
            .spend_usd
            .total_cmp(&left.spend_usd)
            .then_with(|| right.latest_at.cmp(&left.latest_at))
            .then_with(|| left.trace_id.cmp(&right.trace_id))
    });
    items
}

fn breakdown_for_lens(
    usage: &[UsageActivityRecord],
    lens: DashboardLens,
) -> Vec<DashboardBreakdownRow> {
    #[derive(Default)]
    struct Acc {
        spend_usd: f64,
        total_tokens: u64,
        trace_ids: HashSet<String>,
    }
    let mut map: BTreeMap<String, Acc> = BTreeMap::new();
    for item in usage {
        let Some(label) = lens_value_from_usage(item, lens) else {
            continue;
        };
        let entry = map.entry(label).or_default();
        entry.spend_usd += item.cost_usd;
        entry.total_tokens += item.total_tokens;
        if let Some(trace_id) = &item.trace_id {
            entry.trace_ids.insert(trace_id.clone());
        }
    }
    let total_spend = map.values().map(|acc| acc.spend_usd).sum::<f64>();
    let mut rows: Vec<_> = map
        .into_iter()
        .map(|(label, acc)| DashboardBreakdownRow {
            label: prettify_entity_label(&label),
            spend_usd: acc.spend_usd,
            total_tokens: acc.total_tokens,
            traces: acc.trace_ids.len() as u64,
            share_pct: ratio(acc.spend_usd, total_spend),
            note: None,
        })
        .collect();
    rows.sort_by(|left, right| right.spend_usd.total_cmp(&left.spend_usd));
    rows.truncate(8);
    rows
}

fn budget_cards(
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
    selected_window: &str,
) -> Vec<DashboardBudgetCard> {
    #[derive(Default)]
    struct BudgetAcc {
        spend_usd: f64,
        decision_count: u64,
        limit_hits: u64,
        latest_remaining: Option<f64>,
        by_day: BTreeMap<NaiveDate, f64>,
        earliest: Option<DateTime<Utc>>,
    }

    let mut map: BTreeMap<String, BudgetAcc> = BTreeMap::new();
    for item in usage {
        let Some(budget_id) = &item.selected_budget_id else {
            continue;
        };
        let entry = map.entry(budget_id.clone()).or_default();
        entry.spend_usd += item.cost_usd;
        *entry
            .by_day
            .entry(item.occurred_at.date_naive())
            .or_insert(0.0) += item.cost_usd;
        entry.earliest = Some(
            entry
                .earliest
                .map_or(item.occurred_at, |earliest| earliest.min(item.occurred_at)),
        );
    }
    for item in decisions {
        let Some(budget_id) = item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.clone())
        else {
            continue;
        };
        let entry = map.entry(budget_id).or_default();
        entry.decision_count += 1;
        entry.limit_hits += item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0);
        if entry.latest_remaining.is_none() {
            entry.latest_remaining = item
                .routing
                .as_ref()
                .and_then(|routing| routing.budget_window_remaining_usd);
        }
        entry.earliest = Some(
            entry
                .earliest
                .map_or(item.occurred_at, |earliest| earliest.min(item.occurred_at)),
        );
    }

    let window_days = DashboardWindow::parse(Some(selected_window))
        .days
        .unwrap_or(30) as f64;
    let now = usage
        .iter()
        .map(|item| item.occurred_at)
        .chain(decisions.iter().map(|item| item.occurred_at))
        .max()
        .unwrap_or_else(Utc::now);

    let mut cards: Vec<_> = map
        .into_iter()
        .map(|(budget_id, acc)| {
            let peak_day = acc.by_day.values().copied().fold(0.0_f64, f64::max);
            let peak_day_share = if acc.spend_usd <= 0.0 {
                0.0
            } else {
                (peak_day / acc.spend_usd) * 100.0
            };
            let behavior_label = if acc.spend_usd <= 0.0 {
                "idle".to_owned()
            } else if peak_day_share >= 55.0 {
                "peaky".to_owned()
            } else if peak_day_share >= 35.0 {
                "bursty".to_owned()
            } else {
                "steady".to_owned()
            };
            let observed_capacity = acc
                .latest_remaining
                .map(|remaining| acc.spend_usd + remaining);
            let pressure_ratio = observed_capacity.map(|capacity| ratio(acc.spend_usd, capacity));
            let projected_delta_usd = observed_capacity.and_then(|capacity| {
                let earliest = acc.earliest?;
                let observed_days =
                    ((now.date_naive() - earliest.date_naive()).num_days() + 1).max(1) as f64;
                let projected = (acc.spend_usd / observed_days) * window_days;
                Some(projected - capacity)
            });
            DashboardBudgetCard {
                budget_id,
                spend_usd: acc.spend_usd,
                observed_capacity_usd: observed_capacity,
                budget_window_remaining_usd: acc.latest_remaining,
                pressure_ratio,
                projected_delta_usd,
                decision_count: acc.decision_count,
                limit_hits: acc.limit_hits,
                peak_day_share,
                behavior_label,
            }
        })
        .collect();
    cards.sort_by(|left, right| right.spend_usd.total_cmp(&left.spend_usd));
    cards
}

fn adoption_rows(
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
    observations: &[TraceReportItem],
    lens: DashboardLens,
) -> Vec<DashboardAdoptionEntityRow> {
    #[derive(Default)]
    struct Acc {
        spend_usd: f64,
        total_tokens: u64,
        cache_tokens: u64,
        trace_ids: HashSet<String>,
        tool_events: u64,
        limit_hits: u64,
    }
    let mut map: BTreeMap<String, Acc> = BTreeMap::new();
    let mut trace_to_entity: HashMap<String, BTreeSet<String>> = HashMap::new();
    for item in usage {
        let Some(entity) = lens_value_from_usage(item, lens) else {
            continue;
        };
        let entry = map.entry(entity.clone()).or_default();
        entry.spend_usd += item.cost_usd;
        entry.total_tokens += item.total_tokens;
        entry.cache_tokens += item.cache_read_tokens + item.cache_write_tokens;
        if let Some(trace_id) = &item.trace_id {
            entry.trace_ids.insert(trace_id.clone());
            trace_to_entity
                .entry(trace_id.clone())
                .or_default()
                .insert(entity);
        }
    }
    for item in decisions {
        let Some(trace_id) = &item.trace_id else {
            continue;
        };
        if let Some(entities) = trace_to_entity.get(trace_id) {
            for entity in entities {
                map.entry(entity.clone()).or_default().limit_hits += item
                    .limit_hits
                    .as_ref()
                    .map(|hits| hits.len() as u64)
                    .unwrap_or(0);
            }
        }
    }
    for item in observations {
        let Some(trace_id) = &item.trace_id else {
            continue;
        };
        if !is_tool_kind(&item.kind) {
            continue;
        }
        if let Some(entities) = trace_to_entity.get(trace_id) {
            for entity in entities {
                map.entry(entity.clone()).or_default().tool_events += 1;
            }
        }
    }

    let mut rows: Vec<_> = map
        .into_iter()
        .map(|(entity, acc)| {
            let cache_ratio = ratio(acc.cache_tokens as f64, acc.total_tokens as f64);
            let traces = acc.trace_ids.len() as u64;
            let health_label = if acc.limit_hits > 0 {
                "Needs policy coaching"
            } else if traces > 0 && acc.tool_events > traces.saturating_mul(3) {
                "Tool-heavy"
            } else if acc.total_tokens > 0 && cache_ratio < 5.0 {
                "Cache opportunity"
            } else {
                "Healthy"
            }
            .to_owned();
            let opportunity_label = if acc.spend_usd <= 0.0 {
                "No finalized usage yet".to_owned()
            } else if cache_ratio < 5.0 {
                "Improve cache reuse".to_owned()
            } else if acc.limit_hits > 0 {
                "Review limit hits".to_owned()
            } else {
                "Stable usage".to_owned()
            };
            DashboardAdoptionEntityRow {
                entity: prettify_entity_label(&entity),
                spend_usd: acc.spend_usd,
                total_tokens: acc.total_tokens,
                traces,
                cache_ratio,
                tool_events: acc.tool_events,
                limit_hits: acc.limit_hits,
                health_label,
                opportunity_label,
            }
        })
        .collect();
    rows.sort_by(|left, right| right.spend_usd.total_cmp(&left.spend_usd));
    rows.truncate(12);
    rows
}

fn series_points(
    usage: &[UsageActivityRecord],
    decisions: &[TraceReportItem],
) -> Vec<DashboardSeriesPoint> {
    #[derive(Default)]
    struct Acc {
        spend_usd: f64,
        total_tokens: u64,
        decisions: u64,
        trace_ids: HashSet<String>,
    }
    let mut map: BTreeMap<NaiveDate, Acc> = BTreeMap::new();
    for item in usage {
        let entry = map.entry(item.occurred_at.date_naive()).or_default();
        entry.spend_usd += item.cost_usd;
        entry.total_tokens += item.total_tokens;
        if let Some(trace_id) = &item.trace_id {
            entry.trace_ids.insert(trace_id.clone());
        }
    }
    for item in decisions {
        let entry = map.entry(item.occurred_at.date_naive()).or_default();
        entry.decisions += 1;
        if let Some(trace_id) = &item.trace_id {
            entry.trace_ids.insert(trace_id.clone());
        }
    }
    let mut points: Vec<_> = map
        .into_iter()
        .map(|(date, acc)| DashboardSeriesPoint {
            label: format!("{}-{:02}", date.month(), date.day()),
            spend_usd: acc.spend_usd,
            total_tokens: acc.total_tokens,
            decisions: acc.decisions,
            traces: acc.trace_ids.len() as u64,
        })
        .collect();
    if points.len() > 14 {
        points = points.split_off(points.len().saturating_sub(14));
    }
    points
}

fn decision_stats(
    decisions: &[TraceReportItem],
    trace: Option<&TraceReport>,
) -> DashboardDecisionStats {
    let mut stats = DashboardDecisionStats::default();
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
    stats.lifecycle_limits = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("limit.report_only."))
                .count() as u64
        })
        .unwrap_or_default();
    stats
}

fn interop_nodes(trace: Option<&TraceReport>) -> Vec<DashboardInteropNode> {
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    for item in trace.into_iter().flat_map(|trace| trace.items.iter()) {
        *counts.entry(event_category(&item.kind)).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(id, count)| DashboardInteropNode {
            id: id.to_owned(),
            label: id.replace('_', " "),
            count,
        })
        .collect()
}

fn lens_value_from_usage(item: &UsageActivityRecord, lens: DashboardLens) -> Option<String> {
    match lens {
        DashboardLens::Project => item
            .project
            .clone()
            .map(|project| format!("project:{project}"))
            .or_else(|| entity_with_prefix(&item.entities, "project"))
            .or_else(|| item.matched_entity.clone()),
        DashboardLens::User => entity_with_prefix(&item.entities, "user")
            .or_else(|| item.subject.as_deref().map(normalize_subject))
            .or_else(|| item.matched_entity.clone()),
        DashboardLens::Team => entity_with_prefix(&item.entities, "team").or_else(|| {
            item.matched_entity
                .clone()
                .filter(|value| value.starts_with("team:"))
        }),
        DashboardLens::Company => entity_with_prefix(&item.entities, "org"),
        DashboardLens::Workflow => entity_with_prefix(&item.entities, "workflow"),
        DashboardLens::Surface => entity_with_prefix(&item.entities, "surface"),
        DashboardLens::Budget => item.selected_budget_id.clone(),
        DashboardLens::Model => model_label(item.provider.as_deref(), item.model.as_deref()),
    }
}

fn lens_value_from_decision(item: &TraceReportItem, lens: DashboardLens) -> Option<String> {
    match lens {
        DashboardLens::Project => entity_with_prefix(&item.entities, "project"),
        DashboardLens::User => entity_with_prefix(&item.entities, "user"),
        DashboardLens::Team => entity_with_prefix(&item.entities, "team").or_else(|| {
            item.routing
                .as_ref()
                .and_then(|routing| routing.matched_entity.clone())
                .filter(|value| value.starts_with("team:"))
        }),
        DashboardLens::Company => entity_with_prefix(&item.entities, "org"),
        DashboardLens::Workflow => entity_with_prefix(&item.entities, "workflow"),
        DashboardLens::Surface => entity_with_prefix(&item.entities, "surface"),
        DashboardLens::Budget => item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.clone()),
        DashboardLens::Model => reporting::summary_value(&item.summary, "model"),
    }
}

fn matches_usage_entity(item: &UsageActivityRecord, lens: DashboardLens, entity: &str) -> bool {
    lens_value_from_usage(item, lens).as_deref() == Some(entity)
}

fn matches_decision_entity(item: &TraceReportItem, lens: DashboardLens, entity: &str) -> bool {
    lens_value_from_decision(item, lens).as_deref() == Some(entity)
}

fn matches_usage_lens(item: &UsageActivityRecord, lens: DashboardLens) -> bool {
    lens_value_from_usage(item, lens).is_some()
}

fn matches_decision_lens(item: &TraceReportItem, lens: DashboardLens) -> bool {
    lens_value_from_decision(item, lens).is_some()
}

fn normalize_subject(subject: &str) -> String {
    if subject.contains(':') {
        subject.to_owned()
    } else {
        format!("user:{subject}")
    }
}

fn entity_with_prefix(entities: &[String], prefix: &str) -> Option<String> {
    let needle = format!("{prefix}:");
    entities
        .iter()
        .find(|entity| entity.starts_with(&needle))
        .cloned()
}

fn model_label(provider: Option<&str>, model: Option<&str>) -> Option<String> {
    match (provider, model) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (None, Some(model)) => Some(model.to_owned()),
        (Some(provider), None) => Some(provider.to_owned()),
        (None, None) => None,
    }
}

fn prettify_entity_label(value: &str) -> String {
    let base = value.split_once(':').map(|(_, tail)| tail).unwrap_or(value);
    base.replace(['-', '_'], " ")
}

fn event_category(kind: &str) -> &'static str {
    if kind.starts_with("decision.") {
        "policy"
    } else if kind == "usage.finalized" {
        "usage"
    } else if is_tool_kind(kind) {
        "tools"
    } else if kind == "pi.agent_context" {
        "context"
    } else if kind.starts_with("pi.") {
        "agent_runtime"
    } else if kind.starts_with("eval.") {
        "evals"
    } else if kind.starts_with("limit.report_only.") {
        "limits"
    } else {
        "events"
    }
}

fn is_tool_kind(kind: &str) -> bool {
    kind == "tool.observed" || kind == "pi.tool_call"
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator <= 0.0 {
        0.0
    } else {
        (numerator / denominator) * 100.0
    }
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

fn money(value: f64) -> String {
    if value.abs() < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::contract::{AuthorizeRequest, FinalizeReservation, TraceEvent, UsageObservation};

    use super::*;

    #[test]
    fn overview_builds_entity_filters_and_budget_cards() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("dashboard.sqlite");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let reservation = ledger
            .try_authorize(None, &request("trace-a", "req-a", "gpt-4.1", 1.25))
            .expect("authorize")
            .reservation
            .expect("reservation");
        ledger
            .finalize(
                &reservation.id,
                &finalize_payload("trace-a", "gpt-4.1", 1.25, 500, 100),
            )
            .expect("finalize");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-tool".to_owned()),
                trace_id: Some("trace-a".to_owned()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload: json!({"name":"bash"}),
            })
            .expect("event");

        let data = overview(
            &ledger,
            DashboardViewQuery {
                window: Some("all"),
                lens: Some("project"),
                entity: None,
                trace: None,
            },
        )
        .expect("overview");

        assert_eq!(data.filters.selected_lens, "project");
        assert_eq!(data.filters.entities[0].value, "project:noether");
        assert_eq!(data.kpis.len(), 4);
        assert!(!data.spend_trend.is_empty());
        assert!(!data.spend_distribution.is_empty());
    }

    #[test]
    fn trace_explorer_keeps_trace_ids_on_observations() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("traces.sqlite");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-tool".to_owned()),
                trace_id: Some("trace-z".to_owned()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload: json!({"name":"bash"}),
            })
            .expect("event");

        let data = traces(
            &ledger,
            DashboardViewQuery {
                window: Some("all"),
                lens: Some("project"),
                entity: None,
                trace: Some("trace-z"),
            },
        )
        .expect("traces");

        assert_eq!(data.selected_trace_id.as_deref(), Some("trace-z"));
        assert_eq!(data.traces[0].trace_id, "trace-z");
    }

    #[test]
    fn user_lens_excludes_traces_without_subjects() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("user-lens.sqlite");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let reservation = ledger
            .try_authorize(
                None,
                &request_with_subject(
                    Some("user:visible"),
                    "trace-visible",
                    "req-visible",
                    "gpt-4.1",
                    1.0,
                ),
            )
            .expect("authorize visible")
            .reservation
            .expect("visible reservation");
        ledger
            .finalize(
                &reservation.id,
                &finalize_payload("trace-visible", "gpt-4.1", 1.0, 300, 120),
            )
            .expect("finalize visible");

        let reservation = ledger
            .try_authorize(
                None,
                &request_with_subject(None, "trace-hidden", "req-hidden", "gpt-4.1", 2.0),
            )
            .expect("authorize hidden")
            .reservation
            .expect("hidden reservation");
        ledger
            .finalize(
                &reservation.id,
                &finalize_payload("trace-hidden", "gpt-4.1", 2.0, 500, 200),
            )
            .expect("finalize hidden");

        let data = traces(
            &ledger,
            DashboardViewQuery {
                window: Some("all"),
                lens: Some("user"),
                entity: None,
                trace: None,
            },
        )
        .expect("user traces");

        assert_eq!(data.filters.selected_lens, "user");
        assert_eq!(data.traces.len(), 1);
        assert_eq!(data.traces[0].trace_id, "trace-visible");
        assert_eq!(data.filters.entities[0].value, "user:visible");
    }

    fn request(
        trace_id: &str,
        request_id: &str,
        model: &str,
        estimated_cost_usd: f64,
    ) -> AuthorizeRequest {
        request_with_subject(
            Some("user:local"),
            trace_id,
            request_id,
            model,
            estimated_cost_usd,
        )
    }

    fn request_with_subject(
        subject: Option<&str>,
        trace_id: &str,
        request_id: &str,
        model: &str,
        estimated_cost_usd: f64,
    ) -> AuthorizeRequest {
        let mut metadata = BTreeMap::new();
        metadata.insert("trace_id".to_owned(), json!(trace_id));
        metadata.insert("request_id".to_owned(), json!(request_id));
        AuthorizeRequest {
            budget_id: None,
            entities: vec!["project:noether".to_owned()],
            subject: subject.map(ToOwned::to_owned),
            project: Some("noether".to_owned()),
            provider: Some("openai".to_owned()),
            model: Some(model.to_owned()),
            estimated_tokens: Some((estimated_cost_usd * 1_000_000.0) as u64),
            estimated_cost_usd: Some(estimated_cost_usd),
            metadata,
        }
    }

    fn finalize_payload(
        trace_id: &str,
        model: &str,
        actual_cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
    ) -> FinalizeReservation {
        let mut metadata = BTreeMap::new();
        metadata.insert("trace_id".to_owned(), json!(trace_id));
        FinalizeReservation {
            reservation_id: None,
            usage: Some(UsageObservation {
                provider: Some("openai".to_owned()),
                model: Some(model.to_owned()),
                input_tokens: Some(input_tokens),
                output_tokens: Some(output_tokens),
                total_tokens: Some(input_tokens + output_tokens),
                cost_usd: Some(actual_cost_usd),
                latency_ms: None,
                stop_reason: None,
            }),
            actual_cost_usd: Some(actual_cost_usd),
            metadata,
        }
    }
}
