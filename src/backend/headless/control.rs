// SPDX-License-Identifier: GPL-3.0-only

//! Control socket of the headless backend.
//!
//! Newline delimited JSON: every request is one object with a `cmd` field, answered by one line,
//! either `{"ok":true,...}` or `{"ok":false,"error":"..."}`. Requests on a connection are
//! handled in order. Positions are global logical coordinates.
//!
//! - `{"cmd":"outputs"}` → `outputs: [{name, x, y, width, height, scale}]`
//! - `{"cmd":"windows"}` → `windows: [{title, app_id, x, y, width, height, output, workspace,
//!   active_workspace, floating, maximized}]`
//! - `{"cmd":"pointer"}` → `x, y` of the pointer
//! - `{"cmd":"pointer_motion","x":F,"y":F}`
//! - `{"cmd":"pointer_button","button":"left"|"right"|"middle"|CODE,"pressed":B}`
//! - `{"cmd":"pointer_axis","horizontal":F,"vertical":F}`
//! - `{"cmd":"key","key":EVDEV_CODE,"pressed":B}`
//! - `{"cmd":"screenshot","output":"HEADLESS-0","path":"/abs/file.png"}` renders the output
//!   synchronously and writes it as an RGBA PNG.
//! - `{"cmd":"sync"}` → answered once all earlier requests have been handled.

use super::HeadlessState;
use crate::{state::BackendData, utils::prelude::*};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use smithay::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction,
    generic::{Generic, NoIoDrop},
};
use std::{
    io::{ErrorKind, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
};
use tracing::warn;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Outputs,
    Windows,
    Pointer,
    PointerMotion {
        x: f64,
        y: f64,
    },
    PointerButton {
        button: Button,
        pressed: bool,
    },
    PointerAxis {
        #[serde(default)]
        horizontal: f64,
        #[serde(default)]
        vertical: f64,
    },
    Key {
        key: u32,
        pressed: bool,
    },
    Screenshot {
        output: String,
        path: PathBuf,
    },
    Sync,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Button {
    Code(u32),
    Name(String),
}

impl Button {
    fn code(&self) -> Result<u32> {
        match self {
            Button::Code(code) => Ok(*code),
            Button::Name(name) => match name.as_str() {
                "left" => Ok(BTN_LEFT),
                "right" => Ok(BTN_RIGHT),
                "middle" => Ok(BTN_MIDDLE),
                _ => Err(anyhow!("Unknown button `{name}`")),
            },
        }
    }
}

pub fn listen(path: &Path, handle: &LoopHandle<'static, State>) -> Result<()> {
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).with_context(|| format!("Failed to bind {}", path.display()))?;
    listener.set_nonblocking(true)?;

    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            |_, listener, state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(err) = add_client(stream, &state.common.event_loop_handle) {
                                warn!(?err, "Failed to accept control client.");
                            }
                        }
                        Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                        Err(err) => {
                            warn!(?err, "Failed to accept control client.");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| anyhow!("Failed to insert control socket: {err}"))?;
    Ok(())
}

fn add_client(stream: UnixStream, handle: &LoopHandle<'static, State>) -> Result<()> {
    stream.set_nonblocking(true)?;
    let mut pending = Vec::new();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, state| Ok(handle_readable(stream, &mut pending, state)),
        )
        .map_err(|err| anyhow!("Failed to insert control client: {err}"))?;
    Ok(())
}

fn handle_readable(
    stream: &mut NoIoDrop<UnixStream>,
    pending: &mut Vec<u8>,
    state: &mut State,
) -> PostAction {
    let mut closed = false;
    let mut chunk = [0u8; 4096];
    loop {
        match (&**stream).read(&mut chunk) {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                warn!(?err, "Failed to read from control client.");
                return PostAction::Remove;
            }
        }
    }

    while let Some(end) = pending.iter().position(|b| *b == b'\n') {
        let line = pending.drain(..=end).collect::<Vec<_>>();
        let line = String::from_utf8_lossy(&line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Command>(line)
            .context("Invalid command")
            .and_then(|command| execute(command, state))
        {
            Ok(mut value) => {
                value["ok"] = json!(true);
                value
            }
            Err(err) => json!({ "ok": false, "error": format!("{err:#}") }),
        };

        let mut bytes = response.to_string().into_bytes();
        bytes.push(b'\n');
        if let Err(err) = write_all(stream, &bytes) {
            warn!(?err, "Failed to write to control client.");
            return PostAction::Remove;
        }
    }

    if closed {
        PostAction::Remove
    } else {
        PostAction::Continue
    }
}

fn write_all(stream: &NoIoDrop<UnixStream>, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match (&**stream).write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(err) if err.kind() == ErrorKind::WouldBlock => std::thread::yield_now(),
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn execute(command: Command, state: &mut State) -> Result<Value> {
    match command {
        Command::Outputs => {
            let shell = state.common.shell.read();
            let outputs = shell
                .outputs()
                .map(|output| {
                    let geometry = output.geometry();
                    json!({
                        "name": output.name(),
                        "x": geometry.loc.x,
                        "y": geometry.loc.y,
                        "width": geometry.size.w,
                        "height": geometry.size.h,
                        "scale": output.current_scale().fractional_scale(),
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({ "outputs": outputs }))
        }
        Command::Windows => {
            let shell = state.common.shell.read();
            let mut windows = Vec::new();
            for set in shell.workspaces.sets.values() {
                for (idx, workspace) in set.workspaces.iter().enumerate() {
                    for mapped in workspace.mapped() {
                        let Some(geometry) = workspace.element_geometry(mapped) else {
                            continue;
                        };
                        let geometry = geometry.to_global(&workspace.output);
                        let window = mapped.active_window();
                        windows.push(json!({
                            "title": window.title(),
                            "app_id": window.app_id(),
                            "x": geometry.loc.x,
                            "y": geometry.loc.y,
                            "width": geometry.size.w,
                            "height": geometry.size.h,
                            "output": workspace.output.name(),
                            "workspace": idx,
                            "active_workspace": idx == set.active,
                            "floating": workspace.is_floating(&window),
                            "maximized": mapped.is_maximized(false),
                        }));
                    }
                }
            }
            Ok(json!({ "windows": windows }))
        }
        Command::Pointer => {
            let shell = state.common.shell.read();
            let location = shell
                .seats
                .last_active()
                .get_pointer()
                .context("Seat has no pointer")?
                .current_location();
            Ok(json!({ "x": location.x, "y": location.y }))
        }
        Command::PointerMotion { x, y } => {
            HeadlessState::pointer_motion(state, (x, y).into())?;
            Ok(json!({}))
        }
        Command::PointerButton { button, pressed } => {
            HeadlessState::pointer_button(state, button.code()?, pressed);
            Ok(json!({}))
        }
        Command::PointerAxis {
            horizontal,
            vertical,
        } => {
            HeadlessState::pointer_axis(state, horizontal, vertical);
            Ok(json!({}))
        }
        Command::Key { key, pressed } => {
            HeadlessState::key(state, key, pressed);
            Ok(json!({}))
        }
        Command::Screenshot { output, path } => {
            let output = state
                .common
                .shell
                .read()
                .outputs()
                .find(|o| o.name() == output)
                .cloned()
                .with_context(|| format!("Unknown output `{output}`"))?;
            let BackendData::Headless(headless) = &mut state.backend else {
                unreachable!("control socket only exists on the headless backend");
            };
            headless.render(&output, &mut state.common, Some(&path))?;
            Ok(json!({ "path": path }))
        }
        Command::Sync => Ok(json!({})),
    }
}
