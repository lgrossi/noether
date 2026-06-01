use std::path::{Component, Path, PathBuf};

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::response::Html;
use serde::{Deserialize, Serialize};

use crate::error::NoetError;
use crate::simulation::SimulationComparisonReport;

use super::AppState;

#[derive(Debug, Deserialize)]
pub(super) struct SimulationStrategyPath {
    simulation_id: String,
    strategy_id: String,
}

#[derive(Clone, Debug)]
struct SimulationArtifact {
    id: String,
    report: SimulationComparisonReport,
    dashboard_path: PathBuf,
}

#[derive(Clone, Debug)]
struct LoadedSimulationStrategy {
    usage_report_path: PathBuf,
    decisions_report_path: PathBuf,
    dashboard_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct SimulationSurfaceSummary {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    seed: u64,
    horizon_days: u32,
    total_requests: u64,
    strategy_count: usize,
    report_url: String,
    dashboard_url: String,
    strategies: Vec<SimulationStrategySurfaceSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct SimulationStrategySurfaceSummary {
    id: String,
    usage_url: String,
    decisions_url: String,
    dashboard_url: String,
}

pub(super) async fn list_simulations(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let simulations = list_simulation_artifacts(&state.simulation_dir)?
        .into_iter()
        .map(simulation_surface_summary)
        .collect::<Vec<_>>();
    Ok(Json(serde_json::to_value(simulations)?))
}

pub(super) async fn simulation_report(
    State(state): State<AppState>,
    AxumPath(simulation_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let artifact = load_simulation_artifact(&state.simulation_dir, &simulation_id)?;
    Ok(Json(serde_json::to_value(artifact.report)?))
}

pub(super) async fn simulation_dashboard_html(
    State(state): State<AppState>,
    AxumPath(simulation_id): AxumPath<String>,
) -> Result<Html<String>, NoetError> {
    let artifact = load_simulation_artifact(&state.simulation_dir, &simulation_id)?;
    if !artifact.dashboard_path.exists() {
        return Err(NoetError::NotFound(format!(
            "simulation dashboard for {} not found",
            artifact.id
        )));
    }
    Ok(Html(std::fs::read_to_string(&artifact.dashboard_path)?))
}

pub(super) async fn simulation_strategy_usage(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<SimulationStrategyPath>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let strategy = load_simulation_strategy(
        &state.simulation_dir,
        &path.simulation_id,
        &path.strategy_id,
    )?;
    Ok(Json(read_json_file(&strategy.usage_report_path)?))
}

pub(super) async fn simulation_strategy_decisions(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<SimulationStrategyPath>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let strategy = load_simulation_strategy(
        &state.simulation_dir,
        &path.simulation_id,
        &path.strategy_id,
    )?;
    Ok(Json(read_json_file(&strategy.decisions_report_path)?))
}

pub(super) async fn simulation_strategy_dashboard_html(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<SimulationStrategyPath>,
) -> Result<Html<String>, NoetError> {
    let strategy = load_simulation_strategy(
        &state.simulation_dir,
        &path.simulation_id,
        &path.strategy_id,
    )?;
    if !strategy.dashboard_path.exists() {
        return Err(NoetError::NotFound(format!(
            "strategy dashboard for {} not found in simulation {}",
            path.strategy_id, path.simulation_id
        )));
    }
    Ok(Html(std::fs::read_to_string(&strategy.dashboard_path)?))
}

pub(super) async fn simulations_index_html(
    State(state): State<AppState>,
) -> Result<Html<String>, NoetError> {
    let simulations = list_simulation_artifacts(&state.simulation_dir)?;
    Ok(Html(render_simulations_index(&simulations)))
}

fn list_simulation_artifacts(simulation_dir: &Path) -> Result<Vec<SimulationArtifact>, NoetError> {
    if !simulation_dir.exists() {
        return Ok(Vec::new());
    }

    let mut artifacts = Vec::new();
    for entry in std::fs::read_dir(simulation_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let report_path = entry.path().join("simulation-report.json");
        let dashboard_path = entry.path().join("simulation-dashboard.html");
        if !report_path.exists() || !dashboard_path.exists() {
            continue;
        }
        let report = read_simulation_report(&report_path)?;
        artifacts.push(SimulationArtifact {
            id,
            report,
            dashboard_path,
        });
    }

    artifacts.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(artifacts)
}

fn load_simulation_artifact(
    simulation_dir: &Path,
    simulation_id: &str,
) -> Result<SimulationArtifact, NoetError> {
    let simulation_id = normalized_surface_id(simulation_id, "simulation")?;
    let simulation_path = simulation_dir.join(simulation_id);
    let report_path = simulation_path.join("simulation-report.json");
    let dashboard_path = simulation_path.join("simulation-dashboard.html");
    if !report_path.exists() {
        return Err(NoetError::NotFound(format!(
            "simulation artifact {simulation_id} not found"
        )));
    }
    Ok(SimulationArtifact {
        id: simulation_id.to_owned(),
        report: read_simulation_report(&report_path)?,
        dashboard_path,
    })
}

fn load_simulation_strategy(
    simulation_dir: &Path,
    simulation_id: &str,
    strategy_id: &str,
) -> Result<LoadedSimulationStrategy, NoetError> {
    let strategy_id = normalized_surface_id(strategy_id, "strategy")?;
    let artifact = load_simulation_artifact(simulation_dir, simulation_id)?;
    let simulation_root = simulation_dir.join(&artifact.id);
    let report = artifact
        .report
        .strategies
        .into_iter()
        .find(|strategy| strategy.id == strategy_id)
        .ok_or_else(|| {
            NoetError::NotFound(format!(
                "strategy artifact {strategy_id} not found in simulation {simulation_id}"
            ))
        })?;
    let strategy_dir =
        simulation_root
            .join("strategies")
            .join(crate::simulation::encode_path_component(
                &report.id,
                "simulation",
            ));
    Ok(LoadedSimulationStrategy {
        usage_report_path: strategy_dir.join("usage-report.json"),
        decisions_report_path: strategy_dir.join("decisions-report.json"),
        dashboard_path: strategy_dir.join("noether-dashboard.html"),
    })
}

fn normalized_surface_id<'a>(value: &'a str, kind: &str) -> Result<&'a str, NoetError> {
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) if !value.trim().is_empty() => Ok(value),
        _ => Err(NoetError::NotFound(format!("invalid {kind} id {value}"))),
    }
}

