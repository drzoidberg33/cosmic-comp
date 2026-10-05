// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    backend::{
        output_spec::{self, OutputSpec, parse_output_specs},
        render,
    },
    config::ScreenFilter,
    shell::{Devices, SeatExt},
    state::{BackendData, Common},
    utils::prelude::*,
    wayland::protocols::drm::WlDrmState,
};
use anyhow::{Context, Result, anyhow};
use cosmic_comp_config::output::comp::OutputConfig;
use smithay::{
    backend::{
        allocator::{
            dmabuf::{Dmabuf, DmabufAllocator},
            gbm::{GbmAllocator, GbmBufferFlags},
            vulkan::{ImageUsageFlags, VulkanAllocator},
        },
        drm::{DrmDeviceFd, DrmNode, NodeType},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::{AbsolutePositionEvent, Event, InputEvent},
        renderer::{
            Bind, ImportDma,
            damage::{OutputDamageTracker, RenderOutputResult},
            glow::GlowRenderer,
        },
        vulkan::{Instance, PhysicalDevice, version::Version},
        x11::{Window, WindowBuilder, X11Backend, X11Event, X11Handle, X11Input, X11Surface},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{EventLoop, LoopHandle, ping},
        gbm::Device as GbmDevice,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::DisplayHandle,
    },
    utils::{DeviceFd, Logical, Point, Rectangle, Size, Transform},
    wayland::{dmabuf::DmabufFeedbackBuilder, presentation::Refresh},
};
use std::{borrow::BorrowMut, cell::RefCell, os::unix::io::OwnedFd, time::Duration};
use tracing::{debug, error, info, warn};

use super::render::{ScreenFilterStorage, init_shaders};

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum Allocator {
    Gbm(GbmAllocator<DrmDeviceFd>),
    Vulkan(PhysicalDevice),
}

#[derive(Debug)]
pub struct X11State {
    allocator: Allocator,
    _egl: EGLDisplay,
    pub renderer: GlowRenderer,
    surfaces: Vec<Surface>,
    handle: X11Handle,
}

