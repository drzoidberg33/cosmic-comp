// SPDX-License-Identifier: GPL-3.0-only

//! Minimal Wayland client for compositor tests.
//!
//! Prints one JSON event per line on stdout and reads JSON commands, one per
//! line, from stdin. See the crate docs for the protocol.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Write},
    os::fd::{AsFd, OwnedFd},
    process,
};

use rustix::{
    event::{PollFd, PollFlags, poll},
    fs::{MemfdFlags, memfd_create},
};
use serde::Deserialize;
use serde_json::{Value, json};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::{ObjectId, WaylandError},
    delegate_noop,
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_output::{self, WlOutput},
        wl_pointer::{self, WlPointer},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::{self, WlSurface},
    },
};
use wayland_protocols::xdg::{
    decoration::zv1::client::{
        zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
        zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1},
    },
    shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::{self, XdgToplevel},
        xdg_wm_base::{self, XdgWmBase},
    },
};

fn emit(value: Value) {
    let mut out = io::stdout().lock();
    // The harness is gone if stdout is broken; nothing left to report to.
    if writeln!(out, "{value}").and_then(|()| out.flush()).is_err() {
        process::exit(1);
    }
}

fn fatal(message: impl std::fmt::Display) -> ! {
    emit(json!({"event": "error", "message": message.to_string()}));
    process::exit(1);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decorations {
    Server,
    Client,
}

struct Args {
    title: String,
    app_id: String,
    width: u32,
    height: u32,
    color: u32,
    decorations: Decorations,
}

fn parse_color(s: &str) -> Option<u32> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        title: "test-client".into(),
        app_id: "com.example.TestClient".into(),
        width: 400,
        height: 300,
        color: 0xff0000,
        decorations: Decorations::Server,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("missing value for {flag}"));
        match flag.as_str() {
            "--title" => args.title = value()?,
            "--app-id" => args.app_id = value()?,
            "--width" | "--height" => {
                let v = value()?;
                let n = v
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("invalid {flag}: {v}"))?;
                if flag == "--width" {
                    args.width = n;
                } else {
                    args.height = n;
                }
            }
            "--color" => {
                let v = value()?;
                args.color = parse_color(&v).ok_or_else(|| format!("invalid --color: {v}"))?;
            }
            "--decorations" => {
                args.decorations = match value()?.as_str() {
                    "server" => Decorations::Server,
                    "client" => Decorations::Client,
                    other => return Err(format!("invalid --decorations: {other}")),
                };
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Move,
    SetColor { color: String },
    Sync,
    Quit,
}

struct Window {
    surface: WlSurface,
    _xdg_surface: XdgSurface,
    toplevel: XdgToplevel,
    _decoration: Option<ZxdgToplevelDecorationV1>,
}

#[derive(Default)]
struct PendingConfigure {
    width: i32,
    height: i32,
    states: Vec<String>,
}

#[derive(Default)]
struct State {
    args: Option<Args>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    decoration_manager: Option<ZxdgDecorationManagerV1>,
    /// Registry global name -> output, for global_remove.
    outputs: HashMap<u32, WlOutput>,
    output_names: HashMap<ObjectId, String>,
    window: Option<Window>,
    pending: PendingConfigure,
    /// Size of the last committed buffer; `None` until the first configure.
    size: Option<(u32, u32)>,
    color: u32,
    /// Seat and serial of the last pointer button press, for `move`.
    last_button: Option<(WlSeat, u32)>,
    pointer_on_surface: bool,
}

impl State {
    fn args(&self) -> &Args {
        self.args.as_ref().unwrap()
    }

    fn is_our_surface(&self, surface: &WlSurface) -> bool {
        self.window.as_ref().is_some_and(|w| &w.surface == surface)
    }

