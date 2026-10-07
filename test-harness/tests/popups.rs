// SPDX-License-Identifier: GPL-3.0-only

//! Popups (menus) of floating windows spanning outputs.

use cosmic_comp_test_harness::*;

const GREEN: &str = "00ff00";
const MAGENTA: &str = "ff00ff";
const POPUP: (u32, u32) = (200, 150);

/// Two side-by-side 1280x720 outputs (unless `outputs` says otherwise) and a 400x300 window
/// belonging to HEADLESS-0 that reaches 150px onto HEADLESS-1: its client area is
/// 1030..1430 x 136..436, the seam at surface x = 250.
fn spanning_window(outputs: Option<&str>) -> (Compositor, Client, WindowInfo) {
    let mut options = Options::default();
    if let Some(outputs) = outputs {
        options = options.outputs(outputs);
    }
    let mut comp = Compositor::start(options).unwrap();
    let client = comp.spawn_client("w", &["--color", GREEN]).unwrap();
    comp.wait_window("w", "mapped", |_| true).unwrap();
    let window = comp.drag_window_to("w", (1030, 100)).unwrap();
    assert_eq!(window.output, "HEADLESS-0");
    (comp, client, window)
}

/// The popup's area in global coordinates, from its position relative to the window surface.
fn popup_rect(window: &WindowInfo, at: (i32, i32)) -> Rect {
    let content = window.content();
    Rect {
        x: content.x + at.0,
        y: content.y + at.1,
        width: POPUP.0 as i32,
        height: POPUP.1 as i32,
    }
}

fn assert_drawn(comp: &mut Compositor, output: &str, rect: Rect) {
    let info = comp.output(output).unwrap();
    let pixels = to_output_pixels(rect, &info);
    comp.wait_screenshot(output, "popup drawn", |image| {
        image.coverage(pixels, rgb(MAGENTA), 2) > 0.95
    })
    .unwrap_or_else(|err| panic!("{output}: {err}"));
}

#[test]
fn popup_from_the_home_part_stays_on_the_home_output() {
    let (mut comp, mut client, window) = spanning_window(None);
    let at = client.open_popup((20, 50), POPUP, MAGENTA, false).unwrap();
    assert_eq!(at, (20, 50));
    assert_drawn(&mut comp, "HEADLESS-0", popup_rect(&window, at));
}

#[test]
fn popup_from_the_overhang_opens_on_the_other_output() {
    let (mut comp, mut client, window) = spanning_window(None);
    // Anchored at x = 1360 on HEADLESS-1. It used to be pushed back onto HEADLESS-0.
    let at = client.open_popup((330, 50), POPUP, MAGENTA, false).unwrap();
    assert_eq!(at, (330, 50));
    let rect = popup_rect(&window, at);
    assert!(rect.x >= 1280, "{rect:?}");
    assert_drawn(&mut comp, "HEADLESS-1", rect);
    client
        .wait_event("popup enters HEADLESS-1", |e| {
            e["event"] == "popup_enter" && e["output"] == "HEADLESS-1"
        })
        .unwrap();
}

#[test]
fn popup_on_the_other_output_is_kept_within_it() {
    let (_comp, mut client, _window) = spanning_window(None);
    // Anchored at x = 1400 on HEADLESS-1, near the bottom: it has to slide up to fit on
    // HEADLESS-1 (and stays on it, rather than being moved to HEADLESS-0).
    let at = client
        .open_popup((370, 280), POPUP, MAGENTA, false)
        .unwrap();
    assert_eq!(at.0, 370);
    assert!(136 + at.1 + POPUP.1 as i32 <= 720, "{at:?}");
}

#[test]
fn clicking_a_menu_on_the_other_output_reaches_it() {
    let (mut comp, mut client, window) = spanning_window(None);
    // Menus grab with the button press that opened them, and are dismissed by clicks
    // outside the client's surfaces.
    let content = window.content();
    comp.pointer_motion((content.x as f64 + 360.0, content.y as f64 + 40.0))
        .unwrap();
    comp.button("right", true).unwrap();
    client
        .wait_event("button press", |e| e["event"] == "pointer_button")
        .unwrap();
    let at = client.open_popup((330, 50), POPUP, MAGENTA, true).unwrap();
    comp.button("right", false).unwrap();
    let rect = popup_rect(&window, at);
    assert_drawn(&mut comp, "HEADLESS-1", rect);

    comp.click(rect.center()).unwrap();
    client
        .wait_event("click on the popup", |e| {
            e["event"] == "popup_pointer_button" && e["pressed"] == true
        })
        .unwrap();
    client.sync().unwrap();
    assert!(
        !client.history.iter().any(|e| e["event"] == "popup_done"),
        "the click dismissed the popup: {:?}",
        client.history
    );
}

#[test]
fn popup_on_a_scaled_output_is_drawn_at_its_scale() {
    let (mut comp, mut client, window) = spanning_window(Some("1280x720,1280x720@2"));
    let at = client.open_popup((330, 50), POPUP, MAGENTA, false).unwrap();
    assert_eq!(at, (330, 50));
    // `assert_drawn` checks the popup's area in HEADLESS-1's physical pixels.
    assert_drawn(&mut comp, "HEADLESS-1", popup_rect(&window, at));
}
