// SPDX-License-Identifier: GPL-3.0-only

//! Benchmarks cosmic-comp on the headless backend, A/B against a baseline build.
//!
//! ```text
//! bench run   [--binary BIN] [--rounds N] [--filter SUBSTR] [--json FILE]
//! bench ab    --baseline BIN --candidate BIN [--rounds N] [--filter SUBSTR] [--json FILE]
//! bench list
//! ```
//!
//! Every scenario starts a fresh compositor, maps solid-colour windows at fixed positions
//! (some straddling outputs) and measures, inside the compositor:
//!
//! - `render/<output>/{cpu,total}`: full redraw of each output (submit / until `glFinish`),
//! - `input/{surface_under,element_under}`: pointer hit-testing over windows, desktop and the
//!   parts of windows hanging over a seam,
//! - `refresh`: `Common::refresh`, which includes per-output window bookkeeping.
//!
//! `ab` alternates baseline and candidate runs per round to cancel out drift (thermal, clocks)
//! and compares the median over rounds of each metric's median. A metric regresses when it is
//! slower than its threshold allows (see `threshold`); `ab` then exits with 1.

use cosmic_comp_test_harness::{Compositor, Options};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

const RENDER_FRAMES: usize = 240;
const RENDER_WARMUP: usize = 30;
const INPUT_ITERATIONS: usize = 20_000;
const REFRESH_ITERATIONS: usize = 2_000;

const WINDOW_SIZE: (i32, i32) = (400, 300);
const COLORS: [&str; 6] = ["ff0000", "00ff00", "0000ff", "ffff00", "ff00ff", "00ffff"];

struct Scenario {
    name: &'static str,
    outputs: &'static str,
    /// Top-left corners (global, title bar included) of the windows to map, in mapping order.
    windows: Vec<(i32, i32)>,
    /// Whether some windows straddle outputs. Those scenarios do work the baseline can't, so
    /// they get absolute budgets instead of relative thresholds, see [`threshold`].
    spanning: bool,
}

fn grid(origin: (i32, i32), count: usize, columns: usize) -> Vec<(i32, i32)> {
    (0..count)
        .map(|i| {
            (
                origin.0 + (i % columns) as i32 * 300,
                origin.1 + (i / columns) as i32 * 380,
            )
        })
        .collect()
}

fn scenarios() -> Vec<Scenario> {
    let straddle = |seam: i32, y: i32| (seam - WINDOW_SIZE.0 / 2, y);
    vec![
        Scenario {
            name: "1out-1win",
            outputs: "1920x1080",
            windows: vec![(760, 390)],
            spanning: false,
        },
        Scenario {
            name: "2out-10win",
            outputs: "1920x1080,1920x1080",
            windows: grid((100, 100), 10, 5),
            spanning: false,
        },
        Scenario {
            name: "2out-10win-1span",
            outputs: "1920x1080,1920x1080",
            windows: grid((100, 100), 9, 5)
                .into_iter()
                .chain([straddle(1920, 300)])
                .collect(),
            spanning: true,
        },
        Scenario {
            name: "2out-5span",
            outputs: "1920x1080,1920x1080",
            windows: (0..5).map(|i| straddle(1920, 60 + i * 150)).collect(),
            spanning: true,
        },
        Scenario {
            name: "4out-20win-4span",
            outputs: "1920x1080,1920x1080,1920x1080,1920x1080",
            windows: (0..4)
                .flat_map(|o| grid((o * 1920 + 300, 100), 4, 2))
                .chain([
                    straddle(1920, 200),
                    straddle(1920, 600),
                    straddle(3840, 400),
                    straddle(5760, 400),
                ])
                .collect(),
            spanning: true,
        },
        Scenario {
            name: "3out-pyramid-3span",
            outputs: "1920x1080+0+1080,1920x1080+1920+1080,1920x1080+960+0",
            windows: vec![
                (300, 1300),
                (2400, 1300),
                (1300, 300),
                (1200, 900),
                (1720, 900),
                straddle(1920, 1500),
            ],
            spanning: true,
        },
        Scenario {
            name: "mixed-scale-1span",
            outputs: "1920x1080,2560x1440@2",
            windows: vec![(200, 200), straddle(1920, 300)],
            spanning: true,
        },
    ]
}