    fn output_name(&self, output: &WlOutput) -> String {
        let id = output.id();
        self.output_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("unknown-{}", id.protocol_id()))
    }

    fn draw(&self, qh: &QueueHandle<Self>) {
        let (width, height) = self.size.unwrap();
        let window = self.window.as_ref().unwrap();
        let stride = width.checked_mul(4).filter(|s| *s <= i32::MAX as u32);
        let len = stride
            .and_then(|s| s.checked_mul(height))
            .filter(|l| *l <= i32::MAX as u32);
        let (Some(stride), Some(len)) = (stride, len) else {
            fatal(format!("buffer too large: {width}x{height}"));
        };

        let fd: OwnedFd = memfd_create("test-client", MemfdFlags::CLOEXEC)
            .unwrap_or_else(|err| fatal(format!("memfd_create: {err}")));
        let mut file = File::from(fd);
        // XRGB8888 is little-endian 0xXXRRGGBB; keep X at 0xff for opacity.
        let pixel = (0xff00_0000 | self.color).to_le_bytes();
        file.write_all(&pixel.repeat((width * height) as usize))
            .unwrap_or_else(|err| fatal(format!("writing shm pool: {err}")));

        let shm = self.shm.as_ref().unwrap();
        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Xrgb8888,
            qh,
            (),
        );
        pool.destroy();

        window.surface.attach(Some(&buffer), 0, 0);
        window.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        window.surface.commit();
    }

    fn handle_command(&mut self, line: &str, conn: &Connection, qh: &QueueHandle<Self>) {
        let cmd = match serde_json::from_str::<Command>(line) {
            Ok(cmd) => cmd,
            Err(err) => {
                emit(json!({"event": "command_error", "message": err.to_string()}));
                return;
            }
        };
        match cmd {
            Command::Move => {
                if let Some((seat, serial)) = &self.last_button
                    && let Some(window) = &self.window
                {
                    window.toplevel._move(seat, *serial);
                } else {
                    emit(json!({"event": "command_error", "message": "no button press for move"}));
                }
            }
            Command::SetColor { color } => {
                let Some(color) = parse_color(&color) else {
                    emit(
                        json!({"event": "command_error", "message": format!("invalid color: {color}")}),
                    );
                    return;
                };
                self.color = color;
                // Before the first configure, the color is used for the first buffer.
                if self.size.is_some() {
                    self.draw(qh);
                }
                emit(json!({"event": "redrawn"}));
            }
            Command::Sync => {
                conn.display().sync(qh, ());
            }
            Command::Quit => process::exit(0),
        }
    }
}

fn bind<I, U>(
    registry: &WlRegistry,
    qh: &QueueHandle<State>,
    name: u32,
    version: u32,
    max: u32,
) -> I
where
    I: Proxy + 'static,
    U: Default + Send + Sync + 'static,
    State: Dispatch<I, U>,
{
    registry.bind::<I, U, State>(name, version.min(max), qh, U::default())
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(bind::<_, ()>(registry, qh, name, version, 6));
                }
                "wl_shm" => state.shm = Some(bind::<_, ()>(registry, qh, name, version, 1)),
                "wl_seat" => {
                    let _: WlSeat = bind::<_, SeatDevices>(registry, qh, name, version, 8);
                }
                "xdg_wm_base" => {
                    state.wm_base = Some(bind::<_, ()>(registry, qh, name, version, 6));
                }
                "wl_output" => {
                    let output = bind::<_, ()>(registry, qh, name, version, 4);
                    state.outputs.insert(name, output);
                }
                "zxdg_decoration_manager_v1" => {
                    state.decoration_manager = Some(bind::<_, ()>(registry, qh, name, version, 1));
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(output) = state.outputs.remove(&name) {
                    state.output_names.remove(&output.id());
                    if output.version() >= 3 {
                        output.release();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.output_names.insert(output.id(), name);
        }
    }
}