fn read_simulation_report(path: &Path) -> Result<SimulationComparisonReport, NoetError> {
    Ok(serde_json::from_slice(&read_file_bytes(path)?)?)
}

fn read_json_file(path: &Path) -> Result<serde_json::Value, NoetError> {
    Ok(serde_json::from_slice(&read_file_bytes(path)?)?)
}

fn read_file_bytes(path: &Path) -> Result<Vec<u8>, NoetError> {
    std::fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            NoetError::NotFound(format!("artifact {} not found", path.display()))
        } else {
            error.into()
        }
    })
}

fn simulation_surface_summary(artifact: SimulationArtifact) -> SimulationSurfaceSummary {
    let simulation_id = percent_encode_path_component(&artifact.id);
    let report_url = format!("/v1/simulations/{simulation_id}");
    let dashboard_url = format!("{report_url}/dashboard");
    let strategies = artifact
        .report
        .strategies
        .iter()
        .map(|strategy| SimulationStrategySurfaceSummary {
            id: strategy.id.clone(),
            usage_url: format!(
                "/v1/simulations/{simulation_id}/strategies/{}/usage",
                percent_encode_path_component(&strategy.id)
            ),
            decisions_url: format!(
                "/v1/simulations/{simulation_id}/strategies/{}/decisions",
                percent_encode_path_component(&strategy.id)
            ),
            dashboard_url: format!(
                "/v1/simulations/{simulation_id}/strategies/{}/dashboard",
                percent_encode_path_component(&strategy.id)
            ),
        })
        .collect();
    SimulationSurfaceSummary {
        id: artifact.id,
        name: artifact.report.name,
        seed: artifact.report.seed,
        horizon_days: artifact.report.horizon_days,
        total_requests: artifact.report.total_requests,
        strategy_count: artifact.report.strategies.len(),
        report_url,
        dashboard_url,
        strategies,
    }
}

