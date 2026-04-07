use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReadinessSnapshot {
    pub status: String,
    pub chain_id: u64,
    pub block: Option<u64>,
    pub pools: Option<u64>,
    pub vm_enabled: bool,
    pub vm_status: Option<String>,
    pub vm_block: Option<u64>,
    pub vm_pools: Option<u64>,
    pub vm_restarts: Option<u64>,
    pub vm_last_error: Option<String>,
    pub vm_rebuild_duration_ms: Option<u64>,
    pub vm_last_update_age_ms: Option<u64>,
    pub rfq_enabled: bool,
    pub rfq_status: Option<String>,
    pub rfq_block: Option<u64>,
    pub rfq_pools: Option<u64>,
    pub rfq_restarts: Option<u64>,
    pub rfq_last_error: Option<String>,
    pub rfq_rebuild_duration_ms: Option<u64>,
    pub rfq_last_update_age_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AnalysisReport {
    pub schema_version: u64,
    pub run: RunMetadata,
    pub readiness: ReadinessReport,
    pub scenarios: Vec<ScenarioReport>,
    pub logs: LogSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub findings: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineComparison>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RunMetadata {
    pub started_at_epoch_s: u64,
    pub finished_at_epoch_s: u64,
    pub chain_id: u64,
    pub chain_label: String,
    pub profile: String,
    pub repo: String,
    pub base_url: String,
    pub report_dir: String,
    pub started_server: bool,
    pub stop_requested: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReadinessReport {
    pub initial: ReadinessSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_state: Option<ReadinessSnapshot>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScenarioReport {
    pub kind: String,
    pub label: String,
    pub endpoint: String,
    pub request_count: usize,
    pub healthy_count: usize,
    pub degraded_count: usize,
    pub error_count: usize,
    pub status_counts: BTreeMap<String, usize>,
    pub result_quality_counts: BTreeMap<String, usize>,
    pub protocols_seen: BTreeMap<String, usize>,
    pub latency_ms: LatencySummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence_files: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct LatencySummary {
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p99: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LogSummary {
    pub log_file: String,
    pub matched_lines: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt_file: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Finding {
    pub severity: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BaselineComparison {
    pub compared_report_dir: String,
    pub scenario_diffs: Vec<ScenarioDiff>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScenarioDiff {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_rate_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_rate_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90_delta_pct: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

pub fn build_findings(report: &AnalysisReport) -> Vec<Finding> {
    let mut findings = Vec::new();
    add_readiness_findings(report, &mut findings);
    add_scenario_findings(report, &mut findings);
    add_log_findings(report, &mut findings);
    add_baseline_findings(report, &mut findings);
    add_warning_findings(report, &mut findings);

    if findings.is_empty() {
        findings.push(Finding {
            severity: "healthy".to_string(),
            title: "No major anomalies detected".to_string(),
            detail: "The analyzer completed without request errors and did not spot standout regressions or noisy logs.".to_string(),
        });
    }

    findings
}

fn add_readiness_findings(report: &AnalysisReport, findings: &mut Vec<Finding>) {
    if report.readiness.initial.status != "ready" {
        findings.push(Finding {
            severity: "investigate".to_string(),
            title: "Initial readiness was not ready".to_string(),
            detail: format!(
                "The analyzer began with /status={}. The run still proceeded, but the service state looked unstable at the start.",
                report.readiness.initial.status
            ),
        });
    }

    if report.readiness.initial.vm_enabled
        && report.readiness.initial.vm_status.as_deref() != Some("ready")
    {
        findings.push(Finding {
            severity: "attention".to_string(),
            title: "VM pools were enabled but not fully ready".to_string(),
            detail: format!(
                "Initial VM status was {} with vm_pools={}.",
                report
                    .readiness
                    .initial
                    .vm_status
                    .as_deref()
                    .unwrap_or("unknown"),
                report
                    .readiness
                    .initial
                    .vm_pools
                    .map_or_else(|| "unknown".to_string(), |value| value.to_string())
            ),
        });
    }

    if report.readiness.initial.rfq_enabled
        && report.readiness.initial.rfq_status.as_deref() != Some("ready")
    {
        findings.push(Finding {
            severity: "attention".to_string(),
            title: "RFQ pools were enabled but not fully ready".to_string(),
            detail: format!(
                "Initial RFQ status was {} with rfq_pools={}.",
                report
                    .readiness
                    .initial
                    .rfq_status
                    .as_deref()
                    .unwrap_or("unknown"),
                report
                    .readiness
                    .initial
                    .rfq_pools
                    .map_or_else(|| "unknown".to_string(), |value| value.to_string())
            ),
        });
    }
}

fn add_scenario_findings(report: &AnalysisReport, findings: &mut Vec<Finding>) {
    for scenario in &report.scenarios {
        if scenario.error_count > 0 {
            findings.push(Finding {
                severity: "investigate".to_string(),
                title: format!("{} had request errors", scenario.label),
                detail: format!(
                    "{} reported {} errored request(s) across {} total request(s).",
                    scenario.label, scenario.error_count, scenario.request_count
                ),
            });
        } else if scenario.degraded_count > 0 {
            findings.push(Finding {
                severity: "attention".to_string(),
                title: format!("{} surfaced degraded responses", scenario.label),
                detail: format!(
                    "{} reported {} degraded request(s) across {} total request(s).",
                    scenario.label, scenario.degraded_count, scenario.request_count
                ),
            });
        }
    }
}

fn add_log_findings(report: &AnalysisReport, findings: &mut Vec<Finding>) {
    if report.logs.matched_lines > 0 {
        findings.push(Finding {
            severity: "attention".to_string(),
            title: "Interesting log lines were captured".to_string(),
            detail: format!(
                "The analyzer matched {} warning/error-like log line(s) in {}.",
                report.logs.matched_lines, report.logs.log_file
            ),
        });
    }
}

fn add_baseline_findings(report: &AnalysisReport, findings: &mut Vec<Finding>) {
    if let Some(baseline) = &report.baseline {
        for diff in &baseline.scenario_diffs {
            if let Some(delta) = diff.degraded_rate_delta {
                if delta > 0.05 {
                    findings.push(Finding {
                        severity: "attention".to_string(),
                        title: format!("{} degraded rate increased", diff.label),
                        detail: format!(
                            "Degraded rate increased by {:.2}% compared with the saved baseline.",
                            delta * 100.0
                        ),
                    });
                }
            }
            if let Some(delta) = diff.error_rate_delta {
                if delta > 0.0 {
                    findings.push(Finding {
                        severity: "investigate".to_string(),
                        title: format!("{} error rate increased", diff.label),
                        detail: format!(
                            "Error rate increased by {:.2}% compared with the saved baseline.",
                            delta * 100.0
                        ),
                    });
                }
            }
            if let Some(delta) = diff.p90_delta_pct {
                if delta > 25.0 {
                    findings.push(Finding {
                        severity: "attention".to_string(),
                        title: format!("{} latency regressed", diff.label),
                        detail: format!(
                            "p90 latency increased by {:.1}% compared with the saved baseline.",
                            delta
                        ),
                    });
                }
            }
        }
    }
}

fn add_warning_findings(report: &AnalysisReport, findings: &mut Vec<Finding>) {
    for warning in &report.warnings {
        findings.push(Finding {
            severity: "attention".to_string(),
            title: "Analyzer warning".to_string(),
            detail: warning.clone(),
        });
    }
}

pub fn render_summary(report: &AnalysisReport) -> String {
    let mut lines = Vec::new();
    lines.push("# Simulation analysis summary".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- Chain: {} ({})",
        report.run.chain_label, report.run.chain_id
    ));
    lines.push(format!("- Profile: {}", report.run.profile));
    lines.push(format!("- Report dir: {}", report.run.report_dir));
    lines.push(format!(
        "- Server lifecycle: {}",
        if report.run.started_server {
            "started by analyzer"
        } else {
            "reused existing server"
        }
    ));
    lines.push(String::new());
    lines.push("## Findings".to_string());
    for finding in &report.findings {
        lines.push(format!(
            "- [{}] {}: {}",
            finding.severity, finding.title, finding.detail
        ));
    }
    if !report.warnings.is_empty() {
        lines.push(String::new());
        lines.push("## Analyzer warnings".to_string());
        for warning in &report.warnings {
            lines.push(format!("- {}", warning));
        }
    }
    lines.push(String::new());
    lines.push("## Scenario overview".to_string());
    for scenario in &report.scenarios {
        lines.push(format!(
            "- {} ({}) healthy={} degraded={} errors={} p90={}ms protocols={}",
            scenario.label,
            scenario.kind,
            scenario.healthy_count,
            scenario.degraded_count,
            scenario.error_count,
            fmt_latency(scenario.latency_ms.p90),
            fmt_protocols(&scenario.protocols_seen)
        ));
        for note in &scenario.notes {
            lines.push(format!("  note: {}", note));
        }
    }
    lines.push(String::new());
    lines.push("## Readiness".to_string());
    lines.push(format!(
        "- Initial: status={} block={} pools={} vm_status={} rfq_status={}",
        report.readiness.initial.status,
        report
            .readiness
            .initial
            .block
            .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
        report
            .readiness
            .initial
            .pools
            .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
        readiness_component_status(
            report.readiness.initial.vm_enabled,
            report.readiness.initial.vm_status.as_deref(),
        ),
        readiness_component_status(
            report.readiness.initial.rfq_enabled,
            report.readiness.initial.rfq_status.as_deref(),
        )
    ));
    if let Some(final_state) = &report.readiness.final_state {
        lines.push(format!(
            "- Final: status={} block={} pools={} vm_status={} rfq_status={}",
            final_state.status,
            final_state
                .block
                .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            final_state
                .pools
                .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            readiness_component_status(final_state.vm_enabled, final_state.vm_status.as_deref()),
            readiness_component_status(final_state.rfq_enabled, final_state.rfq_status.as_deref())
        ));
    }
    lines.push(String::new());
    lines.push("## Evidence".to_string());
    lines.push(format!("- Log file: {}", report.logs.log_file));
    if let Some(excerpt) = &report.logs.excerpt_file {
        lines.push(format!("- Log excerpts: {}", excerpt));
    }
    if let Some(baseline) = &report.baseline {
        lines.push(format!(
            "- Compared against: {}",
            baseline.compared_report_dir
        ));
    }
    lines.push("- Primary artifact: report.json".to_string());
    lines.push("- Human summary: summary.md".to_string());
    lines.push(String::new());

    format!("{}\n", lines.join("\n"))
}

pub fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn fmt_latency(value: Option<f64>) -> String {
    value
        .map(|latency| format!("{latency:.1}"))
        .unwrap_or_else(|| "n/a".to_string())
}

fn fmt_protocols(protocols: &BTreeMap<String, usize>) -> String {
    if protocols.is_empty() {
        return "none".to_string();
    }

    protocols
        .iter()
        .take(4)
        .map(|(protocol, count)| format!("{protocol}:{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn readiness_component_status<'a>(enabled: bool, status: Option<&'a str>) -> &'a str {
    match status {
        Some(status) => status,
        None if enabled => "unknown",
        None => "disabled",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_findings, render_summary, AnalysisReport, Finding, LatencySummary, LogSummary,
        ReadinessReport,
    };
    use super::{ReadinessSnapshot, RunMetadata, ScenarioReport};
    use std::collections::BTreeMap;

    fn scenario(label: &str, degraded_count: usize, error_count: usize) -> ScenarioReport {
        ScenarioReport {
            kind: "simulate".to_string(),
            label: label.to_string(),
            endpoint: "/simulate".to_string(),
            request_count: 4,
            healthy_count: 4usize.saturating_sub(degraded_count + error_count),
            degraded_count,
            error_count,
            status_counts: BTreeMap::new(),
            result_quality_counts: BTreeMap::new(),
            protocols_seen: BTreeMap::new(),
            latency_ms: LatencySummary::default(),
            notes: Vec::new(),
            evidence_files: Vec::new(),
        }
    }

    fn report(findings: Vec<Finding>, scenarios: Vec<ScenarioReport>) -> AnalysisReport {
        AnalysisReport {
            schema_version: 2,
            run: RunMetadata {
                started_at_epoch_s: 1,
                finished_at_epoch_s: 2,
                chain_id: 1,
                chain_label: "ethereum".to_string(),
                profile: "balanced".to_string(),
                repo: ".".to_string(),
                base_url: "http://localhost:3000".to_string(),
                report_dir: "logs/simulation-reports/1/balanced/1".to_string(),
                started_server: true,
                stop_requested: true,
            },
            readiness: ReadinessReport {
                initial: ReadinessSnapshot {
                    status: "ready".to_string(),
                    chain_id: 1,
                    block: Some(1),
                    pools: Some(10),
                    vm_enabled: false,
                    vm_status: Some("disabled".to_string()),
                    vm_block: None,
                    vm_pools: None,
                    vm_restarts: None,
                    vm_last_error: None,
                    vm_rebuild_duration_ms: None,
                    vm_last_update_age_ms: None,
                    rfq_enabled: false,
                    rfq_status: None,
                    rfq_block: None,
                    rfq_pools: None,
                    rfq_restarts: None,
                    rfq_last_error: None,
                    rfq_rebuild_duration_ms: None,
                    rfq_last_update_age_ms: None,
                },
                final_state: None,
            },
            scenarios,
            logs: LogSummary {
                log_file: "logs/tycho-sim-server.log".to_string(),
                matched_lines: 0,
                excerpt_file: None,
                highlights: Vec::new(),
            },
            warnings: Vec::new(),
            findings,
            baseline: None,
        }
    }

    #[test]
    fn build_findings_marks_degraded_scenarios() {
        let analysis = report(Vec::new(), vec![scenario("core simulate", 1, 0)]);
        let findings = build_findings(&analysis);
        assert!(findings
            .iter()
            .any(|finding| finding.severity == "attention"));
    }

    #[test]
    fn build_findings_marks_healthy_runs() {
        let analysis = report(Vec::new(), vec![scenario("core simulate", 0, 0)]);
        let findings = build_findings(&analysis);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "healthy");
    }

    #[test]
    fn build_findings_marks_rfq_not_ready() {
        let mut analysis = report(Vec::new(), vec![scenario("core simulate", 0, 0)]);
        analysis.readiness.initial.rfq_enabled = true;
        analysis.readiness.initial.rfq_status = Some("warming_up".to_string());
        analysis.readiness.initial.rfq_pools = Some(0);

        let findings = build_findings(&analysis);

        assert!(findings.iter().any(|finding| {
            finding.title == "RFQ pools were enabled but not fully ready"
                && finding.detail.contains("rfq_pools=0")
        }));
    }

    #[test]
    fn render_summary_includes_rfq_status() {
        let mut analysis = report(Vec::new(), vec![scenario("core simulate", 0, 0)]);
        analysis.readiness.initial.rfq_enabled = true;
        analysis.readiness.initial.rfq_status = Some("ready".to_string());
        analysis.readiness.final_state = Some(ReadinessSnapshot {
            rfq_enabled: true,
            rfq_status: Some("rebuilding".to_string()),
            ..analysis.readiness.initial.clone()
        });

        let summary = render_summary(&analysis);

        assert!(summary.contains("vm_status=disabled rfq_status=ready"));
        assert!(summary.contains("vm_status=disabled rfq_status=rebuilding"));
    }
}
