// SPDX-License-Identifier: GPL-3.0-only

//! The fork's feature: floating windows that straddle two outputs are shown, and receive input,
//! on both (pop-os/cosmic-epoch#2273).

use cosmic_comp_test_harness::*;

const GREEN: &str = "00ff00";
const TITLE: &str = "spanning";

/// Starts two side-by-side 1280x720 outputs and drags a 400x300 window so that `overhang`
/// logical pixels of it reach into HEADLESS-1, while its title bar (and the cursor at drop)
/// stays on HEADLESS-0.
fn spanning_window(options: Options, overhang: i32) -> (Compositor, Client, WindowInfo) {
    let mut comp = Compositor::start(options).unwrap();
    let client = comp.spawn_client(TITLE, &["--color", GREEN]).unwrap();
    comp.wait_window(TITLE, "mapped", |_| true).unwrap();
    let window = comp
        .drag_window_to(TITLE, (1280 + overhang - 400, 100))
        .unwrap();
    (comp, client, window)
}

fn assert_shows_window(comp: &mut Compositor, output: &str, window: &WindowInfo) {
    let info = comp.output(output).unwrap();
    let visible = window
        .content()
        .intersection(&info.geometry)
        .unwrap_or_else(|| panic!("window {window:?} does not intersect {output}"));
    let shot = comp.screenshot(output).unwrap();
    let coverage = shot.coverage(to_output_pixels(visible, &info), rgb(GREEN), 2);
    assert!(
        coverage > 0.95,
        "{output}: only {:.1}% of the window's visible part {visible:?} is drawn, \
         artifacts in {}",
        coverage * 100.0,
        comp.dir().display()
    );
}

#[test]
fn spanning_window_keeps_its_position() {
    let (_comp, _client, window) = spanning_window(Options::default(), 150);
    assert_eq!(
        (window.geometry.x, window.geometry.y),
        (1280 + 150 - 400, 100)
    );
    assert!(window.floating);
}

#[test]
fn spanning_window_is_drawn_on_both_outputs() {
    let (mut comp, _client, window) = spanning_window(Options::default(), 150);
    assert_shows_window(&mut comp, "HEADLESS-0", &window);
    assert_shows_window(&mut comp, "HEADLESS-1", &window);
}

#[test]
fn spanning_window_enters_both_outputs() {
    let (_comp, mut client, _window) = spanning_window(Options::default(), 150);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
}

#[test]
fn pointer_on_overhang_reaches_window() {
    let (mut comp, mut client, window) = spanning_window(Options::default(), 150);
    let content = window.content();
    // 100px into HEADLESS-1, inside the part of the window hanging over the seam.
    let point = (1380.0, content.y as f64 + 50.0);
    let start = client.history.len();

    comp.pointer_motion(point).unwrap();
    let enter = client
        .wait_event("pointer_enter", |e| e["event"] == "pointer_enter")
        .unwrap();
    assert_eq!(
        enter["x"].as_f64(),
        Some(point.0 - content.x as f64),
        "{enter}"
    );
    assert_eq!(enter["y"].as_f64(), Some(50.0), "{enter}");

    comp.button("left", true).unwrap();
    comp.button("left", false).unwrap();
    client
        .wait_event("button press", |e| {
            e["event"] == "pointer_button" && e["pressed"] == true
        })
        .unwrap();
    assert!(
        !client
            .events_since(start)
            .iter()
            .any(|e| e["event"] == "pointer_leave"),
        "{:?}",
        client.events_since(start)
    );
}

#[test]
fn window_moved_off_the_seam_leaves_the_other_output() {
    let (mut comp, mut client, _window) = spanning_window(Options::default(), 150);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();

    let window = comp.drag_window_to(TITLE, (200, 100)).unwrap();
    assert_eq!(window.output, "HEADLESS-0");
    client.wait_entered_outputs(&["HEADLESS-0"]).unwrap();
    let shot = comp.screenshot("HEADLESS-1").unwrap();
    assert_eq!(shot.total_coverage(rgb(GREEN), 2), 0.0);
}

#[test]
fn spanning_window_is_drawn_on_both_outputs_with_workspaces_spanning_displays() {
    let options = Options::default().config(
        "com.system76.CosmicComp",
        "workspaces",
        "(workspace_mode: Global, workspace_layout: Vertical)",
    );
    let (mut comp, mut client, window) = spanning_window(options, 150);
    assert_shows_window(&mut comp, "HEADLESS-0", &window);
    assert_shows_window(&mut comp, "HEADLESS-1", &window);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
}

fn overhang_coverage(comp: &mut Compositor, window: &WindowInfo, image: &Image) -> f64 {
    let info = comp.output("HEADLESS-1").unwrap();
    let visible = window.content().intersection(&info.geometry).unwrap();
    image.coverage(to_output_pixels(visible, &info), rgb(GREEN), 2)
}

#[test]
fn spanning_window_is_drawn_at_the_other_outputs_scale() {
    let options = Options::default().outputs("1280x720,1280x720@2");
    let (mut comp, mut client, window) = spanning_window(options, 150);
    assert_eq!(comp.output("HEADLESS-1").unwrap().scale, 2.0);
    assert_shows_window(&mut comp, "HEADLESS-0", &window);
    assert_shows_window(&mut comp, "HEADLESS-1", &window);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
}

