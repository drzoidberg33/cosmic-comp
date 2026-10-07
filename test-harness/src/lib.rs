// SPDX-License-Identifier: GPL-3.0-only

//! Test harness for cosmic-comp.
//!
//! [`Compositor`] starts cosmic-comp on the fork-only headless backend in isolated
//! runtime/config/state directories and drives it over the backend's control socket.
//! [`Client`] runs the `test-client` binary (a solid-colour Wayland window that reports its
//! events as JSON lines) against it. Nothing is shown on screen and the host session is not
//! touched: the session D-Bus is disabled and Xwayland is not started.
//!
//! The compositor binary defaults to `../target/dev-opt/cosmic-comp` and can be overridden with
//! `COSMIC_COMP_BIN`. Set `COSMIC_TEST_KEEP=1` to keep the run directory (logs, screenshots) of
//! passing tests; it is always kept when a test fails.

use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, RecvTimeoutError, channel},
    },
    thread,
    time::{Duration, Instant},
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Height of the server-side title bar COSMIC draws for windows without client decorations.
pub const HEADER_HEIGHT: i32 = 36;

pub type Result<T, E = String> = std::result::Result<T, E>;

fn compositor_binary() -> PathBuf {
    std::env::var_os("COSMIC_COMP_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/dev-opt/cosmic-comp")
        })
}

fn client_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("COSMIC_TEST_CLIENT_BIN") {
        return path.into();
    }
    // Next to the running binary: `target/<profile>/` for bins, `target/<profile>/deps/` for
    // integration tests.
    let exe = std::env::current_exe().unwrap_or_default();
    exe.ancestors()
        .skip(1)
        .take(2)
        .map(|dir| dir.join("test-client"))
        .find(|path| path.exists())
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/test-client"))
}

fn run_dir() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    // Sockets must fit in `sun_path`, so this lives in the short runtime dir, not in `target`.
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("cosmic-comp-test");
    let dir = base.join(format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("Failed to create test run directory");
    dir
}

/// A cosmic-config entry written before the compositor starts, e.g.
/// `("com.system76.CosmicComp", "autotile", "false")`.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    pub component: String,
    pub key: String,
    pub ron: String,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// `COSMIC_HEADLESS_OUTPUTS`, e.g. `1280x720,1280x720@2`.
    pub outputs: String,
    pub config: Vec<ConfigEntry>,
    pub rust_log: String,
    /// Compositor binary to run instead of the default, e.g. a baseline build for benchmarks.
    pub binary: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            outputs: "1280x720,1280x720".into(),
            config: Vec::new(),
            rust_log: "info".into(),
            binary: None,
        }
    }
}

impl Options {
    pub fn outputs(mut self, outputs: &str) -> Self {
        self.outputs = outputs.into();
        self
    }