fn render_simulations_index(simulations: &[SimulationArtifact]) -> String {
    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether simulation surfaces</title>");
    html.push_str(
        "<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --blue:#38bdf8; }
        * { box-sizing:border-box; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:radial-gradient(circle at top left,#172554,#0f172a 42%); color:var(--text); }
        main { max-width:1120px; margin:0 auto; padding:32px 20px 48px; }
        h1 { margin:0 0 8px; font-size:34px; letter-spacing:-0.04em; }
        h2 { margin:0 0 10px; font-size:22px; }
        p, li { color:var(--muted); }
        .sub { margin:0 0 22px; max-width:760px; }
        .stack { display:grid; gap:16px; }
        .card { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; padding:20px; box-shadow:0 18px 55px rgba(0,0,0,.22); }
        .meta { display:flex; gap:10px; flex-wrap:wrap; margin:10px 0 16px; }
        .pill { display:inline-flex; align-items:center; border-radius:999px; padding:4px 9px; background:#1e293b; border:1px solid var(--line); font-size:13px; color:var(--text); }
        .links, .strategy-list { display:grid; gap:8px; }
        .split { display:grid; gap:16px; grid-template-columns:1.15fr .85fr; }
        .empty { color:var(--muted); padding:18px; border:1px dashed var(--line); border-radius:14px; }
        a { color:var(--blue); text-decoration:none; }
        a:hover { text-decoration:underline; }
        code { color:var(--blue); }
        @media (max-width:860px) { .split { grid-template-columns:1fr; } h1 { font-size:28px; } }
        </style>",
    );
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether simulation surfaces</h1>");
    html.push_str("<p class=\"sub\">Artifact-backed simulation review for outputs generated by <code>noet simulate</code>. These routes serve the checked report and dashboard files under the configured simulation directory without inventing a separate server-owned registry.</p>");

    if simulations.is_empty() {
        html.push_str("<div class=\"empty\">No simulation artifacts are available yet. Run <code>noet simulate &lt;file&gt;</code> first, then refresh this page.</div>");
        html.push_str("</main></body></html>");
        return html;
    }

    html.push_str("<section class=\"stack\">");
    for artifact in simulations {
        let title = artifact.report.name.as_deref().unwrap_or(&artifact.id);
        let summary = simulation_surface_summary(artifact.clone());
        let _ = std::fmt::Write::write_fmt(
            &mut html,
            format_args!(
                "<article class=\"card\"><h2>{}</h2><div class=\"meta\"><span class=\"pill\">seed {}</span><span class=\"pill\">{} simulated day(s)</span><span class=\"pill\">{} request(s)</span><span class=\"pill\">{} strategy variant(s)</span></div>",
                escape_html(title),
                artifact.report.seed,
                artifact.report.horizon_days,
                artifact.report.total_requests,
                artifact.report.strategies.len()
            ),
        );
        html.push_str("<div class=\"split\">");
        html.push_str("<div class=\"links\"><strong>Simulation comparison surface</strong>");
        let _ = std::fmt::Write::write_fmt(
            &mut html,
            format_args!(
                "<a href=\"{}\">Comparison dashboard</a><a href=\"{}\">Simulation report JSON</a>",
                escape_html(&summary.dashboard_url),
                escape_html(&summary.report_url)
            ),
        );
        html.push_str(
            "</div><div class=\"strategy-list\"><strong>Per-strategy artifact surfaces</strong>",
        );
        for strategy in summary.strategies {
            let _ = std::fmt::Write::write_fmt(
                &mut html,
                format_args!(
                    "<div><span class=\"pill\">{}</span> <a href=\"{}\">dashboard</a> · <a href=\"{}\">usage</a> · <a href=\"{}\">decisions</a></div>",
                    escape_html(&strategy.id),
                    escape_html(&strategy.dashboard_url),
                    escape_html(&strategy.usage_url),
                    escape_html(&strategy.decisions_url)
                ),
            );
        }
        html.push_str("</div></div></article>");
    }
    html.push_str("</section></main></body></html>");
    html
}

pub(super) fn percent_encode_path_component(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("%{byte:02X}"));
            }
        }
    }
    encoded
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#39;")
}
