// SPDX-License-Identifier: GPL-3.0-only

//! Output arrangements other than a single row: explicit positions via `WxH+X+Y`.

use cosmic_comp_test_harness::*;

/// Two 1920x1080 outputs at the bottom and one centred above them:
///
/// ```text
///           +-----------+
///           |     2     |
/// +---------+-+-------+-+---------+
/// |     0     |       1           |
/// +-----------+-------------------+
/// ```
const PYRAMID: &str = "1920x1080+0+1080,1920x1080+1920+1080,1920x1080+960+0";

fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn explicit_positions_are_applied() {
    let mut comp = Compositor::start(Options::default().outputs(PYRAMID)).unwrap();
    let outputs = comp
        .outputs()
        .unwrap()
        .into_iter()
        .map(|o| (o.name, o.geometry))
        .collect::<Vec<_>>();
    assert_eq!(
        outputs,
        vec![
            ("HEADLESS-0".to_string(), rect(0, 1080, 1920, 1080)),
            ("HEADLESS-1".to_string(), rect(1920, 1080, 1920, 1080)),
            ("HEADLESS-2".to_string(), rect(960, 0, 1920, 1080)),
        ]
    );
}

#[test]
fn pointer_crosses_between_stacked_outputs() {
    let mut comp = Compositor::start(Options::default().outputs(PYRAMID)).unwrap();
    for point in [(100.0, 2000.0), (1500.0, 500.0), (3000.0, 1500.0)] {
        comp.pointer_motion(point).unwrap();
        assert_eq!(comp.pointer().unwrap(), point);
    }
    // Left of the top output there is no output to move to.
    assert!(comp.pointer_motion((100.0, 500.0)).is_err());
}

#[test]
fn window_can_be_dragged_up_onto_the_top_output() {
    let mut comp = Compositor::start(Options::default().outputs(PYRAMID)).unwrap();
    let _client = comp.spawn_client("w", &[]).unwrap();
    comp.wait_window("w", "mapped", |_| true).unwrap();
    // Below the top output first, then straight up across the shared edge. The top edge of
    // an output is a maximize snap zone, which must not hold the window back.
    comp.drag_window_to("w", (1300, 1300)).unwrap();
    let window = comp.drag_window_to("w", (1300, 200)).unwrap();
    assert_eq!(window.output, "HEADLESS-2");
    assert!(!window.maximized, "{window:?}");
}
