// SPDX-License-Identifier: GPL-3.0-only

//! Fork-only headless backend for automated testing.
//!
//! Renders virtual outputs offscreen and takes input from a control socket instead of real
//! devices, so the compositor can be driven by tests without showing anything on screen.
//!
//! - `COSMIC_HEADLESS_OUTPUTS`: comma separated `WxH[@scale]` entries, one per output
//!   (default `1280x720`). Outputs are named `HEADLESS-0`, `HEADLESS-1`, ...
//! - `COSMIC_HEADLESS_RENDER_NODE`: render node to use (e.g. `/dev/dri/renderD128`). Defaults to
//!   the first EGL device with a render node, falling back to a software (llvmpipe) device.
//! - `COSMIC_HEADLESS_SOFTWARE=1`: force the software device.
//! - `COSMIC_HEADLESS_CONTROL`: path of the control socket, see [`control`].

use crate::{
    backend::render::{self, CursorMode, ScreenFilterStorage, init_shaders},
    config::ScreenFilter,
    state::{BackendData, Common},
    utils::prelude::*,
    wayland::protocols::drm::WlDrmState,
};
use anyhow::{Context, Result, anyhow, bail};
use cosmic_comp_config::output::comp::OutputConfig;
use smithay::{
    backend::{
        allocator::Fourcc,
        drm::{DrmNode, NodeType},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::{
            AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, Device,
            DeviceCapability, Event, InputBackend, InputEvent, InputTime, KeyState,
            KeyboardKeyEvent, Keycode, PointerAxisEvent, PointerButtonEvent,
            PointerMotionAbsoluteEvent, UnusedEvent,
        },
        renderer::{
            Bind, ExportMem, ImportDma, Offscreen,
            damage::{OutputDamageTracker, RenderOutputResult},
            gles::GlesRenderbuffer,
            glow::GlowRenderer,
        },
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::DisplayHandle,
    },
    utils::{Buffer as BufferCoords, Point, Rectangle, Size, Transform},
    wayland::{dmabuf::DmabufFeedbackBuilder, presentation::Refresh},
};
use std::{borrow::BorrowMut, cell::RefCell, path::Path, time::Duration};
use tracing::{error, info, warn};

pub mod control;

const REFRESH_MILLIHZ: i32 = 60_000;
const FRAME_TIME: Duration = Duration::from_micros(16_667);

#[derive(Debug)]
pub struct HeadlessState {
    pub renderer: GlowRenderer,
    surfaces: Vec<Surface>,
    loop_handle: LoopHandle<'static, State>,
    input_device_added: bool,
}

#[derive(Debug)]
struct Surface {
    output: Output,
    buffer: GlesRenderbuffer,
    damage_tracker: OutputDamageTracker,
    screen_filter_state: ScreenFilterStorage,
    render_pending: bool,
}

/// One `WxH[@scale]` entry of `COSMIC_HEADLESS_OUTPUTS`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OutputSpec {
    size: (i32, i32),
    scale: f64,
}

fn parse_output_specs(value: &str) -> Result<Vec<OutputSpec>> {
    value
        .split(',')
        .map(|entry| {
            let entry = entry.trim();
            let (size, scale) = match entry.split_once('@') {
                Some((size, scale)) => (
                    size,
                    scale
                        .parse::<f64>()
                        .with_context(|| format!("Invalid scale in `{entry}`"))?,
                ),
                None => (entry, 1.0),
            };
            let (w, h) = size
                .split_once('x')
                .with_context(|| format!("Expected `WxH[@scale]`, got `{entry}`"))?;
            let size = (
                w.parse::<i32>()
                    .with_context(|| format!("Invalid width in `{entry}`"))?,
                h.parse::<i32>()
                    .with_context(|| format!("Invalid height in `{entry}`"))?,
            );
            if size.0 <= 0 || size.1 <= 0 || scale <= 0.0 {
                bail!("Output size and scale must be positive in `{entry}`");
            }
            Ok(OutputSpec { size, scale })
        })
        .collect()
}