/// Thresholds as (relative, absolute in µs): a metric regresses only when it is slower by more
/// than both.
///
/// Scenarios without spanning windows must stay within noise of the baseline. With spanning
/// windows the candidate does real extra work the baseline doesn't: drawing windows on a second
/// output (about as expensive as drawing them on their own), and hit-testing overhangs that used
/// to be empty desktop. A relative limit can't tell that apart from waste, so those scenarios get
/// absolute budgets instead:
///
/// - render: +300µs per output and frame, ~1.8% of a 60Hz frame;
/// - input: +5µs per lookup, ~0.5% of a core at 1000 pointer events per second;
/// - refresh: +15µs, which runs at most every 150ms.
fn threshold(metric: &str, spanning: bool) -> (f64, f64) {
    match (metric.split('/').next().unwrap_or_default(), spanning) {
        ("render", false) => (0.10, 50.0),
        ("input", false) => (0.15, 0.5),
        (_, false) => (0.15, 5.0),
        ("render", true) => (0.0, 300.0),
        ("input", true) => (0.0, 5.0),
        (_, true) => (0.0, 15.0),
    }
}

/// Runs one scenario once and returns metric name → median in µs.
fn run_scenario(
    binary: Option<&PathBuf>,
    scenario: &Scenario,
) -> Result<BTreeMap<String, f64>, String> {
    let mut options = Options::default().outputs(scenario.outputs).config(
        "com.system76.CosmicComp",
        "autotile",
        "false",
    );
    options.rust_log = "warn".into();
    if let Some(binary) = binary {
        options = options.binary(binary);
    }
    let mut comp = Compositor::start(options)?;

    let mut clients = Vec::new();
    for (i, &position) in scenario.windows.iter().enumerate() {
        let title = format!("w{i}");
        let width = WINDOW_SIZE.0.to_string();
        let height = (WINDOW_SIZE.1 - cosmic_comp_test_harness::HEADER_HEIGHT).to_string();
        let client = comp.spawn_client(
            &title,
            &[
                "--color",
                COLORS[i % COLORS.len()],
                "--width",
                &width,
                "--height",
                &height,
            ],
        )?;
        comp.wait_window(&title, "mapped", |_| true)?;
        comp.drag_window_to(&title, position)?;
        clients.push(client);
    }

    let outputs = comp.outputs()?;
    let windows = comp.windows()?;
    let mut points = Vec::new();
    for window in &windows {
        let content = window.content();
        let (cx, cy) = content.center();
        points.push(json!([cx, cy]));
        // Inside the window on every output it overlaps, e.g. the overhang of straddling ones.
        for output in &outputs {
            if let Some(visible) = content.intersection(&output.geometry) {
                let (x, y) = visible.center();
                points.push(json!([x, y]));
            }
        }
    }
    for output in &outputs {
        let g = output.geometry;
        points.push(json!([g.x as f64 + 5.0, (g.y + g.height) as f64 - 5.0]));
    }

    let mut metrics = BTreeMap::new();
    let median = |value: &Value| value["median"].as_f64().unwrap_or(f64::NAN);
    for output in &outputs {
        let response = comp.request(json!({
            "cmd": "bench_render",
            "output": output.name,
            "frames": RENDER_FRAMES,
            "warmup": RENDER_WARMUP,
        }))?;
        metrics.insert(
            format!("render/{}/cpu", output.name),
            median(&response["cpu"]),
        );
        metrics.insert(
            format!("render/{}/total", output.name),
            median(&response["total"]),
        );
    }
    let response = comp.request(json!({
        "cmd": "bench_input",
        "points": points,
        "iterations": INPUT_ITERATIONS,
    }))?;
    metrics.insert(
        "input/surface_under".into(),
        median(&response["surface_under"]),
    );
    metrics.insert(
        "input/element_under".into(),
        median(&response["element_under"]),
    );
    let response =
        comp.request(json!({"cmd": "bench_refresh", "iterations": REFRESH_ITERATIONS}))?;
    metrics.insert("refresh".into(), median(&response["refresh"]));

    drop(clients);
    Ok(metrics)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// Runs `scenario` `rounds` times per binary (alternating) and returns per binary the median of
/// each metric over rounds.
fn measure(
    binaries: &[Option<&PathBuf>],
    scenario: &Scenario,
    rounds: usize,
) -> Result<Vec<BTreeMap<String, f64>>, String> {
    let mut samples = vec![BTreeMap::<String, Vec<f64>>::new(); binaries.len()];
    for round in 0..rounds {
        for (i, binary) in binaries.iter().enumerate() {
            eprintln!(
                "  {} round {}/{} {}",
                scenario.name,
                round + 1,
                rounds,
                binary.map(|b| b.display().to_string()).unwrap_or_default()
            );
            for (metric, value) in run_scenario(*binary, scenario)? {
                samples[i].entry(metric).or_default().push(value);
            }
        }
    }
    Ok(samples
        .into_iter()
        .map(|metrics| {
            metrics
                .into_iter()
                .map(|(metric, mut values)| (metric, median(&mut values)))
                .collect()
        })
        .collect())
}

struct Args {
    command: String,
    binary: Option<PathBuf>,
    baseline: Option<PathBuf>,
    candidate: Option<PathBuf>,
    rounds: usize,
    filter: Option<String>,
    json: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or("expected a command: run, ab or list")?;
    let mut parsed = Args {
        command,
        binary: None,
        baseline: None,
        candidate: None,
        rounds: 3,
        filter: None,
        json: None,
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} expects a value"));
        match arg.as_str() {
            "--binary" => parsed.binary = Some(value()?.into()),
            "--baseline" => parsed.baseline = Some(value()?.into()),
            "--candidate" => parsed.candidate = Some(value()?.into()),
            "--rounds" => parsed.rounds = value()?.parse().map_err(|e| format!("--rounds: {e}"))?,
            "--filter" => parsed.filter = Some(value()?),
            "--json" => parsed.json = Some(value()?.into()),
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    if parsed.rounds == 0 {
        return Err("--rounds must be at least 1".into());
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("bench: {err}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let args = parse_args()?;
    let scenarios = scenarios()
        .into_iter()
        .filter(|s| {
            args.filter
                .as_ref()
                .is_none_or(|f| s.name.contains(f.as_str()))
        })
        .collect::<Vec<_>>();

    match args.command.as_str() {
        "list" => {
            for scenario in &scenarios {
                println!(
                    "{:20} outputs={} windows={}",
                    scenario.name,
                    scenario.outputs,
                    scenario.windows.len()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        "run" => {
            let mut report = Map::new();
            for scenario in &scenarios {
                let [metrics] = measure(&[args.binary.as_ref()], scenario, args.rounds)?
                    .try_into()
                    .expect("one binary");
                println!("{}", scenario.name);
                for (metric, value) in &metrics {
                    println!("  {metric:32} {value:>10.2} µs");
                }
                report.insert(scenario.name.into(), json!(metrics));
            }
            write_json(args.json.as_ref(), &Value::Object(report))?;
            Ok(ExitCode::SUCCESS)
        }
        "ab" => {
            let baseline = args.baseline.as_ref().ok_or("ab needs --baseline")?;
            let candidate = args.candidate.as_ref().ok_or("ab needs --candidate")?;
            let mut report = Map::new();
            let mut regressions = Vec::new();
            for scenario in &scenarios {
                let [base, cand] =
                    measure(&[Some(baseline), Some(candidate)], scenario, args.rounds)?
                        .try_into()
                        .expect("two binaries");
                println!(
                    "{}{}",
                    scenario.name,
                    if scenario.spanning { " (spanning)" } else { "" }
                );
                println!(
                    "  {:32} {:>10} {:>10} {:>8}   threshold",
                    "metric (median µs)", "baseline", "candidate", "change"
                );
                let mut entries = Map::new();
                for (metric, b) in &base {
                    let c = cand.get(metric).copied().unwrap_or(f64::NAN);
                    let change = (c - b) / b;
                    let (rel, abs) = threshold(metric, scenario.spanning);
                    let regressed = change > rel && c - b > abs;
                    let limit = if rel > 0.0 {
                        format!("+{:.0}% & +{abs}µs", rel * 100.0)
                    } else {
                        format!("budget +{abs}µs")
                    };
                    println!(
                        "  {metric:32} {b:>10.2} {c:>10.2} {:>+7.1}%   {limit}{}",
                        change * 100.0,
                        if regressed { "  REGRESSION" } else { "" }
                    );
                    if regressed {
                        regressions.push(format!("{}: {metric}", scenario.name));
                    }
                    entries.insert(
                        metric.clone(),
                        json!({"baseline": b, "candidate": c, "change": change, "regressed": regressed}),
                    );
                }
                report.insert(scenario.name.into(), Value::Object(entries));
            }
            write_json(args.json.as_ref(), &Value::Object(report))?;
            if regressions.is_empty() {
                println!("\nNo regressions beyond thresholds.");
                Ok(ExitCode::SUCCESS)
            } else {
                println!("\nRegressions:");
                for regression in &regressions {
                    println!("  {regression}");
                }
                Ok(ExitCode::FAILURE)
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn write_json(path: Option<&PathBuf>, value: &Value) -> Result<(), String> {
    if let Some(path) = path {
        std::fs::write(path, serde_json::to_string_pretty(value).unwrap())
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}