#[test]
fn switching_the_home_workspace_hides_the_overhang() {
    use keys::*;
    let (mut comp, mut client, window) = spanning_window(Options::default(), 150);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();

    // Super+2 switches the workspace of the output the pointer is on.
    comp.pointer_motion((100.0, 600.0)).unwrap();
    comp.chord(&[KEY_LEFTMETA, KEY_2]).unwrap();
    comp.wait_screenshot("HEADLESS-1", "no overhang", |image| {
        image.total_coverage(rgb(GREEN), 2) == 0.0
    })
    .unwrap();
    client
        .wait_outputs_where("off HEADLESS-1", |outputs| !outputs.contains("HEADLESS-1"))
        .unwrap();

    comp.chord(&[KEY_LEFTMETA, KEY_1]).unwrap();
    comp.wait_screenshot("HEADLESS-1", "the overhang again", |image| {
        image.total_coverage(rgb(GREEN), 2) > 0.0
    })
    .unwrap();
    // Wait out the workspace animation, then check the overhang is back in place.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let image = comp.screenshot("HEADLESS-1").unwrap();
    assert!(overhang_coverage(&mut comp, &window, &image) > 0.95);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
}

#[test]
fn switching_the_other_outputs_workspace_keeps_the_overhang() {
    use keys::*;
    let (mut comp, mut client, window) = spanning_window(Options::default(), 150);

    comp.pointer_motion((2000.0, 600.0)).unwrap();
    comp.chord(&[KEY_LEFTMETA, KEY_2]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let image = comp.screenshot("HEADLESS-1").unwrap();
    assert!(
        overhang_coverage(&mut comp, &window, &image) > 0.95,
        "artifacts in {}",
        comp.dir().display()
    );
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1"])
        .unwrap();
}

#[test]
fn window_can_be_dragged_by_the_overhanging_part_of_its_title_bar() {
    let (mut comp, mut client, window) = spanning_window(Options::default(), 250);
    // A point on the title bar, on HEADLESS-1.
    let from = (1280.0 + 100.0, window.geometry.y as f64 + 18.0);
    comp.drag(from, (from.0 - 600.0, from.1), 20).unwrap();
    let moved = comp
        .wait_window(TITLE, "moved left by 600", |w| {
            w.geometry.x == window.geometry.x - 600
        })
        .unwrap();
    assert_eq!(moved.output, "HEADLESS-0");
    client.wait_entered_outputs(&["HEADLESS-0"]).unwrap();
}

#[test]
fn clicking_the_overhang_focuses_the_window() {
    let (mut comp, mut client, window) = spanning_window(Options::default(), 150);
    let mut other = comp.spawn_client("other", &["--color", "0000ff"]).unwrap();
    other
        .wait_event("keyboard_enter", |e| e["event"] == "keyboard_enter")
        .unwrap();
    client
        .wait_event("keyboard_leave", |e| e["event"] == "keyboard_leave")
        .unwrap();

    let point = (1380.0, window.content().y as f64 + 100.0);
    comp.pointer_motion(point).unwrap();
    comp.button("left", true).unwrap();
    comp.button("left", false).unwrap();
    client
        .wait_event("keyboard_enter", |e| e["event"] == "keyboard_enter")
        .unwrap();
}

/// Two 1920x1080 outputs at the bottom and one centred above them, see `tests/layouts.rs`.
const PYRAMID: &str = "1920x1080+0+1080,1920x1080+1920+1080,1920x1080+960+0";

#[test]
fn window_spans_a_horizontal_seam() {
    let mut comp = Compositor::start(Options::default().outputs(PYRAMID)).unwrap();
    let mut client = comp.spawn_client("w", &["--color", GREEN]).unwrap();
    comp.wait_window("w", "mapped", |_| true).unwrap();
    // Title bar on the top output, the lower part reaching down onto HEADLESS-0.
    let window = comp.drag_window_to("w", (1200, 900)).unwrap();
    assert_eq!(window.output, "HEADLESS-2");
    assert_shows_window(&mut comp, "HEADLESS-2", &window);
    assert_shows_window(&mut comp, "HEADLESS-0", &window);
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-2"])
        .unwrap();
}

#[test]
fn window_spans_three_outputs() {
    let mut comp = Compositor::start(Options::default().outputs(PYRAMID)).unwrap();
    let mut client = comp.spawn_client("w", &["--color", GREEN]).unwrap();
    comp.wait_window("w", "mapped", |_| true).unwrap();
    // Straddles the top output and the seam between the two bottom ones.
    let window = comp.drag_window_to("w", (1720, 900)).unwrap();
    for output in ["HEADLESS-0", "HEADLESS-1", "HEADLESS-2"] {
        assert_shows_window(&mut comp, output, &window);
    }
    client
        .wait_entered_outputs(&["HEADLESS-0", "HEADLESS-1", "HEADLESS-2"])
        .unwrap();

    // Input on each of the overhangs reaches the window.
    let content = window.content();
    for point in [(1800.0, 1150.0), (2000.0, 1150.0)] {
        let start = client.history.len();
        comp.pointer_motion(point).unwrap();
        client.sync().unwrap();
        let motion = client
            .events_since(start)
            .iter()
            .rev()
            .find(|e| e["event"] == "pointer_motion" || e["event"] == "pointer_enter");
        let motion = motion.unwrap_or_else(|| panic!("no pointer event at {point:?}"));
        assert_eq!(
            motion["x"].as_f64(),
            Some(point.0 - content.x as f64),
            "{motion}"
        );
        assert_eq!(
            motion["y"].as_f64(),
            Some(point.1 - content.y as f64),
            "{motion}"
        );
    }
}