    pub fn binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = Some(binary.into());
        self
    }

    pub fn config(mut self, component: &str, key: &str, ron: &str) -> Self {
        self.config.push(ConfigEntry {
            component: component.into(),
            key: key.into(),
            ron: ron.into(),
        });
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn center(&self) -> (f64, f64) {
        (
            self.x as f64 + self.width as f64 / 2.0,
            self.y as f64 + self.height as f64 / 2.0,
        )
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > x && bottom > y).then(|| Rect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub geometry: Rect,
    pub scale: f64,
    /// Renders of the output so far.
    pub renders: u64,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub title: String,
    pub app_id: String,
    /// Includes the server-side title bar.
    pub geometry: Rect,
    pub output: String,
    pub workspace: u64,
    pub active_workspace: bool,
    pub floating: bool,
    pub maximized: bool,
    /// The output whose renders send the window its frame callbacks.
    pub primary_output: Option<String>,
    /// Increases every time a window is raised, across outputs: of two overlapping floating
    /// windows, the one with the higher value is on top.
    pub stacking: u64,
}

impl WindowInfo {
    /// A point on the server-side title bar, away from the buttons.
    pub fn header_point(&self) -> (f64, f64) {
        (
            self.geometry.x as f64 + 40.0,
            self.geometry.y as f64 + HEADER_HEIGHT as f64 / 2.0,
        )
    }

    /// The client area (below the title bar) in global coordinates.
    pub fn content(&self) -> Rect {
        Rect {
            x: self.geometry.x,
            y: self.geometry.y + HEADER_HEIGHT,
            width: self.geometry.width,
            height: self.geometry.height - HEADER_HEIGHT,
        }
    }
}

pub struct Compositor {
    child: Child,
    dir: PathBuf,
    runtime_dir: PathBuf,
    socket: String,
    control: BufReader<UnixStream>,
    last_release: Option<Instant>,
}

impl Compositor {
    pub fn start(options: Options) -> Result<Self> {
        let binary = options.binary.clone().unwrap_or_else(compositor_binary);
        if !binary.exists() {
            return Err(format!(
                "{} does not exist, build it with `cargo build --profile dev-opt` \
                 (or use scripts/test.sh)",
                binary.display()
            ));
        }

        let dir = run_dir();
        let runtime_dir = dir.join("run");
        let config_dir = dir.join("config");
        let state_dir = dir.join("state");
        for d in [&runtime_dir, &config_dir, &state_dir] {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        set_private(&runtime_dir);
        for entry in &options.config {
            let path = config_dir.join("cosmic").join(&entry.component).join("v1");
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            std::fs::write(path.join(&entry.key), &entry.ron).map_err(|e| e.to_string())?;
        }

        let control_path = dir.join("control.sock");
        let log_path = dir.join("cosmic-comp.log");
        let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let child = Command::new(&binary)
            .arg("--no-xwayland")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", &config_dir)
            .env("XDG_STATE_HOME", &state_dir)
            .env("DBUS_SESSION_BUS_ADDRESS", "disabled:")
            .env("COSMIC_BACKEND", "headless")
            .env("COSMIC_HEADLESS_OUTPUTS", &options.outputs)
            .env("COSMIC_HEADLESS_CONTROL", &control_path)
            .env("RUST_LOG", &options.rust_log)
            .env("RUST_BACKTRACE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("Failed to start {}: {e}", binary.display()))?;

        let mut compositor = Compositor {
            child,
            dir,
            runtime_dir,
            socket: String::new(),
            control: BufReader::new(UnixStream::pair().map_err(|e| e.to_string())?.0),
            last_release: None,
        };

        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            if let Some(status) = compositor.child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!(
                    "cosmic-comp exited during startup ({status}), log: {}",
                    log_path.display()
                ));
            }
            // The runtime dir is private to this compositor, so its only `wayland-N` socket is ours.
            if let Some(socket) = wayland_socket(&compositor.runtime_dir)
                && let Ok(stream) = UnixStream::connect(&control_path)
            {
                compositor.socket = socket.to_string();
                stream
                    .set_read_timeout(Some(DEFAULT_TIMEOUT))
                    .map_err(|e| e.to_string())?;
                compositor.control = BufReader::new(stream);
                break;
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "Timed out waiting for cosmic-comp, log: {}",
                    log_path.display()
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }

        Ok(compositor)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("cosmic-comp.log")).unwrap_or_default()
    }

    /// Sends a control request and returns the response, failing on `"ok": false`.
    pub fn request(&mut self, request: Value) -> Result<Value> {
        let stream = self.control.get_mut();
        writeln!(stream, "{request}").map_err(|e| format!("control write failed: {e}"))?;
        let mut line = String::new();
        self.control
            .read_line(&mut line)
            .map_err(|e| format!("control read failed: {e}"))?;
        if line.is_empty() {
            return Err(format!(
                "cosmic-comp closed the control socket (crashed?), log: {}",
                self.dir.join("cosmic-comp.log").display()
            ));
        }
        let response: Value =
            serde_json::from_str(&line).map_err(|e| format!("bad response `{line}`: {e}"))?;
        if response["ok"] == json!(true) {
            Ok(response)
        } else {
            Err(format!("{request} failed: {}", response["error"]))
        }
    }

    pub fn outputs(&mut self) -> Result<Vec<OutputInfo>> {
        let response = self.request(json!({"cmd": "outputs"}))?;
        Ok(response["outputs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| OutputInfo {
                name: o["name"].as_str().unwrap_or_default().to_string(),
                geometry: rect(o),
                scale: o["scale"].as_f64().unwrap_or(1.0),
                renders: o["renders"].as_u64().unwrap_or_default(),
            })
            .collect())
    }

    pub fn output(&mut self, name: &str) -> Result<OutputInfo> {
        self.outputs()?
            .into_iter()
            .find(|o| o.name == name)
            .ok_or_else(|| format!("no output {name}"))
    }

    pub fn windows(&mut self) -> Result<Vec<WindowInfo>> {
        let response = self.request(json!({"cmd": "windows"}))?;
        Ok(response["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| WindowInfo {
                title: w["title"].as_str().unwrap_or_default().to_string(),
                app_id: w["app_id"].as_str().unwrap_or_default().to_string(),
                geometry: rect(w),
                output: w["output"].as_str().unwrap_or_default().to_string(),
                workspace: w["workspace"].as_u64().unwrap_or_default(),
                active_workspace: w["active_workspace"].as_bool().unwrap_or_default(),
                floating: w["floating"].as_bool().unwrap_or_default(),
                maximized: w["maximized"].as_bool().unwrap_or_default(),
                primary_output: w["primary_output"].as_str().map(str::to_string),
                stacking: w["stacking"].as_u64().unwrap_or_default(),
            })
            .collect())
    }

    pub fn window(&mut self, title: &str) -> Result<WindowInfo> {
        self.windows()?
            .into_iter()
            .find(|w| w.title == title)
            .ok_or_else(|| format!("no window titled {title}"))
    }

    /// Polls `windows` until `predicate` holds for the window titled `title`.
    pub fn wait_window(
        &mut self,
        title: &str,
        what: &str,
        predicate: impl Fn(&WindowInfo) -> bool,
    ) -> Result<WindowInfo> {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        let mut last = None;
        loop {
            if let Some(window) = self.windows()?.into_iter().find(|w| w.title == title) {
                if predicate(&window) {
                    return Ok(window);
                }
                last = Some(window);
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "timed out waiting for window {title} to be {what}, last: {last:?}"
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn pointer(&mut self) -> Result<(f64, f64)> {
        let response = self.request(json!({"cmd": "pointer"}))?;
        Ok((
            response["x"].as_f64().unwrap_or_default(),
            response["y"].as_f64().unwrap_or_default(),
        ))
    }

    pub fn pointer_motion(&mut self, (x, y): (f64, f64)) -> Result<()> {
        self.request(json!({"cmd": "pointer_motion", "x": x, "y": y}))
            .map(|_| ())
    }

    pub fn button(&mut self, button: &str, pressed: bool) -> Result<()> {
        self.request(json!({"cmd": "pointer_button", "button": button, "pressed": pressed}))?;
        if !pressed {
            self.last_release = Some(Instant::now());
        }
        Ok(())
    }

    /// Left click at `at`, far enough from the last release not to count as a double click.
    pub fn click(&mut self, at: (f64, f64)) -> Result<()> {
        if let Some(elapsed) = self.last_release.map(|t| t.elapsed()) {
            thread::sleep(Duration::from_millis(400).saturating_sub(elapsed));
        }
        self.pointer_motion(at)?;
        self.button("left", true)?;
        self.button("left", false)?;
        self.request(json!({"cmd": "sync"})).map(|_| ())
    }

    pub fn key(&mut self, key: u32, pressed: bool) -> Result<()> {
        self.request(json!({"cmd": "key", "key": key, "pressed": pressed}))
            .map(|_| ())
    }

    /// Left-button drag from `from` to `to` in `steps` motion events, with a short pause
    /// between events so the compositor sees a realistic stream.
    pub fn drag(&mut self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
        // A press soon after the previous release would be a double click, which maximizes
        // windows when it lands on a title bar (iced's double click interval is 300ms).
        if let Some(elapsed) = self.last_release.map(|t| t.elapsed()) {
            thread::sleep(Duration::from_millis(400).saturating_sub(elapsed));
        }
        self.pointer_motion(from)?;
        self.button("left", true)?;
        // Start with a small nudge like a real pointer would: drag detection (e.g. on server-side
        // title bars) only starts if the first motion after the press is still on the element.
        let distance = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
        if distance > 4.0 {
            let t = 4.0 / distance;
            self.pointer_motion((from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t))?;
            thread::sleep(Duration::from_millis(5));
        }
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            self.pointer_motion((from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t))?;
            thread::sleep(Duration::from_millis(5));
        }
        self.button("left", false)?;
        // Let the grab's drop handling (queued as an idle callback) run.
        thread::sleep(Duration::from_millis(50));
        self.request(json!({"cmd": "sync"})).map(|_| ())
    }

    /// Moves the pointer by `(dx, dy)` through the relative motion path, like a mouse on real
    /// hardware: the position is clamped to outputs and grabs may restrict it.
    pub fn pointer_relative(&mut self, (dx, dy): (f64, f64)) -> Result<()> {
        self.request(json!({"cmd": "pointer_motion_relative", "dx": dx, "dy": dy}))
            .map(|_| ())
    }

    /// Like [`Compositor::drag`], but after warping to `from` and pressing, moves with relative
    /// motion like a mouse. Use it for anything that depends on how real pointers move between
    /// outputs (absolute motion warps and skips that logic).
    pub fn drag_relative(&mut self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
        if let Some(elapsed) = self.last_release.map(|t| t.elapsed()) {
            thread::sleep(Duration::from_millis(400).saturating_sub(elapsed));
        }
        self.pointer_motion(from)?;
        self.button("left", true)?;
        let distance = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
        let mut current = from;
        let mut move_to = |comp: &mut Self, next: (f64, f64)| -> Result<()> {
            comp.pointer_relative((next.0 - current.0, next.1 - current.1))?;
            current = next;
            thread::sleep(Duration::from_millis(5));
            Ok(())
        };
        if distance > 4.0 {
            let t = 4.0 / distance;
            move_to(
                self,
                (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t),
            )?;
        }
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            move_to(
                self,
                (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t),
            )?;
        }
        self.button("left", false)?;
        thread::sleep(Duration::from_millis(50));
        self.request(json!({"cmd": "sync"})).map(|_| ())
    }

    /// Drags a window by its title bar so its top-left corner ends up at `to`.
    pub fn drag_window_to(&mut self, title: &str, to: (i32, i32)) -> Result<WindowInfo> {
        let window = self.window(title)?;
        let from = window.header_point();
        let target = (
            from.0 + (to.0 - window.geometry.x) as f64,
            from.1 + (to.1 - window.geometry.y) as f64,
        );
        self.drag(from, target, 20)?;
        self.wait_window(title, &format!("at {to:?}"), |w| {
            (w.geometry.x, w.geometry.y) == to
        })
    }

    pub fn screenshot(&mut self, output: &str) -> Result<Image> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = self.dir.join(format!(
            "{output}-{}.png",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        self.request(json!({"cmd": "screenshot", "output": output, "path": path}))?;
        Image::load(&path)
    }

    /// Takes screenshots of `output` until `predicate` holds, e.g. after an animation.
    pub fn wait_screenshot(
        &mut self,
        output: &str,
        what: &str,
        predicate: impl Fn(&Image) -> bool,
    ) -> Result<Image> {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            let image = self.screenshot(output)?;
            if predicate(&image) {
                return Ok(image);
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "timed out waiting for {output} to show {what}, artifacts in {}",
                    self.dir.display()
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Presses and releases `keys` (evdev codes) as a chord, e.g. `[KEY_LEFTMETA, KEY_2]`.
    pub fn chord(&mut self, keys: &[u32]) -> Result<()> {
        for key in keys {
            self.key(*key, true)?;
        }
        for key in keys.iter().rev() {
            self.key(*key, false)?;
        }
        self.request(json!({"cmd": "sync"})).map(|_| ())
    }

    /// Starts a `test-client` window. `args` are passed through (e.g. `--color`, `--width`).
    pub fn spawn_client(&mut self, title: &str, args: &[&str]) -> Result<Client> {
        let binary = client_binary();
        let mut child = Command::new(&binary)
            .args(["--title", title])
            .args(args)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("WAYLAND_DISPLAY", &self.socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to start {}: {e}", binary.display()))?;

        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let value = serde_json::from_str(&line)
                    .unwrap_or_else(|_| json!({"event": "unparsed", "line": line}));
                if tx.send(value).is_err() {
                    break;
                }
            }
        });

        let mut client = Client {
            title: title.to_string(),
            stdin: child.stdin.take(),
            child,
            events: rx,
            history: Vec::new(),
            outputs: BTreeSet::new(),
        };
        client.wait_event("ready", |e| e["event"] == "ready")?;
        Ok(client)
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let keep = std::env::var_os("COSMIC_TEST_KEEP").is_some_and(|v| v != "0");
        if thread::panicking() || keep {
            eprintln!("cosmic-comp test artifacts: {}", self.dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

fn rect(value: &Value) -> Rect {
    let get = |key: &str| value[key].as_i64().unwrap_or_default() as i32;
    Rect {
        x: get("x"),
        y: get("y"),
        width: get("width"),
        height: get("height"),
    }
}

pub struct Client {
    pub title: String,
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Value>,
    /// Every event received so far, in order.
    pub history: Vec<Value>,
    outputs: BTreeSet<String>,
}

impl Client {
    pub fn send(&mut self, command: Value) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or("client stdin closed")?;
        writeln!(stdin, "{command}").map_err(|e| format!("client write failed: {e}"))?;
        stdin.flush().map_err(|e| e.to_string())
    }

    fn record(&mut self, event: Value) {
        match event["event"].as_str() {
            Some("enter") => {
                self.outputs
                    .insert(event["output"].as_str().unwrap_or_default().to_string());
            }
            Some("leave") => {
                self.outputs
                    .remove(event["output"].as_str().unwrap_or_default());
            }
            _ => {}
        }
        self.history.push(event);
    }

    /// Waits for the next event matching `predicate`, recording everything received meanwhile.
    pub fn wait_event(&mut self, what: &str, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(event) => {
                    if event["event"] == "error" {
                        return Err(format!("client {} failed: {event}", self.title));
                    }
                    let matched = predicate(&event);
                    self.record(event.clone());
                    if matched {
                        return Ok(event);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "client {} timed out waiting for {what}, history: {:?}",
                        self.title, self.history
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "client {} exited while waiting for {what}, history: {:?}",
                        self.title, self.history
                    ));
                }
            }
        }
    }

    /// Opens a popup with its top-left corner at `at` in window surface coordinates (below the
    /// title bar), unless the compositor moves it to fit, and waits until it's drawn. Returns
    /// where the compositor placed it, in the same coordinates.
    ///
    /// With `grab`, it takes an explicit grab with the client's last button press, like menus.
    pub fn open_popup(
        &mut self,
        at: (i32, i32),
        size: (u32, u32),
        color: &str,
        grab: bool,
    ) -> Result<(i32, i32)> {
        self.send(json!({
            "cmd": "popup",
            "x": at.0,
            "y": at.1,
            "width": size.0,
            "height": size.1,
            "color": color,
            "grab": grab,
        }))?;
        let configure = self.wait_event("popup_configure", |e| e["event"] == "popup_configure")?;
        self.wait_event("popup_drawn", |e| e["event"] == "popup_drawn")?;
        Ok((
            configure["x"].as_i64().unwrap_or_default() as i32,
            configure["y"].as_i64().unwrap_or_default() as i32,
        ))
    }

    /// Round-trips with the compositor, so every event it sent before is recorded.
    pub fn sync(&mut self) -> Result<()> {
        self.send(json!({"cmd": "sync"}))?;
        self.wait_event("synced", |e| e["event"] == "synced")
            .map(|_| ())
    }

    /// Outputs the surface is currently entered on, according to `wl_surface.enter/leave`.
    ///
    /// cosmic-comp updates output membership in its refresh pass, which is throttled to once
    /// every 150ms, so after a change prefer [`Client::wait_entered_outputs`].
    pub fn entered_outputs(&mut self) -> Result<BTreeSet<String>> {
        self.sync()?;
        Ok(self.outputs.clone())
    }

    /// Waits until the surface is entered on exactly `expected`, then checks it stays that way
    /// for longer than one refresh interval.
    pub fn wait_entered_outputs(&mut self, expected: &[&str]) -> Result<()> {
        let expected = expected
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>();
        self.wait_outputs_where(&format!("entered on {expected:?}"), |current| {
            *current == expected
        })
    }

    /// Waits until the set of entered outputs satisfies `predicate`, then checks it still does
    /// after longer than one refresh interval.
    pub fn wait_outputs_where(
        &mut self,
        what: &str,
        predicate: impl Fn(&BTreeSet<String>) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            let current = self.entered_outputs()?;
            if predicate(&current) {
                thread::sleep(Duration::from_millis(200));
                let settled = self.entered_outputs()?;
                return if predicate(&settled) {
                    Ok(())
                } else {
                    Err(format!(
                        "client {} was {what} but then changed to {settled:?}",
                        self.title
                    ))
                };
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "client {} is entered on {current:?}, expected to be {what}",
                    self.title
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Events recorded since `index` into [`Client::history`].
    pub fn events_since(&self, index: usize) -> &[Value] {
        &self.history[index.min(self.history.len())..]
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.send(json!({"cmd": "quit"}));
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An RGBA screenshot of one output, in physical pixels.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Image {
    pub fn load(path: &Path) -> Result<Image> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut decoder = png::Decoder::new(BufReader::new(file));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        let mut data = vec![0; reader.output_buffer_size().unwrap_or_default()];
        let info = reader.next_frame(&mut data).map_err(|e| e.to_string())?;
        if info.color_type != png::ColorType::Rgba {
            return Err(format!("unexpected color type {:?}", info.color_type));
        }
        data.truncate(info.buffer_size());
        Ok(Image {
            width: info.width,
            height: info.height,
            data,
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Fraction of pixels in `rect` (image coordinates, clamped) within `tolerance` of `rgb`.
    pub fn coverage(&self, rect: Rect, rgb: [u8; 3], tolerance: u8) -> f64 {
        let x0 = rect.x.clamp(0, self.width as i32) as u32;
        let y0 = rect.y.clamp(0, self.height as i32) as u32;
        let x1 = rect.right().clamp(0, self.width as i32) as u32;
        let y1 = rect.bottom().clamp(0, self.height as i32) as u32;
        let total = (x1.saturating_sub(x0) * y1.saturating_sub(y0)) as f64;
        if total == 0.0 {
            return 0.0;
        }
        let mut matching = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let p = self.pixel(x, y);
                if (0..3).all(|c| p[c].abs_diff(rgb[c]) <= tolerance) {
                    matching += 1;
                }
            }
        }
        matching as f64 / total
    }

    /// Fraction of the whole image within `tolerance` of `rgb`.
    pub fn total_coverage(&self, rgb: [u8; 3], tolerance: u8) -> f64 {
        self.coverage(
            Rect {
                x: 0,
                y: 0,
                width: self.width as i32,
                height: self.height as i32,
            },
            rgb,
            tolerance,
        )
    }
}

/// Parses `RRGGBB`.
pub fn rgb(hex: &str) -> [u8; 3] {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).expect("invalid colour");
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// Converts a global logical rect to the image coordinates of `output`.
pub fn to_output_pixels(rect: Rect, output: &OutputInfo) -> Rect {
    let s = output.scale;
    Rect {
        x: ((rect.x - output.geometry.x) as f64 * s).round() as i32,
        y: ((rect.y - output.geometry.y) as f64 * s).round() as i32,
        width: (rect.width as f64 * s).round() as i32,
        height: (rect.height as f64 * s).round() as i32,
    }
}

fn wayland_socket(runtime_dir: &Path) -> Option<String> {
    use std::os::unix::fs::FileTypeExt;
    std::fs::read_dir(runtime_dir)
        .ok()?
        .flatten()
        .find(|entry| {
            entry.file_type().is_ok_and(|t| t.is_socket())
                && entry.file_name().to_string_lossy().starts_with("wayland-")
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
}

/// evdev key codes (`linux/input-event-codes.h`) for [`Compositor::key`] and
/// [`Compositor::chord`].
pub mod keys {
    pub const KEY_ESC: u32 = 1;
    pub const KEY_1: u32 = 2;
    pub const KEY_2: u32 = 3;
    pub const KEY_3: u32 = 4;
    pub const KEY_LEFTCTRL: u32 = 29;
    pub const KEY_LEFTSHIFT: u32 = 42;
    pub const KEY_LEFTALT: u32 = 56;
    pub const KEY_UP: u32 = 103;
    pub const KEY_LEFT: u32 = 105;
    pub const KEY_RIGHT: u32 = 106;
    pub const KEY_DOWN: u32 = 108;
    pub const KEY_LEFTMETA: u32 = 125;
}
