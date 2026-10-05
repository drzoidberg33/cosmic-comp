// SPDX-License-Identifier: GPL-3.0-only

//! Behaviour that already works upstream; guards the harness itself and catches regressions.

use cosmic_comp_test_harness::*;

const GREEN: &str = "00ff00";

#[test]
fn outputs_are_side_by_side() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let outputs = comp.outputs().unwrap();
    let geometries = outputs
        .iter()
        .map(|o| (o.name.as_str(), o.geometry))
        .collect::<Vec<_>>();
    assert_eq!(
        geometries,
        vec![
            (
                "HEADLESS-0",
                Rect {
                    x: 0,
                    y: 0,
                    width: 1280,
                    height: 720
                }
            ),
            (
                "HEADLESS-1",
                Rect {
                    x: 1280,
                    y: 0,
                    width: 1280,
                    height: 720
                }
            ),
        ]
    );
}

#[test]
fn window_maps_on_first_output() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let mut client = comp.spawn_client("green", &["--color", GREEN]).unwrap();
    let window = comp.wait_window("green", "mapped", |_| true).unwrap();
    assert_eq!(window.output, "HEADLESS-0");
    assert!(window.floating, "{window:?}");

    client.wait_entered_outputs(&["HEADLESS-0"]).unwrap();

    let output = comp.output("HEADLESS-0").unwrap();
    let shot = comp.screenshot("HEADLESS-0").unwrap();
    let coverage = shot.coverage(to_output_pixels(window.content(), &output), rgb(GREEN), 2);
    assert!(coverage > 0.95, "window content coverage {coverage}");
    let other = comp.screenshot("HEADLESS-1").unwrap();
    assert_eq!(other.total_coverage(rgb(GREEN), 2), 0.0);
}

#[test]
fn pointer_input_reaches_window() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let mut client = comp.spawn_client("green", &["--color", GREEN]).unwrap();
    let window = comp.wait_window("green", "mapped", |_| true).unwrap();
    let content = window.content();

    comp.pointer_motion((content.x as f64 + 10.0, content.y as f64 + 20.0))
        .unwrap();
    let enter = client
        .wait_event("pointer_enter", |e| e["event"] == "pointer_enter")
        .unwrap();
    assert_eq!(enter["x"].as_f64(), Some(10.0), "{enter}");
    assert_eq!(enter["y"].as_f64(), Some(20.0), "{enter}");

    comp.button("left", true).unwrap();
    comp.button("left", false).unwrap();
    client
        .wait_event("button press", |e| {
            e["event"] == "pointer_button" && e["pressed"] == true
        })
        .unwrap();
}

#[test]
fn dragging_title_bar_moves_window_to_other_output() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let mut client = comp.spawn_client("green", &["--color", GREEN]).unwrap();
    comp.wait_window("green", "mapped", |_| true).unwrap();

    let window = comp.drag_window_to("green", (1280 + 200, 100)).unwrap();
    assert_eq!(window.output, "HEADLESS-1");
    client.wait_entered_outputs(&["HEADLESS-1"]).unwrap();

    let output = comp.output("HEADLESS-1").unwrap();
    let shot = comp.screenshot("HEADLESS-1").unwrap();
    let coverage = shot.coverage(to_output_pixels(window.content(), &output), rgb(GREEN), 2);
    assert!(coverage > 0.95, "window content coverage {coverage}");
    let first = comp.screenshot("HEADLESS-0").unwrap();
    assert_eq!(first.total_coverage(rgb(GREEN), 2), 0.0);
}
