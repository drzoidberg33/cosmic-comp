// SPDX-License-Identifier: GPL-3.0-only

//! Resizing floating windows by their edges across output boundaries, with relative pointer
//! motion like a real mouse.

use cosmic_comp_test_harness::*;

const GREEN: &str = "00ff00";
const PYRAMID: &str = "1920x1080+0+1080,1920x1080+1920+1080,1920x1080+960+0";

/// The resize border around server-side decorated windows is 10px wide; grab it in the middle.
const BORDER: f64 = 5.0;

fn window_at(comp: &mut Compositor, title: &str, at: (i32, i32)) -> (Client, WindowInfo) {
    let client = comp.spawn_client(title, &["--color", GREEN]).unwrap();
    comp.wait_window(title, "mapped", |_| true).unwrap();
    let window = comp.drag_window_to(title, at).unwrap();
    (client, window)
}

#[test]
fn right_edge_can_be_dragged_onto_the_next_output() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let (mut client, window) = window_at(&mut comp, "w", (700, 100));
    let right = window.geometry.right() as f64;
    let y = window.geometry.y as f64 + 150.0;

    comp.drag_relative((right + BORDER, y), (1500.0 + BORDER, y), 40)
        .unwrap();
    let resized = comp
        .wait_window("w", "resized to x = 1500", |w| w.geometry.right() == 1500)
        .unwrap();
    assert_eq!(resized.geometry.x, 700, "{resized:?}");
    assert_eq!(resized.output, "HEADLESS-0");
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
}

#[test]
fn bottom_edge_can_be_dragged_down_onto_a_lower_output() {
    let mut comp = Compositor::start(Options::default().outputs(PYRAMID)).unwrap();
    // Onto the top output via a point below it: a straight drag would cross the gap left of it.
    let (mut client, _) = window_at(&mut comp, "w", (1300, 1300));
    let window = comp.drag_window_to("w", (1300, 600)).unwrap();
    assert_eq!(window.output, "HEADLESS-2");
    let bottom = window.geometry.bottom() as f64;
    let x = window.geometry.x as f64 + 200.0;

    comp.drag_relative((x, bottom + BORDER), (x, 1400.0 + BORDER), 40)
        .unwrap();
    comp.wait_window("w", "resized to y = 1400", |w| w.geometry.bottom() == 1400)
        .unwrap();
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-2"])
        .unwrap();
}

#[test]
fn resizing_back_and_forth_across_a_seam_follows_the_pointer() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let (mut client, window) = window_at(&mut comp, "w", (700, 100));
    let right = window.geometry.right() as f64;
    let y = window.geometry.y as f64 + 150.0;

    // Out onto HEADLESS-1, back onto HEADLESS-0 and out again, in one grab.
    comp.pointer_motion((right + BORDER, y)).unwrap();
    comp.button("left", true).unwrap();
    let mut x = right + BORDER;
    for target in [1600.0, 1150.0, 1450.0, 1200.0] {
        while (x - (target + BORDER)).abs() > 0.5 {
            let step = ((target + BORDER) - x).clamp(-20.0, 20.0);
            comp.pointer_relative((step, 0.0)).unwrap();
            x += step;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            comp.pointer().unwrap(),
            (x, y),
            "pointer held back near {target}"
        );
    }
    comp.button("left", false).unwrap();

    comp.wait_window("w", "resized to x = 1200", |w| w.geometry.right() == 1200)
        .unwrap();
    client.wait_entered_outputs(&["HEADLESS-0"]).unwrap();
}

#[test]
fn spanning_window_can_be_resized_from_its_overhanging_edge() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    // 800..1400 spans the seam at x = 1280, so its right edge is on HEADLESS-1. Wide enough to
    // stay above the minimum width (360) when shrunk back across the seam.
    let mut client = comp
        .spawn_client("w", &["--color", GREEN, "--width", "600"])
        .unwrap();
    comp.wait_window("w", "mapped", |_| true).unwrap();
    let window = comp.drag_window_to("w", (800, 100)).unwrap();
    assert_eq!(window.geometry.right(), 1400);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
    let right = window.geometry.right() as f64;
    let y = window.geometry.y as f64 + 150.0;

    // Grab the edge on HEADLESS-1 and pull it back across the seam onto HEADLESS-0.
    comp.drag_relative((right + BORDER, y), (1250.0 + BORDER, y), 30)
        .unwrap();
    comp.wait_window("w", "resized to x = 1250", |w| w.geometry.right() == 1250)
        .unwrap();
    client.wait_entered_outputs(&["HEADLESS-0"]).unwrap();
}