impl Dispatch<WlSeat, SeatDevices> for State {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        devices: &SeatDevices,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            let mut devices = devices.0.lock().unwrap();
            if caps.contains(wl_seat::Capability::Pointer) && devices.pointer.is_none() {
                devices.pointer = Some(seat.get_pointer(qh, seat.clone()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && devices.keyboard.is_none() {
                devices.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

/// Per-seat input devices, stored as the seat's user data.
#[derive(Default)]
struct SeatDevices(std::sync::Mutex<SeatDevicesInner>);

#[derive(Default)]
struct SeatDevicesInner {
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
}

fn main() {
    let args = parse_args().unwrap_or_else(|err| fatal(err));

    let conn = Connection::connect_to_env()
        .unwrap_or_else(|err| fatal(format!("failed to connect to compositor: {err}")));
    let mut queue: EventQueue<State> = conn.new_event_queue();
    let qh = queue.handle();

    let mut state = State {
        color: args.color,
        args: Some(args),
        ..Default::default()
    };

    conn.display().get_registry(&qh, ());
    queue
        .roundtrip(&mut state)
        .unwrap_or_else(|err| fatal(format!("initial roundtrip: {err}")));
    // Second roundtrip so output names and seat capabilities arrive before mapping.
    queue
        .roundtrip(&mut state)
        .unwrap_or_else(|err| fatal(format!("initial roundtrip: {err}")));

    let Some(compositor) = &state.compositor else {
        fatal("wl_compositor not advertised");
    };
    let Some(wm_base) = &state.wm_base else {
        fatal("xdg_wm_base not advertised");
    };
    if state.shm.is_none() {
        fatal("wl_shm not advertised");
    }

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title(state.args().title.clone());
    toplevel.set_app_id(state.args().app_id.clone());
    let decoration = state.decoration_manager.as_ref().map(|manager| {
        let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
        decoration.set_mode(match state.args().decorations {
            Decorations::Server => zxdg_toplevel_decoration_v1::Mode::ServerSide,
            Decorations::Client => zxdg_toplevel_decoration_v1::Mode::ClientSide,
        });
        decoration
    });
    surface.commit();
    state.window = Some(Window {
        surface,
        _xdg_surface: xdg_surface,
        toplevel,
        _decoration: decoration,
    });

    run(&conn, &mut queue, &mut state);
}

fn run(conn: &Connection, queue: &mut EventQueue<State>, state: &mut State) -> ! {
    let qh = queue.handle();
    let stdin = io::stdin();
    let mut stdin_open = true;
    let mut stdin_buf: Vec<u8> = Vec::new();
    let mut read_buf = [0u8; 4096];

    loop {
        if let Err(err) = queue.dispatch_pending(state) {
            fatal(format!("wayland dispatch: {err}"));
        }
        let mut want_write = false;
        match queue.flush() {
            Ok(()) => {}
            Err(WaylandError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                want_write = true;
            }
            Err(err) => fatal(format!("wayland flush: {err}")),
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };

        let mut wl_flags = PollFlags::IN;
        if want_write {
            wl_flags |= PollFlags::OUT;
        }
        let stdin_fd = stdin.as_fd();
        let mut fds = vec![PollFd::from_borrowed_fd(guard.connection_fd(), wl_flags)];
        if stdin_open {
            fds.push(PollFd::from_borrowed_fd(stdin_fd, PollFlags::IN));
        }
        match poll(&mut fds, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(err) => fatal(format!("poll: {err}")),
        }
        let wl_revents = fds[0].revents();
        let stdin_ready = stdin_open && !fds[1].revents().is_empty();
        drop(fds);

        if wl_revents.intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP) {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) => fatal(format!("wayland read: {err}")),
            }
        } else {
            drop(guard);
        }

        if stdin_ready {
            match rustix::io::read(stdin_fd, &mut read_buf) {
                // EOF: tests may close stdin; keep running.
                Ok(0) => stdin_open = false,
                Ok(n) => {
                    stdin_buf.extend_from_slice(&read_buf[..n]);
                    while let Some(pos) = stdin_buf.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = stdin_buf.drain(..=pos).collect();
                        let line = String::from_utf8_lossy(&line);
                        let line = line.trim();
                        if !line.is_empty() {
                            state.handle_command(line, conn, &qh);
                        }
                    }
                }
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
                Err(_) => stdin_open = false,
            }
        }
    }
}

fn decode_states(raw: &[u8]) -> Vec<String> {
    raw.chunks_exact(4)
        .map(|c| {
            let v = u32::from_ne_bytes(c.try_into().unwrap());
            match xdg_toplevel::State::try_from(v) {
                Ok(xdg_toplevel::State::Maximized) => "maximized".into(),
                Ok(xdg_toplevel::State::Fullscreen) => "fullscreen".into(),
                Ok(xdg_toplevel::State::Resizing) => "resizing".into(),
                Ok(xdg_toplevel::State::Activated) => "activated".into(),
                Ok(xdg_toplevel::State::TiledLeft) => "tiled_left".into(),
                Ok(xdg_toplevel::State::TiledRight) => "tiled_right".into(),
                Ok(xdg_toplevel::State::TiledTop) => "tiled_top".into(),
                Ok(xdg_toplevel::State::TiledBottom) => "tiled_bottom".into(),
                Ok(xdg_toplevel::State::Suspended) => "suspended".into(),
                _ => v.to_string(),
            }
        })
        .collect()
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                state.pending = PendingConfigure {
                    width,
                    height,
                    states: decode_states(&states),
                };
            }
            xdg_toplevel::Event::Close => {
                emit(json!({"event": "closed"}));
                process::exit(0);
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let xdg_surface::Event::Configure { serial } = event else {
            return;
        };
        let pending = std::mem::take(&mut state.pending);
        emit(json!({
            "event": "configure",
            "width": pending.width,
            "height": pending.height,
            "states": pending.states,
        }));
        let args = state.args();
        let width = u32::try_from(pending.width)
            .ok()
            .filter(|w| *w > 0)
            .unwrap_or(args.width);
        let height = u32::try_from(pending.height)
            .ok()
            .filter(|h| *h > 0)
            .unwrap_or(args.height);
        let first = state.size.is_none();
        state.size = Some((width, height));
        xdg_surface.ack_configure(serial);
        state.draw(qh);
        if first {
            emit(json!({"event": "ready", "width": width, "height": height}));
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event {
            let mode = match mode {
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ServerSide) => "server".into(),
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ClientSide) => "client".into(),
                WEnum::Value(other) => u32::from(other).to_string(),
                WEnum::Unknown(v) => v.to_string(),
            };
            emit(json!({"event": "decoration", "mode": mode}));
        }
    }
}