impl HeadlessState {
    fn add_output(&mut self, spec: OutputSpec) -> Result<Output> {
        let name = format!("HEADLESS-{}", self.surfaces.len());
        let props = PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "COSMIC".to_string(),
            model: name.clone(),
            serial_number: "Unknown".to_string(),
        };
        let mode = Mode {
            size: spec.size.into(),
            refresh: REFRESH_MILLIHZ,
        };
        let output = Output::new(name, props);
        output.add_mode(mode);
        output.set_preferred(mode);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(spec.scale)),
            Some((0, 0).into()),
        );
        output.user_data().insert_if_missing(|| {
            RefCell::new(OutputConfig {
                mode: (spec.size, None),
                scale: spec.scale,
                ..Default::default()
            })
        });

        let buffer = self.create_buffer(spec.size)?;
        self.surfaces.push(Surface {
            damage_tracker: OutputDamageTracker::from_output(&output),
            output: output.clone(),
            buffer,
            screen_filter_state: ScreenFilterStorage::default(),
            render_pending: false,
        });
        self.schedule_render(&output);

        Ok(output)
    }

    fn create_buffer(&mut self, size: (i32, i32)) -> Result<GlesRenderbuffer> {
        Offscreen::<GlesRenderbuffer>::create_buffer(
            &mut self.renderer,
            Fourcc::Abgr8888,
            Size::<i32, BufferCoords>::from(size),
        )
        .context("Failed to create offscreen buffer")
    }

    pub fn schedule_render(&mut self, output: &Output) {
        let Some(surface) = self.surfaces.iter_mut().find(|s| s.output == *output) else {
            return;
        };
        if surface.render_pending {
            return;
        }
        surface.render_pending = true;

        let output = output.clone();
        if let Err(err) =
            self.loop_handle
                .insert_source(Timer::from_duration(FRAME_TIME), move |_, _, state| {
                    if let BackendData::Headless(headless) = &mut state.backend
                        && let Err(err) = headless.render(&output, &mut state.common, None)
                    {
                        error!(?err, "Error rendering.");
                    }
                    TimeoutAction::Drop
                })
        {
            error!(?err, "Failed to schedule render.");
        }
    }

    /// Renders `output` and optionally writes the result to `capture` as a PNG.
    pub fn render(
        &mut self,
        output: &Output,
        common: &mut Common,
        capture: Option<&Path>,
    ) -> Result<()> {
        let surface = self
            .surfaces
            .iter_mut()
            .find(|s| s.output == *output)
            .with_context(|| format!("Unknown output {}", output.name()))?;
        surface.render_pending = false;

        let size = output
            .current_mode()
            .map(|mode| (mode.size.w, mode.size.h))
            .context("Output has no mode")?;
        let renderer = &mut self.renderer;
        let mut fb = renderer
            .bind(&mut surface.buffer)
            .context("Failed to bind offscreen buffer")?;
        let RenderOutputResult { damage, states, .. } = render::render_output(
            None,
            renderer,
            &mut fb,
            &mut surface.damage_tracker,
            // Always redraw everything, so captures never depend on buffer history.
            0,
            &common.shell,
            common.clock.now(),
            output,
            CursorMode::None,
            &mut surface.screen_filter_state,
            &common.event_loop_handle,
        )
        .map_err(|err| anyhow!("Rendering failed: {err}"))?;

        if let Some(path) = capture {
            let mapping = renderer
                .copy_framebuffer(
                    &fb,
                    Rectangle::from_size(Size::<i32, BufferCoords>::from(size)),
                    Fourcc::Abgr8888,
                )
                .context("Failed to read back framebuffer")?;
            let data = renderer
                .map_texture(&mapping)
                .context("Failed to map framebuffer")?;
            write_png(path, size, data)?;
        }
        std::mem::drop(fb);

        common.send_frames(output, None);
        common.update_primary_output(output, &states);
        common.send_dmabuf_feedback(output, &states, |_| None);
        if damage.is_some() {
            let mut feedback = common
                .shell
                .read()
                .take_presentation_feedback(output, &states);
            feedback.presented(
                common.clock.now(),
                Refresh::Fixed(FRAME_TIME),
                0,
                wp_presentation_feedback::Kind::empty(),
            );
        }

        Ok(())
    }

    pub fn all_outputs(&self) -> Vec<Output> {
        self.surfaces.iter().map(|s| s.output.clone()).collect()
    }

    pub fn apply_config_for_outputs(&mut self, test_only: bool) -> Result<()> {
        // Modes are fixed by `COSMIC_HEADLESS_OUTPUTS`; scale and position are applied generically.
        let mut result = Ok(());
        for surface in &self.surfaces {
            let Some(mode) = surface.output.current_mode() else {
                continue;
            };
            let mut config = surface.output.config_mut();
            if config.mode.0 != (mode.size.w, mode.size.h) {
                if !test_only {
                    config.mode = ((mode.size.w, mode.size.h), None);
                }
                result = Err(anyhow!("Cannot change the mode of a headless output"));
            }
        }
        result
    }

    pub fn update_screen_filter(&mut self, screen_filter: &ScreenFilter) -> Result<()> {
        for surface in &mut self.surfaces {
            surface.screen_filter_state.filter = screen_filter.clone();
        }
        Ok(())
    }

    /// Feeds a synthetic input event through the regular input path.
    pub fn inject_input(state: &mut State, event: InputEvent<HeadlessInput>) {
        if let BackendData::Headless(headless) = &mut state.backend
            && !headless.input_device_added
        {
            headless.input_device_added = true;
            state.process_input_event(
                InputEvent::<HeadlessInput>::DeviceAdded {
                    device: HeadlessDevice,
                },
                crate::input::InputBackendId::Normal,
            );
        }

        state.process_input_event(event, crate::input::InputBackendId::Normal);

        let outputs = state
            .common
            .shell
            .read()
            .outputs()
            .cloned()
            .collect::<Vec<_>>();
        for output in outputs {
            state.backend.schedule_render(&output);
        }
    }

    /// Moves the pointer to `position` in global logical coordinates.
    pub fn pointer_motion(state: &mut State, position: Point<f64, Global>) -> Result<()> {
        let output = state
            .common
            .shell
            .read()
            .outputs()
            .find(|o| o.geometry().to_f64().contains(position))
            .cloned()
            .with_context(|| format!("No output at {position:?}"))?;
        let geometry = output.geometry().to_f64();
        let relative = (
            (position.x - geometry.loc.x) / geometry.size.w,
            (position.y - geometry.loc.y) / geometry.size.h,
        );

        for seat in state.common.shell.read().seats.iter() {
            seat.set_active_output(&output);
        }

        Self::inject_input(
            state,
            InputEvent::PointerMotionAbsolute {
                event: HeadlessMotionEvent {
                    time: InputTime::now(),
                    relative,
                },
            },
        );
        Ok(())
    }

    pub fn pointer_button(state: &mut State, button: u32, pressed: bool) {
        Self::inject_input(
            state,
            InputEvent::PointerButton {
                event: HeadlessButtonEvent {
                    time: InputTime::now(),
                    button,
                    state: if pressed {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                },
            },
        );
    }

    pub fn pointer_axis(state: &mut State, horizontal: f64, vertical: f64) {
        Self::inject_input(
            state,
            InputEvent::PointerAxis {
                event: HeadlessAxisEvent {
                    time: InputTime::now(),
                    horizontal,
                    vertical,
                },
            },
        );
    }

    /// `key` is an evdev key code (see `linux/input-event-codes.h`).
    pub fn key(state: &mut State, key: u32, pressed: bool) {
        Self::inject_input(
            state,
            InputEvent::Keyboard {
                event: HeadlessKeyEvent {
                    time: InputTime::now(),
                    key,
                    state: if pressed {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    },
                },
            },
        );
    }
}

