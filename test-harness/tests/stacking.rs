// SPDX-License-Identifier: GPL-3.0-only

//! Stacking of floating windows of different outputs against each other, where one reaches
//! onto the other's output.

use cosmic_comp_test_harness::*;

const GREEN: &str = "00ff00";
const BLUE: &str = "0000ff";

fn window_at(comp: &mut Compositor, title: &str, color: &str, at: (i32, i32)) -> Client {
    let client = comp.spawn_client(title, &["--color", color]).unwrap();
    comp.wait_window(title, "mapped", |_| true).unwrap();
    comp.drag_window_to(title, at).unwrap();
    client
}

#[test]
fn raising_orders_windows_across_outputs() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let _a = window_at(&mut comp, "a", GREEN, (100, 100));
    let _b = window_at(&mut comp, "b", BLUE, (1380, 100));
    let a = comp.window("a").unwrap();
    let b = comp.window("b").unwrap();
    assert_eq!((a.output.as_str(), b.output.as_str()), ("HEADLESS-0", "HEADLESS-1"));
    assert!(b.stacking > a.stacking, "{a:?} {b:?}");

    comp.click(a.content().center()).unwrap();
    comp.wait_window("a", "raised above b", |a| a.stacking > b.stacking)
        .unwrap();
}

#[test]
fn window_moving_to_another_output_keeps_its_place_in_the_stacking_order() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let mut x = comp
        .spawn_client("x", &["--color", GREEN, "--width", "1000"])
        .unwrap();
    let mapped = comp.wait_window("x", "mapped", |_| true).unwrap();
    // Drop it by its title bar 700px in, at x = 1400 on HEADLESS-1: 700..1700 belongs to
    // HEADLESS-1 and reaches onto HEADLESS-0.
    let grab = 700.0;
    let from = (
        mapped.geometry.x as f64 + grab,
        mapped.geometry.y as f64 + 18.0,
    );
    comp.drag(from, (700.0 + grab, 100.0 + 18.0), 30).unwrap();
    comp.wait_window("x", "placed", |w| {
        (w.geometry.x, w.output.as_str()) == (700, "HEADLESS-1")
    })
    .unwrap();
    // A window of HEADLESS-0 raised above it, overlapping the overhang.
    let _y = window_at(&mut comp, "y", BLUE, (800, 150));
    let y = comp.window("y").unwrap();

    // The client shrinks it onto HEADLESS-0 by itself, so it moves there without being raised.
    x.send(serde_json::json!({"cmd": "set_size", "width": 400, "height": 300}))
        .unwrap();
    let moved = comp
        .wait_window("x", "on HEADLESS-0", |w| w.output == "HEADLESS-0")
        .unwrap();
    assert!(moved.stacking < y.stacking, "{moved:?} {y:?}");
}
