use std::fmt::Write as _;

use crate::api::schema::{EmptyParams, Method, Request};
use crate::latency::{InputKind, Segment, ServerLatency};

pub(super) fn run_stats_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("latency") => stats_latency(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_stats_help();
            Ok(0)
        }
        _ => {
            print_stats_help();
            Ok(2)
        }
    }
}

fn print_stats_help() {
    eprintln!("usage: twodr stats latency [--json]");
    eprintln!(
        "  twodr stats latency [--json]  input latency and render phase timing, last 5 minutes"
    );
}

fn stats_latency(args: &[String]) -> std::io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: twodr stats latency [--json]");
            return Ok(2);
        }
    };

    let response = super::send_request(&Request {
        id: "cli:stats:latency".into(),
        method: Method::ServerLatency(EmptyParams::default()),
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    let latency =
        match serde_json::from_value::<ServerLatency>(response["result"]["latency"].clone()) {
            Ok(latency) => latency,
            Err(err) => {
                eprintln!("unexpected server.latency response ({err}): {response}");
                return Ok(1);
            }
        };
    print!("{}", format_latency_table(&latency));
    Ok(0)
}

fn ms(us: u64) -> String {
    format!("{:.1}", us as f64 / 1_000.0)
}

pub(super) fn format_latency_table(latency: &ServerLatency) -> String {
    let report = &latency.inputs;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "input latency, last {} min (ms; total = client message received -> frame queued)",
        report.window_secs / 60
    );
    if report.kinds.is_empty() {
        let _ = writeln!(out, "  no completed inputs in the window");
    } else {
        let _ = writeln!(
            out,
            "  {:<7} {:>6}  {:>17}  {:>17}  {:>17}  {:>17}",
            "kind", "count", "total p50/p95/max", "queue p95", "handle p95", "present p95"
        );
        for kind in InputKind::ALL {
            let row = |segment: Segment| {
                report
                    .kinds
                    .iter()
                    .find(|row| row.kind == kind && row.segment == segment)
                    .map(|row| row.summary)
            };
            let Some(total) = row(Segment::Total) else {
                continue;
            };
            let p95 = |segment| row(segment).map_or_else(|| "-".to_owned(), |s| ms(s.p95_us));
            let _ = writeln!(
                out,
                "  {:<7} {:>6}  {:>17}  {:>17}  {:>17}  {:>17}",
                kind.name(),
                total.count,
                format!(
                    "{}/{}/{}",
                    ms(total.p50_us),
                    ms(total.p95_us),
                    ms(total.max_us)
                ),
                p95(Segment::Queue),
                p95(Segment::Handle),
                p95(Segment::Present),
            );
        }
    }
    if !report.panes.is_empty() {
        let _ = writeln!(out, "\nper pane (total ms, slowest p95 first)");
        let _ = writeln!(
            out,
            "  {:<8} {:>6}  {:>8}  {:>8}  {:>8}",
            "pane", "count", "p50", "p95", "max"
        );
        for row in &report.panes {
            let pane = if row.pane == crate::latency::UNKNOWN_PANE {
                "attach".to_owned()
            } else {
                row.pane.to_string()
            };
            let _ = writeln!(
                out,
                "  {:<8} {:>6}  {:>8}  {:>8}  {:>8}",
                pane,
                row.summary.count,
                ms(row.summary.p50_us),
                ms(row.summary.p95_us),
                ms(row.summary.max_us),
            );
        }
    }
    if !latency.phases.is_empty() {
        let _ = writeln!(out, "\nserver phases (ms per call)");
        let _ = writeln!(
            out,
            "  {:<14} {:>8}  {:>8}  {:>8}  {:>8}  slowest pane",
            "phase", "count", "p50", "p95", "max"
        );
        for row in &latency.phases {
            let _ = writeln!(
                out,
                "  {:<14} {:>8}  {:>8}  {:>8}  {:>8}  {}",
                row.phase.name(),
                row.summary.count,
                ms(row.summary.p50_us),
                ms(row.summary.p95_us),
                ms(row.summary.max_us),
                row.max_pane
                    .map_or_else(|| "-".to_owned(), |pane| pane.to_string()),
            );
        }
    }
    let _ = writeln!(
        out,
        "\nin flight {}, dropped {}, never framed {}",
        report.in_flight, report.dropped, report.unframed
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latency::{KindRow, LatencyReport, PaneRow, Phase, Summary};

    fn summary(count: u64, p50_us: u64, p95_us: u64, max_us: u64) -> Summary {
        Summary {
            count,
            p50_us,
            p95_us,
            max_us,
        }
    }

    fn sample() -> ServerLatency {
        ServerLatency {
            inputs: LatencyReport {
                window_secs: 300,
                kinds: vec![
                    KindRow {
                        kind: InputKind::Enter,
                        segment: Segment::Total,
                        summary: summary(3, 4_000, 120_000, 150_000),
                    },
                    KindRow {
                        kind: InputKind::Enter,
                        segment: Segment::Queue,
                        summary: summary(3, 100, 900, 900),
                    },
                ],
                panes: vec![PaneRow {
                    pane: 7,
                    summary: summary(3, 4_000, 120_000, 150_000),
                }],
                in_flight: 1,
                dropped: 0,
                unframed: 2,
            },
            phases: vec![crate::latency::phase::PhaseRow {
                phase: Phase::Cwd,
                summary: summary(90, 30, 1_400_000, 1_400_000),
                max_pane: Some(7),
            }],
        }
    }

    #[test]
    fn stats_latency_table_shows_kinds_panes_and_phases() {
        let table = format_latency_table(&sample());
        assert!(table.contains("last 5 min"), "{table}");
        assert!(table.contains("enter"), "{table}");
        assert!(table.contains("4.0/120.0/150.0"), "{table}");
        assert!(table.contains("0.9"), "queue p95 shown: {table}");
        assert!(table.contains("per pane"), "{table}");
        assert!(table.contains("cwd"), "{table}");
        assert!(table.contains("1400.0"), "{table}");
        assert!(
            table.contains("in flight 1, dropped 0, never framed 2"),
            "{table}"
        );
    }

    #[test]
    fn stats_latency_table_handles_an_empty_window() {
        let mut latency = sample();
        latency.inputs.kinds.clear();
        latency.inputs.panes.clear();
        latency.phases.clear();
        let table = format_latency_table(&latency);
        assert!(table.contains("no completed inputs"), "{table}");
        assert!(!table.contains("per pane"), "{table}");
    }

    #[test]
    fn stats_latency_response_round_trips_through_the_api_schema() {
        let response = crate::api::schema::SuccessResponse {
            id: "cli:stats:latency".into(),
            result: crate::api::schema::ResponseResult::Latency {
                latency: Box::new(sample()),
            },
        };
        let value = serde_json::to_value(&response).expect("serialize");
        assert_eq!(value["result"]["type"], "latency");
        assert_eq!(value["result"]["latency"]["kinds"][0]["kind"], "enter");
        assert_eq!(value["result"]["latency"]["kinds"][0]["p95_us"], 120_000);
        assert_eq!(value["result"]["latency"]["phases"][0]["max_pane"], 7);
        let parsed: ServerLatency =
            serde_json::from_value(value["result"]["latency"].clone()).expect("parse");
        assert_eq!(parsed, sample());
    }

    #[test]
    fn stats_latency_rejects_unknown_arguments() {
        assert_eq!(stats_latency(&["--bogus".into()]).expect("usage"), 2);
        assert_eq!(run_stats_command(&[]).expect("usage"), 2);
    }
}