impl X11State {
    pub fn add_window(
        &mut self,
        handle: LoopHandle<'_, State>,
        spec: Option<OutputSpec>,
    ) -> Result<Output> {
        let title = format!("COSMIC (X11-{})", self.surfaces.len());
        let mut builder = WindowBuilder::new().title(&title);
        if let Some(spec) = spec {
            let size = (
                u16::try_from(spec.size.0).context("Window too wide")?,
                u16::try_from(spec.size.1).context("Window too high")?,
            );
            builder = builder.size(Size::<u16, Logical>::from(size));
        }
        let scale = spec.map_or(1.0, |spec| spec.scale);
        let window = builder
            .build(&self.handle)
            .with_context(|| "Failed to create window")?;
        let fourcc = window.format();
        let modifiers = Bind::<Dmabuf>::supported_formats(&self.renderer).unwrap();
        let filtered_modifiers = modifiers
            .iter()
            .filter(|format| format.code == fourcc)
            .map(|format| format.modifier);
        let surface = match &self.allocator {
            Allocator::Gbm(gbm) => self
                .handle
                .create_surface(&window, DmabufAllocator(gbm.clone()), filtered_modifiers)
                .with_context(|| "Failed to create surface")?,
            Allocator::Vulkan(vulkan) => self
                .handle
                .create_surface(
                    &window,
                    DmabufAllocator(
                        VulkanAllocator::new(vulkan, ImageUsageFlags::COLOR_ATTACHMENT)
                            .with_context(|| "Failed to create vulkan allocator for window")?,
                    ),
                    filtered_modifiers,
                )
                .with_context(|| "Failed to create surface")?,
        };

        let name = format!("X11-{}", self.surfaces.len());
        let size = window.size();
        let props = PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "COSMIC".to_string(),
            model: name.clone(),
            serial_number: "Unknown".to_string(),
        };
        let mode = Mode {
            size: (size.w as i32, size.h as i32).into(),
            refresh: 60_000,
        };
        let output = Output::new(name, props);
        output.add_mode(mode);
        output.set_preferred(mode);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(scale)),
            Some((0, 0).into()),
        );
        output.user_data().insert_if_missing(|| {
            RefCell::new(OutputConfig {
                mode: ((size.w as i32, size.h as i32), None),
                scale,
                ..Default::default()
            })
        });

        let output_ref = output.clone();
        let (ping, source) =
            ping::make_ping().with_context(|| "Failed to create output event loop source")?;
        let _token = handle
            .insert_source(source, move |_, _, state| {
                let x11_state = state.backend.x11();
                if let Some(surface) = x11_state
                    .surfaces
                    .iter_mut()
                    .find(|s| s.output == output_ref)
                {
                    if let Err(err) =
                        surface.render_output(&mut x11_state.renderer, &mut state.common)
                    {
                        error!(?err, "Error rendering.");
                    }
                    surface.dirty = false;
                    surface.pending = true;
                }
            })
            .with_context(|| "Failed to add output to event loop")?;

        self.surfaces.push(Surface {
            window,
            surface,
            damage_tracker: OutputDamageTracker::from_output(&output),
            output: output.clone(),
            render: ping.clone(),
            dirty: false,
            pending: true,
            screen_filter_state: ScreenFilterStorage::default(),
        });

        // schedule first render
        ping.ping();
        Ok(output)
    }

    pub fn schedule_render(&mut self, output: &Output) {
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.output == *output) {
            surface.dirty = true;
            if !surface.pending {
                surface.render.ping();
            }
        }
    }

    pub fn all_outputs(&self) -> Vec<Output> {
        self.surfaces.iter().map(|s| s.output.clone()).collect()
    }

    pub fn apply_config_for_outputs(&mut self, test_only: bool) -> Result<(), anyhow::Error> {
        // TODO: don't ignore OutputState
        let mut result = Ok(());
        for surface in &self.surfaces {
            let size = surface.window.size();
            let mut config = surface
                .output
                .user_data()
                .get::<RefCell<OutputConfig>>()
                .unwrap()
                .borrow_mut();

            // reset size
            if config.mode.0 != (size.w as i32, size.h as i32) {
                if !test_only {
                    config.mode = ((size.w as i32, size.h as i32), None);
                }
                result = Err(anyhow::anyhow!("Cannot set window size"));
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
}

#[derive(Debug)]
pub struct Surface {
    window: Window,
    damage_tracker: OutputDamageTracker,
    surface: X11Surface,
    output: Output,
    render: ping::Ping,
    dirty: bool,
    pending: bool,
    screen_filter_state: ScreenFilterStorage,
}

impl Surface {
    pub fn render_output(&mut self, renderer: &mut GlowRenderer, state: &mut Common) -> Result<()> {
        let (mut buffer, age) = self
            .surface
            .buffer()
            .with_context(|| "Failed to allocate buffer")?;
        let mut fb = renderer
            .bind(&mut buffer)
            .with_context(|| "Failed to bind dmabuf")?;
        match render::render_output(
            None,
            renderer,
            &mut fb,
            &mut self.damage_tracker,
            age as usize,
            &state.shell,
            state.clock.now(),
            &self.output,
            render::CursorMode::NotDefault,
            &mut self.screen_filter_state,
            &state.event_loop_handle,
        ) {
            Ok(RenderOutputResult { damage, states, .. }) => {
                self.surface
                    .submit()
                    .with_context(|| "Failed to submit buffer for display")?;
                state.send_frames(&self.output, None);
                state.update_primary_output(&self.output, &states);
                state.send_dmabuf_feedback(&self.output, &states, |_| None);
                if damage.is_some() {
                    let mut output_presentation_feedback = state
                        .shell
                        .read()
                        .take_presentation_feedback(&self.output, &states);
                    output_presentation_feedback.presented(
                        state.clock.now(),
                        self.output
                            .current_mode()
                            .map(|mode| {
                                Refresh::Fixed(Duration::from_secs_f64(
                                    1_000.0 / mode.refresh as f64,
                                ))
                            })
                            .unwrap_or(Refresh::Unknown),
                        0,
                        wp_presentation_feedback::Kind::Vsync,
                    )
                }
            }
            Err(err) => {
                self.surface.reset_buffers();
                anyhow::bail!("Rendering failed: {}", err);
            }
        };

        Ok(())
    }
}

fn try_vulkan_allocator(node: &DrmNode) -> Option<Allocator> {
    let instance = match Instance::new(Version::VERSION_1_2, None) {
        Ok(instance) => instance,
        Err(err) => {
            warn!(
                ?err,
                "Failed to instanciate vulkan, falling back to gbm allocator.",
            );
            return None;
        }
    };

    let devices = match PhysicalDevice::enumerate(&instance) {
        Ok(devices) => devices,
        Err(err) => {
            debug!(?err, "No vulkan devices, falling back to gbm.");
            return None;
        }
    };

    let Some(device) = devices
        .filter(|phd| {
            phd.has_device_extension(smithay::reexports::ash::ext::physical_device_drm::NAME)
                && VulkanAllocator::required_extensions(phd)
                    .into_iter()
                    .all(|ext| phd.has_device_extension(ext))
        })
        .find(|phd| {
            phd.primary_node().unwrap() == Some(*node) || phd.render_node().unwrap() == Some(*node)
        })
    else {
        debug!(?node, "No vulkan device for node, falling back to gbm.");
        return None;
    };

    Some(Allocator::Vulkan(device))
}

fn try_gbm_allocator(fd: OwnedFd) -> Option<Allocator> {
    // Create the gbm device for buffer allocation.
    let device = match GbmDevice::new(DrmDeviceFd::new(DeviceFd::from(fd))) {
        Ok(gbm) => gbm,
        Err(err) => {
            error!(?err, "Failed to create GBM device.");
            return None;
        }
    };

    Some(Allocator::Gbm(GbmAllocator::new(
        device,
        GbmBufferFlags::RENDERING,
    )))
}

/// Parses `COSMIC_X11_OUTPUTS` to open one window (and thus one output) per entry, for testing
/// multi-output behaviour nested. Accepts a count (`2`) or `WxH[@scale][+X+Y]` entries (see
/// [`output_spec`]), e.g. `960x540+0+540,960x540+960+540,960x540+480+0`. Defaults to a single
/// window of the default size.
fn output_specs_from_env() -> Result<Vec<Option<OutputSpec>>> {
    let Ok(value) = std::env::var("COSMIC_X11_OUTPUTS") else {
        return Ok(vec![None]);
    };

    if let Ok(count) = value.trim().parse::<usize>() {
        return Ok(vec![None; count.max(1)]);
    }

    Ok(parse_output_specs(&value)
        .context("Invalid COSMIC_X11_OUTPUTS")?
        .into_iter()
        .map(Some)
        .collect())
}

pub fn init_backend(
    dh: &DisplayHandle,
    event_loop: &mut EventLoop<State>,
    state: &mut State,
) -> Result<()> {
    let backend = X11Backend::new().with_context(|| "Failed to initilize X11 backend")?;
    let handle = backend.handle();

    // Obtain the DRM node the X server uses for direct rendering.
    let (drm_node, fd) = handle
        .drm_node()
        .with_context(|| "Could not get DRM node used by X server")?;

    let device = EGLDevice::enumerate()
        .with_context(|| "Failed to enumerate EGL devices")?
        .find(|device| device.try_get_render_node().ok().flatten() == Some(drm_node))
        .with_context(|| format!("Failed to find EGLDevice for node {}", drm_node))?;
    // Initialize EGL
    let egl = unsafe { EGLDisplay::new(device) }.with_context(|| "Failed to create EGL display")?;
    // Create the OpenGL context
    let context = EGLContext::new(&egl).with_context(|| "Failed to create EGL context")?;
    // Create a renderer
    let mut renderer =
        unsafe { GlowRenderer::new(context) }.with_context(|| "Failed to initialize renderer")?;

    init_shaders(renderer.borrow_mut()).context("Failed to initialize renderer")?;
    init_egl_client_side(dh, state, drm_node, &mut renderer)?;

    state.backend = BackendData::X11(X11State {
        handle,
        allocator: try_vulkan_allocator(&drm_node)
            .or_else(|| try_gbm_allocator(fd))
            .context("Failed to create allocator for x11")?,
        _egl: egl,
        renderer,
        surfaces: Vec::new(),
    });

    let specs = output_specs_from_env()?;
    let outputs = specs
        .iter()
        .map(|spec| {
            state
                .backend
                .x11()
                .add_window(event_loop.handle(), *spec)
                .with_context(|| "Failed to create wl_output")
        })
        .collect::<Result<Vec<_>>>()?;
    state
        .common
        .output_configuration_state
        .add_heads(outputs.iter());
    {
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
        let positions = outputs
            .iter()
            .cloned()
            .zip(specs.iter().map(|spec| spec.and_then(|spec| spec.position)))
            .collect::<Vec<_>>();
        output_spec::apply_positions(state, &positions)?;
        state.common.refresh();
    }

    if state.common.with_xwayland {
        state.launch_xwayland(None);
    } else {
        state.notify_ready();
    }

    event_loop
        .handle()
        .insert_source(backend, move |event, _, state| match event {
            X11Event::CloseRequested { window_id } => {
                // TODO: drain_filter
                let mut outputs_removed = Vec::new();
                for surface in state
                    .backend
                    .x11()
                    .surfaces
                    .iter()
                    .filter(|s| s.window.id() == window_id)
                {
                    surface.window.unmap();
                    outputs_removed.push(surface.output.clone());
                }
                state
                    .backend
                    .x11()
                    .surfaces
                    .retain(|s| s.window.id() != window_id);
                for output in outputs_removed.into_iter() {
                    state.common.remove_output(&output);
                }
            }
            X11Event::Resized {
                new_size,
                window_id,
            } => {
                let size = { (new_size.w as i32, new_size.h as i32).into() };
                let mode = Mode {
                    size,
                    refresh: 60_000,
                };
                if let Some(surface) = state
                    .backend
                    .x11()
                    .surfaces
                    .iter_mut()
                    .find(|s| s.window.id() == window_id)
                {
                    let output = &surface.output;
                    {
                        let mut config = output
                            .user_data()
                            .get::<RefCell<OutputConfig>>()
                            .unwrap()
                            .borrow_mut();
                        config.mode.0 = size.into();
                    }

                    output.delete_mode(output.current_mode().unwrap());
                    output.change_current_state(Some(mode), None, None, None);
                    output.set_preferred(mode);
                    layer_map_for_output(output).arrange();
                    state.common.output_configuration_state.update();
                    surface.dirty = true;
                    if !surface.pending {
                        surface.render.ping();
                    }
                }
            }
            X11Event::Refresh { window_id } | X11Event::PresentCompleted { window_id } => {
                if let Some(surface) = state
                    .backend
                    .x11()
                    .surfaces
                    .iter_mut()
                    .find(|s| s.window.id() == window_id)
                {
                    if surface.dirty {
                        surface.render.ping();
                    } else {
                        surface.pending = false;
                    }
                }
            }
            X11Event::Input {
                event,
                window_id: _,
            } => state.process_x11_event(event),
            X11Event::Focus { .. } => {} // TODO: release all keys when losing focus and make sure to go through our keyboard filter code
        })
        .map_err(|_| anyhow::anyhow!("Failed to insert X11 Backend into event loop"))?;

    Ok(())
}

fn init_egl_client_side<R>(
    dh: &DisplayHandle,
    state: &mut State,
    render_node: DrmNode,
    renderer: &mut R,
) -> Result<()>
where
    R: ImportDma,
{
    let default_feedback =
        DmabufFeedbackBuilder::new(render_node.dev_id(), renderer.dmabuf_formats())
            .build()
            .unwrap();
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

    info!("EGL hardware-acceleration enabled.");

    Ok(())
}

impl State {
    pub fn process_x11_event(&mut self, event: InputEvent<X11Input>) {
        // here we can handle special cases for x11 inputs, like mapping them to windows
        if let InputEvent::PointerMotionAbsolute { event } = &event
            && let Some(window) = event.window()
        {
            let (output, window_size) = self
                .backend
                .x11()
                .surfaces
                .iter()
                .find(|surface| &surface.window == window.as_ref())
                .map(|surface| (surface.output.clone(), surface.window.size()))
                .unwrap();

            let device = event.device();
            let seat = self
                .common
                .shell
                .read()
                .seats
                .iter()
                .find(|seat| {
                    let devices = seat.user_data().get::<Devices>().unwrap();
                    devices.has_device(&device, &crate::input::InputBackendId::Normal)
                })
                .cloned();
            if let Some(seat) = seat {
                seat.set_active_output(&output);

                // While a button is held, X11 keeps reporting motion relative to the window the
                // press happened in, also past its edges. smithay clamps negative coordinates to
                // 0 (but not ones past the far edges), which pinned drags at the top and left
                // edges. Map the raw coordinates to a global position and use the output there.
                let outputs = self
                    .common
                    .shell
                    .read()
                    .outputs()
                    .cloned()
                    .collect::<Vec<_>>();
                let geometries = outputs
                    .iter()
                    .map(|o| o.geometry().to_f64())
                    .collect::<Vec<_>>();
                let (target, position) = nested_pointer_target(
                    output.geometry().to_f64(),
                    window_size,
                    (event.x(), event.y()),
                    &geometries,
                );
                let inside_window = (0.0..f64::from(window_size.w)).contains(&event.x())
                    && (0.0..f64::from(window_size.h)).contains(&event.y());
                if !inside_window {
                    let target = target.map_or_else(|| output.clone(), |i| outputs[i].clone());
                    self.common.idle_notifier_state.notify_activity(&seat);
                    render::cursor::notify_cursor_activity(self, &seat);
                    self.pointer_motion_absolute(&seat, target, position, event.time());
                    for output in self.common.shell.read().outputs() {
                        self.backend.x11().schedule_render(output);
                    }
                    return;
                }
            }
        };

        self.process_input_event(event, crate::input::InputBackendId::Normal);
        // TODO actually figure out the output
        for output in self.common.shell.read().outputs() {
            self.backend.x11().schedule_render(output);
        }
    }
}

/// Maps pointer coordinates relative to the X11 window showing an output at `source` (global
/// logical geometry) to a global position, and returns the index of the output in `outputs`
/// containing it. The coordinates may lie outside the window during an implicit grab. If no
/// output contains the position, it's clamped to `source` and no index is returned.
fn nested_pointer_target(
    source: Rectangle<f64, Global>,
    window_size: Size<u16, Logical>,
    (x, y): (f64, f64),
    outputs: &[Rectangle<f64, Global>],
) -> (Option<usize>, Point<f64, Global>) {
    let position = source.loc
        + Point::<f64, Global>::from((
            x * source.size.w / f64::from(window_size.w),
            y * source.size.h / f64::from(window_size.h),
        ));
    match outputs.iter().position(|o| o.contains(position)) {
        Some(index) => (Some(index), position),
        None => (
            None,
            Point::from((
                position
                    .x
                    .clamp(source.loc.x, source.loc.x + source.size.w - 1.0),
                position
                    .y
                    .clamp(source.loc.y, source.loc.y + source.size.h - 1.0),
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rectangle<f64, Global> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    /// Two 960x540 outputs at the bottom, one centred above them.
    fn pyramid() -> Vec<Rectangle<f64, Global>> {
        vec![
            rect(0.0, 540.0, 960.0, 540.0),
            rect(960.0, 540.0, 960.0, 540.0),
            rect(480.0, 0.0, 960.0, 540.0),
        ]
    }

    #[test]
    fn maps_coordinates_past_the_window_to_neighbouring_outputs() {
        let outputs = pyramid();
        let size = Size::from((960, 540));
        // Inside the window.
        assert_eq!(
            nested_pointer_target(outputs[0], size, (100.0, 100.0), &outputs),
            (Some(0), (100.0, 640.0).into())
        );
        // Above the bottom-left window, onto the top output (used to be clamped to y = 0).
        assert_eq!(
            nested_pointer_target(outputs[0], size, (600.0, -40.0), &outputs),
            (Some(2), (600.0, 500.0).into())
        );
        // Left of the bottom-right window, onto the bottom-left output.
        assert_eq!(
            nested_pointer_target(outputs[1], size, (-60.0, 10.0), &outputs),
            (Some(0), (900.0, 550.0).into())
        );
        // Right of the bottom-left window, onto the bottom-right output.
        assert_eq!(
            nested_pointer_target(outputs[0], size, (1000.0, 10.0), &outputs),
            (Some(1), (1000.0, 550.0).into())
        );
    }

    #[test]
    fn clamps_to_the_window_where_there_is_no_output() {
        let outputs = pyramid();
        let size = Size::from((960, 540));
        // Above the left part of the bottom-left output there is no output.
        assert_eq!(
            nested_pointer_target(outputs[0], size, (100.0, -40.0), &outputs),
            (None, (100.0, 540.0).into())
        );
        assert_eq!(
            nested_pointer_target(outputs[0], size, (-50.0, 2000.0), &outputs),
            (None, (0.0, 1079.0).into())
        );
    }

    #[test]
    fn scales_window_pixels_to_logical_coordinates() {
        // A 2x output: the 960x540 window shows 480x270 logical pixels.
        let outputs = vec![rect(0.0, 270.0, 480.0, 270.0), rect(0.0, 0.0, 480.0, 270.0)];
        assert_eq!(
            nested_pointer_target(
                outputs[0],
                Size::from((960, 540)),
                (200.0, -100.0),
                &outputs
            ),
            (Some(1), (100.0, 220.0).into())
        );
    }
}
