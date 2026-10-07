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
    comp
        .wait_screenshot(output, "popup drawn", |image| {
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
