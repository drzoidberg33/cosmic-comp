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

/// Waits until the part of `rect` (global) on `output` is drawn in `color`.
fn wait_drawn(comp: &mut Compositor, output: &str, rect: Rect, color: &str, what: &str) {
    let info = comp.output(output).unwrap();
    let visible = rect.intersection(&info.geometry).unwrap();
    let pixels = to_output_pixels(visible, &info);
    comp.wait_screenshot(output, what, |image| {
        image.coverage(pixels, rgb(color), 2) > 0.95
    })
    .unwrap();
}

/// Drags `title` by its title bar, `grab` px in from its left edge, so its top-left corner
/// ends up at `to`. The output under the pointer at the drop becomes its home.
fn drag_by(comp: &mut Compositor, title: &str, grab: f64, to: (i32, i32)) -> WindowInfo {
    let window = comp.window(title).unwrap();
    let from = (window.geometry.x as f64 + grab, window.geometry.y as f64 + 18.0);
    comp.drag(from, (to.0 as f64 + grab, to.1 as f64 + 18.0), 30)
        .unwrap();
    comp.wait_window(title, "placed", |w| (w.geometry.x, w.geometry.y) == to)
        .unwrap()
}

/// `x` (green) belongs to HEADLESS-0 and reaches 150px onto HEADLESS-1, where `y` (blue), a
/// window of HEADLESS-1 raised after it, overlaps the overhang. Returns both and their
/// overlap.
fn overhang_under_other_window(
    comp: &mut Compositor,
) -> (Client, Client, WindowInfo, WindowInfo, Rect) {
    let x_client = window_at(comp, "x", GREEN, (1280 + 150 - 400, 100));
    let y_client = window_at(comp, "y", BLUE, (1330, 200));
    let x = comp.window("x").unwrap();
    let y = comp.window("y").unwrap();
    assert_eq!((x.output.as_str(), y.output.as_str()), ("HEADLESS-0", "HEADLESS-1"));
    let overlap = x.content().intersection(&y.content()).unwrap();
    (x_client, y_client, x, y, overlap)
}

#[test]
fn window_raised_on_the_other_output_covers_the_overhang() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let (mut x_client, _y_client, x, _y, overlap) = overhang_under_other_window(&mut comp);
    // The overhang used to be drawn above all of HEADLESS-1's windows.
    wait_drawn(&mut comp, "HEADLESS-1", overlap, BLUE, "y over the overhang");
    // And clicks there reached x.
    x_client.sync().unwrap();
    let start = x_client.history.len();
    comp.click(overlap.center()).unwrap();
    x_client.sync().unwrap();
    assert!(
        !x_client
            .events_since(start)
            .iter()
            .any(|e| e["event"] == "keyboard_enter" || e["event"] == "pointer_enter"),
        "the click went to x: {:?}",
        x_client.events_since(start)
    );
    // x itself is unaffected on its own output.
    let home_part = x.content().intersection(&comp.output("HEADLESS-0").unwrap().geometry);
    wait_drawn(&mut comp, "HEADLESS-0", home_part.unwrap(), GREEN, "x on HEADLESS-0");
}

#[test]
fn focusing_a_spanning_window_brings_its_overhang_back_on_top() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let (mut x_client, _y_client, x, _y, overlap) = overhang_under_other_window(&mut comp);
    wait_drawn(&mut comp, "HEADLESS-1", overlap, BLUE, "y over the overhang");

    comp.click((x.content().x as f64 + 50.0, x.content().y as f64 + 50.0))
        .unwrap();
    x_client
        .wait_event("keyboard_enter", |e| e["event"] == "keyboard_enter")
        .unwrap();
    wait_drawn(&mut comp, "HEADLESS-1", overlap, GREEN, "the overhang over y");
    // Clicks follow what's drawn.
    comp.click(overlap.center()).unwrap();
    x_client
        .wait_event("pointer button on the overhang", |e| {
            e["event"] == "pointer_button"
        })
        .unwrap();
}

#[test]
fn windows_reaching_onto_each_others_outputs_stack_the_same_on_both() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    // x belongs to HEADLESS-0 and reaches onto HEADLESS-1, z the other way round, and they
    // overlap on both outputs: 1180..1380 x 236..436.
    let _x = window_at(&mut comp, "x", GREEN, (980, 100));
    let _z = comp.spawn_client("z", &["--color", BLUE]).unwrap();
    comp.wait_window("z", "mapped", |_| true).unwrap();
    let z = drag_by(&mut comp, "z", 200.0, (1180, 200));
    let x = comp.window("x").unwrap();
    assert_eq!((x.output.as_str(), z.output.as_str()), ("HEADLESS-0", "HEADLESS-1"));
    let overlap = x.content().intersection(&z.content()).unwrap();

    // z was raised last.
    wait_drawn(&mut comp, "HEADLESS-0", overlap, BLUE, "z on top on HEADLESS-0");
    wait_drawn(&mut comp, "HEADLESS-1", overlap, BLUE, "z on top on HEADLESS-1");

    comp.click((x.content().x as f64 + 50.0, x.content().y as f64 + 50.0))
        .unwrap();
    wait_drawn(&mut comp, "HEADLESS-0", overlap, GREEN, "x on top on HEADLESS-0");
    wait_drawn(&mut comp, "HEADLESS-1", overlap, GREEN, "x on top on HEADLESS-1");
}