impl Dispatch<WlSurface, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_surface::Event::Enter { output } => {
                emit(json!({"event": "enter", "output": state.output_name(&output)}));
            }
            wl_surface::Event::Leave { output } => {
                emit(json!({"event": "leave", "output": state.output_name(&output)}));
            }
            wl_surface::Event::PreferredBufferScale { factor } => {
                emit(json!({"event": "preferred_buffer_scale", "scale": factor}));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Every draw uses a fresh buffer, so a released buffer is never reused.
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

impl Dispatch<WlPointer, WlSeat> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        seat: &WlSeat,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_on_surface = state.is_our_surface(&surface);
                if state.pointer_on_surface {
                    emit(json!({"event": "pointer_enter", "x": surface_x, "y": surface_y}));
                }
            }
            wl_pointer::Event::Leave { surface, .. } => {
                if state.is_our_surface(&surface) {
                    state.pointer_on_surface = false;
                    emit(json!({"event": "pointer_leave"}));
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } if state.pointer_on_surface => {
                emit(json!({"event": "pointer_motion", "x": surface_x, "y": surface_y}));
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: button_state,
                ..
            } if state.pointer_on_surface => {
                let pressed = button_state == WEnum::Value(wl_pointer::ButtonState::Pressed);
                if pressed {
                    state.last_button = Some((seat.clone(), serial));
                }
                emit(json!({"event": "pointer_button", "button": button, "pressed": pressed}));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } if state.is_our_surface(&surface) => {
                emit(json!({"event": "keyboard_enter"}));
            }
            wl_keyboard::Event::Leave { surface, .. } if state.is_our_surface(&surface) => {
                emit(json!({"event": "keyboard_leave"}));
            }
            wl_keyboard::Event::Key {
                key,
                state: key_state,
                ..
            } => {
                let pressed = key_state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                emit(json!({"event": "key", "key": key, "pressed": pressed}));
            }
            // Keymap fd is closed when the event is dropped.
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            emit(json!({"event": "synced"}));
        }
    }
}

delegate_noop!(State: WlCompositor);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ZxdgDecorationManagerV1);