fn write_png(path: &Path, size: (i32, i32), data: &[u8]) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("Failed to create {}", path.display()))?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, size.0 as u32, size.1 as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(data)?;
    Ok(())
}

fn select_egl_device() -> Result<EGLDevice> {
    let force_software = std::env::var_os("COSMIC_HEADLESS_SOFTWARE").is_some_and(|v| v != "0");
    let requested_node = std::env::var("COSMIC_HEADLESS_RENDER_NODE")
        .ok()
        .map(|path| DrmNode::from_path(&path).with_context(|| format!("Invalid node {path}")))
        .transpose()?;

    let devices = EGLDevice::enumerate()
        .context("Failed to enumerate EGL devices")?
        .collect::<Vec<_>>();
    let is_software = |device: &EGLDevice| {
        device
            .extensions()
            .iter()
            .any(|ext| ext == "EGL_MESA_device_software")
    };
    let render_node = |device: &EGLDevice| device.try_get_render_node().ok().flatten();

    let hardware = || {
        devices.iter().find(|device| {
            !is_software(device)
                && render_node(device).is_some_and(|node| {
                    requested_node.is_none_or(|requested| {
                        requested.dev_id() == node.dev_id()
                            || requested
                                .node_with_type(NodeType::Render)
                                .and_then(Result::ok)
                                .is_some_and(|r| r == node)
                    })
                })
        })
    };
    let software = || devices.iter().find(|device| is_software(device));

    let device = if force_software {
        software()
    } else {
        hardware().or_else(|| {
            if requested_node.is_some() {
                None
            } else {
                warn!("No hardware EGL device found, falling back to software rendering.");
                software()
            }
        })
    };
    device
        .cloned()
        .context("No usable EGL device for the headless backend")
}

