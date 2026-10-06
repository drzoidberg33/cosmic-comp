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

#[test]
fn window_shrunk_off_its_home_output_moves_to_the_other_output_in_place() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let mut client = comp
        .spawn_client("w", &["--color", GREEN, "--width", "1000"])
        .unwrap();
    let mapped = comp.wait_window("w", "mapped", |_| true).unwrap();
    // Drop it by its title bar 700px in, at x = 1400 on HEADLESS-1: 700..1552 belongs to
    // HEADLESS-1 and reaches onto HEADLESS-0.
    let grab = 700.0;
    let from = (
        mapped.geometry.x as f64 + grab,
        mapped.geometry.y as f64 + 18.0,
    );
    comp.drag(from, (700.0 + grab, 100.0 + 18.0), 30).unwrap();
    let window = comp
        .wait_window("w", "placed", |w| {
            (w.geometry.x, w.geometry.y) == (700, 100)
        })
        .unwrap();
    assert_eq!(window.output, "HEADLESS-1");

    // Pull the right edge back until the window is entirely on HEADLESS-0.
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
    let start = client.history.len();
    let y = window.geometry.y as f64 + 150.0;
    comp.pointer_motion((window.geometry.right() as f64 + BORDER, y))
        .unwrap();
    comp.button("left", true).unwrap();
    for _ in 0..40 {
        comp.pointer_relative((-10.0, 0.0)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let during = comp.window("w").unwrap();
        // It used to jump elsewhere as soon as it no longer touched its home output.
        assert_eq!(
            (during.geometry.x, during.geometry.y),
            (700, 100),
            "{during:?}"
        );
    }
    comp.button("left", false).unwrap();

    let moved = comp
        .wait_window("w", "on HEADLESS-0", |w| w.output == "HEADLESS-0")
        .unwrap();
    assert_eq!((moved.geometry.x, moved.geometry.y), (700, 100));
    assert!(moved.geometry.right() < 1280, "{moved:?}");
    client.wait_entered_outputs(&["HEADLESS-0"]).unwrap();
    assert!(
        !client
            .events_since(start)
            .iter()
            .any(|e| e["event"] == "keyboard_leave"),
        "lost keyboard focus: {:?}",
        client.events_since(start)
    );
    // It stayed visible on HEADLESS-0 throughout, so no leave/enter round trip.
    assert!(
        !client
            .events_since(start)
            .iter()
            .any(|e| e["event"] == "leave" && e["output"] == "HEADLESS-0"),
        "{:?}",
        client.events_since(start)
    );
}

/// A frame-paced client (it only draws again once its last frame was presented, like GPU
/// toolkits) placed exactly half on each output: 880..1680 with the seam at 1280.
fn frame_paced_half_and_half(comp: &mut Compositor) -> (Client, WindowInfo) {
    let mut client = comp
        .spawn_client("w", &["--frame-paced", "--width", "800", "--height", "500"])
        .unwrap();
    comp.wait_window("w", "mapped", |_| true).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mapped = comp.window("w").unwrap();
    let grab = 200.0;
    let from = (
        mapped.geometry.x as f64 + grab,
        mapped.geometry.y as f64 + 18.0,
    );
    comp.drag(from, (880.0 + grab, 100.0 + 18.0), 30).unwrap();
    let window = comp
        .wait_window("w", "placed", |w| {
            (w.geometry.x, w.geometry.y) == (880, 100)
        })
        .unwrap();
    client.sync().unwrap();
    (client, window)
}

/// Grows `window` onto HEADLESS-1 by its right edge, first by `head_start` px at once, then for
/// a second with ~125 relative motion events per second like a mouse. Returns the client's
/// frame callbacks during that second.
fn resize_for_a_second(
    comp: &mut Compositor,
    client: &mut Client,
    window: &WindowInfo,
    head_start: f64,
) -> usize {
    let y = window.geometry.y as f64 + 200.0;
    comp.pointer_motion((window.geometry.right() as f64 + BORDER, y))
        .unwrap();
    comp.button("left", true).unwrap();
    for _ in 0..(head_start / 20.0) as usize {
        comp.pointer_relative((20.0, 0.0)).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    let start = client.history.len();
    let begin = std::time::Instant::now();
    while begin.elapsed() < std::time::Duration::from_secs(1) {
        comp.pointer_relative((2.0, 0.0)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    comp.button("left", false).unwrap();
    client.sync().unwrap();
    client
        .events_since(start)
        .iter()
        .filter(|e| e["event"] == "frame")
        .count()
}

#[test]
fn overhang_is_redrawn_while_a_spanning_window_is_resized() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let (mut client, window) = frame_paced_half_and_half(&mut comp);
    let before = comp.outputs().unwrap();

    let frames = resize_for_a_second(&mut comp, &mut client, &window, 0.0);

    let after = comp.outputs().unwrap();
    let redraws = |name: &str| {
        let count =
            |outputs: &[OutputInfo]| outputs.iter().find(|o| o.name == name).unwrap().renders;
        count(&after) - count(&before)
    };
    let (home, other) = (redraws("HEADLESS-0"), redraws("HEADLESS-1"));
    // Commits used to only redraw the window's own output, freezing the overhang (and
    // stalling frame callbacks whenever the other output was the window's primary one).
    assert!(
        other * 10 >= home * 8,
        "HEADLESS-1 redrawn {other} times, HEADLESS-0 {home} times"
    );
    assert!(frames >= 30, "only {frames} frames in a second of resizing");
}

#[test]
fn window_mostly_on_another_output_keeps_getting_frames_while_resized() {
    let mut comp = Compositor::start(Options::default()).unwrap();
    let (mut client, window) = frame_paced_half_and_half(&mut comp);
    // 400px stay on HEADLESS-0, 1000px end up on HEADLESS-1, making it the primary output.
    let frames = resize_for_a_second(&mut comp, &mut client, &window, 600.0);
    let window = comp.window("w").unwrap();
    assert_eq!(window.output, "HEADLESS-0", "{window:?}");
    assert_eq!(window.primary_output.as_deref(), Some("HEADLESS-1"), "{window:?}");
    assert!(frames >= 30, "only {frames} frames in a second of resizing");
}