fn init_egl_client_side(
    dh: &DisplayHandle,
    state: &mut State,
    render_node: DrmNode,
    renderer: &mut GlowRenderer,
) -> Result<()> {
    let default_feedback =
        DmabufFeedbackBuilder::new(render_node.dev_id(), renderer.dmabuf_formats())
            .build()
            .context("Failed to build dmabuf feedback")?;
    let dmabuf_global = state
        .common
        .dmabuf_state
        .create_global_with_default_feedback::<State>(dh, &default_feedback);
    state.common.wl_drm_state = Some(WlDrmState::new::<State>(
        dh,
        render_node
            .dev_path_with_type(NodeType::Render)
            .or_else(|| render_node.dev_path())
            .ok_or(anyhow!(
                "Could not determine path for gpu node: {}",
                render_node
            ))?,
        renderer.dmabuf_formats(),
        &dmabuf_global,
    ));
    Ok(())
}

pub fn init_backend(
    dh: &DisplayHandle,
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
) -> Result<()> {
    let specs = match std::env::var("COSMIC_HEADLESS_OUTPUTS") {
        Ok(value) => parse_output_specs(&value)?,
        Err(_) => vec![OutputSpec {
            size: (1280, 720),
            scale: 1.0,
        }],
    };

    let device = select_egl_device()?;
    let render_node = device.try_get_render_node().ok().flatten();
    let egl = unsafe { EGLDisplay::new(device) }.context("Failed to create EGL display")?;
    let context = EGLContext::new(&egl).context("Failed to create EGL context")?;
    let mut renderer =
        unsafe { GlowRenderer::new(context) }.context("Failed to initialize renderer")?;
    init_shaders(renderer.borrow_mut()).context("Failed to initialize renderer")?;

    match render_node {
        Some(node) => {
            init_egl_client_side(dh, state, node, &mut renderer)?;
            info!(?node, "Headless backend rendering on hardware.");
        }
        None => info!("Headless backend rendering in software, dmabuf is unavailable."),
    }

    state.backend = BackendData::Headless(HeadlessState {
        renderer,
        surfaces: Vec::new(),
        loop_handle: event_loop.handle(),
        input_device_added: false,
    });

    let outputs = specs
        .into_iter()
        .map(|spec| state.backend.headless().add_output(spec))
        .collect::<Result<Vec<_>>>()?;
    state
        .common
        .output_configuration_state
        .add_heads(outputs.iter());
    for output in &outputs {
        state.common.add_output(output);
    }
    if let Err(err) = state.common.config.read_outputs(
        &mut state.common.output_configuration_state,
        &mut state.backend,
        &state.common.shell,
        &state.common.event_loop_handle,
        &mut state.common.workspace_state.update(),
        &state.common.xdg_activation_state,
        state.common.startup_done.clone(),
        &state.common.clock,
    ) {
        error!("Unrecoverable output configuration error: {}", err);
    }
    for output in &outputs {
        layer_map_for_output(output).arrange();
    }
    state.common.refresh();

    if let Ok(path) = std::env::var("COSMIC_HEADLESS_CONTROL") {
        control::listen(Path::new(&path), &event_loop.handle())
            .context("Failed to set up the headless control socket")?;
    }

    if state.common.with_xwayland {
        state.launch_xwayland(None);
    } else {
        state.notify_ready();
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeadlessDevice;

impl Device for HeadlessDevice {
    fn id(&self) -> String {
        "headless-input".to_string()
    }

    fn name(&self) -> String {
        "Headless test input".to_string()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(
            capability,
            DeviceCapability::Keyboard | DeviceCapability::Pointer
        )
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

#[derive(Debug)]
pub struct HeadlessInput;

impl InputBackend for HeadlessInput {
    type Device = HeadlessDevice;
    type KeyboardKeyEvent = HeadlessKeyEvent;
    type PointerAxisEvent = HeadlessAxisEvent;
    type PointerButtonEvent = HeadlessButtonEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = HeadlessMotionEvent;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = UnusedEvent;
}

#[derive(Debug)]
pub struct HeadlessKeyEvent {
    time: InputTime,
    key: u32,
    state: KeyState,
}

impl Event<HeadlessInput> for HeadlessKeyEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> HeadlessDevice {
        HeadlessDevice
    }
}

impl KeyboardKeyEvent<HeadlessInput> for HeadlessKeyEvent {
    fn key_code(&self) -> Keycode {
        // xkb keycodes are offset by 8 from evdev codes.
        Keycode::new(self.key + 8)
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        u32::from(self.state == KeyState::Pressed)
    }
}

#[derive(Debug)]
pub struct HeadlessButtonEvent {
    time: InputTime,
    button: u32,
    state: ButtonState,
}

impl Event<HeadlessInput> for HeadlessButtonEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> HeadlessDevice {
        HeadlessDevice
    }
}

impl PointerButtonEvent<HeadlessInput> for HeadlessButtonEvent {
    fn button_code(&self) -> u32 {
        self.button
    }

    fn state(&self) -> ButtonState {
        self.state
    }
}

#[derive(Debug)]
pub struct HeadlessAxisEvent {
    time: InputTime,
    horizontal: f64,
    vertical: f64,
}

impl Event<HeadlessInput> for HeadlessAxisEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> HeadlessDevice {
        HeadlessDevice
    }
}

impl PointerAxisEvent<HeadlessInput> for HeadlessAxisEvent {
    fn amount(&self, axis: Axis) -> Option<f64> {
        Some(match axis {
            Axis::Horizontal => self.horizontal,
            Axis::Vertical => self.vertical,
        })
    }

    fn amount_v120(&self, _axis: Axis) -> Option<f64> {
        None
    }

    fn source(&self) -> AxisSource {
        AxisSource::Continuous
    }

    fn relative_direction(&self, _axis: Axis) -> AxisRelativeDirection {
        AxisRelativeDirection::Identical
    }
}

/// Absolute motion relative to the seat's active output, as fractions of its size.
#[derive(Debug)]
pub struct HeadlessMotionEvent {
    time: InputTime,
    relative: (f64, f64),
}

impl Event<HeadlessInput> for HeadlessMotionEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> HeadlessDevice {
        HeadlessDevice
    }
}

impl AbsolutePositionEvent<HeadlessInput> for HeadlessMotionEvent {
    fn x(&self) -> f64 {
        self.relative.0
    }

    fn y(&self) -> f64 {
        self.relative.1
    }

    fn x_transformed(&self, width: i32) -> f64 {
        self.relative.0 * width as f64
    }

    fn y_transformed(&self, height: i32) -> f64 {
        self.relative.1 * height as f64
    }
}

impl PointerMotionAbsoluteEvent<HeadlessInput> for HeadlessMotionEvent {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_output_specs() {
        assert_eq!(
            parse_output_specs("1280x720, 1920x1080@1.5").unwrap(),
            vec![
                OutputSpec {
                    size: (1280, 720),
                    scale: 1.0
                },
                OutputSpec {
                    size: (1920, 1080),
                    scale: 1.5
                },
            ]
        );
        assert!(parse_output_specs("1280").is_err());
        assert!(parse_output_specs("0x720").is_err());
        assert!(parse_output_specs("1280x720@0").is_err());
    }
}
