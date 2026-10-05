# AGENTS.md

Guidance for coding agents working in this repository. This is a **personal fork** of
[pop-os/cosmic-comp](https://github.com/pop-os/cosmic-comp), the Wayland compositor for the
COSMIC desktop, built on [Smithay](https://github.com/Smithay/smithay).

Sources for this document: `README.md`, `justfile`/`cargo.just`, `Cargo.toml`,
`rust-toolchain.toml`, `rustfmt.toml`, `.github/` (CI + PR template), the pop-os
[contributor guide](https://github.com/pop-os/pop/blob/master/CONTRIBUTING.md), and, where
nothing is written down, upstream commit history and maintainer PR review comments.

## Fork context

- **Goal of the fork:** let windows (initially floating windows only) display across multiple
  outputs. Upstream tracking issue:
  [pop-os/cosmic-epoch#2273](https://github.com/pop-os/cosmic-epoch/issues/2273).
- **Remotes:** `origin` = `drzoidberg33/cosmic-comp` (the fork), `upstream` = `pop-os/cosmic-comp`.
- **Base fork branches on the commit Pop!_OS ships, not on `upstream/master`.** The compositor
  shares protocols (`cosmic-protocols`) and config formats with the panel, settings, applets,
  etc. Running a newer compositor against older apt-installed components can break things
  unrelated to the fork's changes. The shipped commit is the last component of the package
  version: `dpkg-query -W -f='${Version}\n' cosmic-comp` → `0.1~<timestamp>~24.04~<short-sha>`.
- **Keep fork changes small, isolated and easy to rebase.** Prefer gating new behaviour behind
  a config option or env var, and keep the diff out of hot, frequently-changed upstream code
  where possible. Each Pop update means rebasing onto the newly shipped commit.
- **Upstream does not accept LLM-generated contributions.** The pop-os contributor guide and
  the PR template forbid AI-generated code, comments and descriptions in issues and PRs. Work
  produced with agent help in this fork **must not be submitted upstream** as-is. Never open
  PRs, issues or comments against `pop-os/*` repositories.
- **Deployment:** the custom binary is installed outside the package (e.g.
  `/opt/cosmic-comp-custom/bin/cosmic-comp`) and launched via a separate login session that
  puts it first on `PATH`; `cosmic-session` runs `cosmic-comp` from `PATH`
  (or its first CLI argument). The stock "COSMIC" session stays as a fallback. Never
  overwrite `/usr/bin/cosmic-comp`.

## Setup

- Rust toolchain is pinned in `rust-toolchain.toml` (currently `1.93`, edition 2024,
  `rust-src`). Use `rustup` so the pin is honoured. `rust-version` in `Cargo.toml` matches.
- [`just`](https://github.com/casey/just) is the command runner on upstream `master`. The
  current fork base still uses `make` (see below).
- Install `rustup` from apt (`sudo apt install rustup just`), **not** Ubuntu's `cargo`/`rustc`
  packages, which conflict with it. The pinned toolchain downloads automatically on the first
  `cargo` run in the repo.
- System packages (from CI and `debian/control`):
  `libudev-dev libgbm-dev libxkbcommon-dev libegl1-mesa-dev libwayland-dev libinput-dev
  libdbus-1-dev libsystemd-dev libseat-dev libdisplay-info-dev libpixman-1-dev
  libfontconfig-dev cmake`
- Several dependencies are git dependencies (libcosmic, cosmic-protocols, cosmic-settings-daemon,
  Smithay pinned via `[patch.crates-io]`, Drakulix's `id-tree` fork). The first build needs
  network access. Respect `Cargo.lock`; don't run `cargo update` unless the task is a
  dependency update.

## Build, check, test

The build entry point depends on the base commit. Upstream `master` switched from `make` to
`just` in `af025856`, after the commit the fork's `span-outputs` branch is based on. Check
which file exists (`Makefile` or `justfile`). Plain `cargo` commands work on either.

| Task | `cargo` | `Makefile` (current fork base) | `justfile` (upstream master) |
|---|---|---|---|
| Debug build | `cargo build` | `make DEBUG=1` | `just build-debug` |
| Release build (fat LTO, slow) | `cargo build --release` | `make` | `just` |
| Optimised build with debuginfo | `cargo build --profile fastdebug` | — | — |
| Iteration build (fork-only) | `cargo build --profile dev-opt` | — | — |
| Lint (pedantic warnings) | `cargo clippy --all-features -- -W clippy::pedantic` | — | `just check` |
| Install to a prefix | — | `make install prefix=<p> DESTDIR=<d>` | `just rootdir=<d> prefix=<p> install` |

**Use `--profile dev-opt` while iterating.** It's a fork-only profile in `Cargo.toml`:
dependencies at `opt-level = 2` (built once and cached), cosmic-comp at `opt-level = 1`,
incremental, no LTO, with debug assertions and `debug!` logging. A rebuild after touching one
file takes about 16s, versus about 7 minutes for `fastdebug`/`release`, which inherit
`lto = "fat"` and non-incremental builds and re-run a mostly single-threaded LTO pass on every
change. The remaining time is rustc's single-threaded front end on the large `cosmic-comp`
crate, not linking (smaller debuginfo didn't help). Use `fastdebug`/`release` only to check
release-like performance or before deploying the custom binary.

Before considering a change done, run the same checks CI runs (`.github/workflows/ci.yml`):

```sh
cargo fmt --all -- --check
cargo clippy --all-features -- -D warnings
cargo check --no-default-features
cargo check --features debug
cargo check --features profile-with-tracy
cargo test --all-features
```

Cargo features: `systemd` (default; journald logging, logind), `logind`, `debug` (egui debug
overlay), `profile-with-tracy`, `profile-with-tracy-gpu`. Code must compile with and without
default features. Gate feature-specific code with `#[cfg(feature = "...")]`.

There are very few unit tests (`src/backend/render/cursor.rs`, `src/backend/kms/device.rs`,
`cosmic-comp-config/src/lib.rs`). Maintainers push back on trivial tests, e.g. testing that
`#[serde(default)]` works. Upstream verifies behaviour by running the compositor. This fork
also has automated integration tests (next section).

## Automated integration tests (fork-only)

**Run `scripts/test.sh` after every behavioural change** and before every commit. It builds
the compositor (`dev-opt`) and runs `cargo test` in `test-harness/`. Extra arguments go to
`cargo test`, e.g. `scripts/test.sh --test span` or `scripts/test.sh pointer`. The suite runs
in about a second, in parallel, and shows nothing on screen.

How it works:

- **Headless backend** (`src/backend/headless.rs`, `COSMIC_BACKEND=headless`): virtual
  outputs from `COSMIC_HEADLESS_OUTPUTS` (`WxH[@scale]`, comma separated), rendered offscreen
  with EGL on the first hardware render node (or llvmpipe with `COSMIC_HEADLESS_SOFTWARE=1`).
  Synthetic input goes through the normal `process_input_event` path via a `HeadlessInput`
  `InputBackend`.
- **Control socket** (`src/backend/headless/control.rs`, path in `COSMIC_HEADLESS_CONTROL`):
  JSON lines. Commands are `outputs`, `windows`, `pointer`, `pointer_motion`, `pointer_button`,
  `pointer_axis`, `key` (evdev codes), `screenshot` (renders synchronously to a PNG) and
  `sync`. The module docs describe the exact protocol. Extend it when a test needs to observe
  or drive something new, rather than sleeping or guessing.
- **`test-client`** (`test-harness/src/bin/test-client.rs`): a solid-colour xdg_toplevel. It
  reports `configure`, `ready`, `enter`/`leave` (output names), `preferred_buffer_scale`,
  pointer and keyboard events as JSON lines, and accepts `move`, `set_color`, `sync` and
  `quit` on stdin.
- **Library** (`test-harness/src/lib.rs`):
  - `Compositor::start(Options)` gives every compositor its own runtime, config and state dirs
    under `$XDG_RUNTIME_DIR/cosmic-comp-test/`, disables the session bus and runs
    `--no-xwayland`, so the host session is never touched.
  - `Options::config(component, key, ron)` seeds cosmic-config entries, e.g. workspace mode.
  - Helpers: `spawn_client`, `windows`/`wait_window`, `drag`/`drag_window_to` (real title-bar
    drags), `screenshot` → `Image::coverage` for pixel checks, and `Client::entered_outputs`
    (syncs first).
- Window geometry from `windows` includes the 36px server-side title bar
  (`HEADER_HEIGHT`). `WindowInfo::content()` is the client area.
- Failing tests keep their run directory (log, screenshots, config) and print its path. Set
  `COSMIC_TEST_KEEP=1` to keep passing ones too. View screenshots to debug rendering.
- Unix socket paths are limited to 108 bytes, so never put compositor runtime dirs under a
  long path (e.g. a scratchpad or `target/`).

Writing tests:

- Assert on what clients and users observe (pixels, `wl_surface.enter/leave`, pointer
  events), plus `windows` for layout. Avoid asserting on implementation details.
- Synchronise instead of sleeping: control requests are handled in order, `Client::sync`
  round-trips, and `wait_window`/`wait_event` poll with a 10s timeout.
- Keep `cargo test` green at every commit. A feature's failing tests land together with
  its implementation.
- Check new tests for flakiness by running them repeatedly, e.g. 20 times in a loop.

## Performance benchmarks (fork-only)

**Run `scripts/bench.sh` before committing anything that touches rendering, input
hit-testing, `Shell`/`Workspace` refresh or the floating layout.** It exits non-zero on a
regression.

- It does an A/B run of the working tree against a baseline revision (`--baseline REV`,
  default: the `span-baseline` tag, the last commit before the span-outputs feature). Both
  sides are built with the `bench-opt` profile: release optimisation, no fat LTO,
  incremental. The baseline is built in a git worktree at `target/bench-baseline/` with its
  own target dir. The first run builds it from scratch (a few minutes).
- Scenarios (`test-harness/src/bin/bench.rs`, list them with `bench list`) cover 1–4
  outputs, 1–20 floating windows, windows straddling seams, and mixed scales. Each one starts
  a fresh headless compositor and places windows with real title-bar drags.
- Metrics are measured inside the compositor via the control socket's `bench_*` commands,
  so there's no IPC noise:
  - `render/<output>/cpu` and `/total`: a full redraw of each output, until submit and until
    `glFinish`.
  - `input/surface_under` and `/element_under`: pointer hit-testing over windows, the
    desktop and overhangs.
  - `refresh`: `Common::refresh`.
- Rounds alternate baseline and candidate (`--rounds N`, default 3). The comparison uses the
  median over rounds of each metric's median.
- A metric regresses only when it is slower than both the relative and the absolute
  threshold allow (see `threshold()` in `bench.rs`). Scenarios with spanning windows have
  looser render and input thresholds, because drawing and hit-testing a window on two
  outputs is real extra work. Non-spanning scenarios must stay within noise of the baseline.
- `--filter SUBSTR` runs a subset, and `--json FILE` writes the comparison.
  `test-harness/target/release/bench run --binary BIN` measures a single build.
- Results depend on the machine and its load. Close heavy programs and compare A/B runs
  from the same session only. Never compare numbers across machines or days.
- When a regression is real and accepted (e.g. new work that the feature requires), explain
  it in the commit message with the numbers. Don't just loosen thresholds.

## Running and manual testing

- **Backend selection:** `COSMIC_BACKEND=kms|x11|winit|headless`. If unset and `DISPLAY` or
  `WAYLAND_DISPLAY` is set, it starts nested (x11, falling back to winit); otherwise it uses KMS.
- **Nested:** from inside a running desktop session, `cargo run` opens the compositor in a
  window. Run clients inside it by pointing them at the nested socket (`WAYLAND_DISPLAY`,
  which is logged at startup).
- **Logging:** `RUST_LOG` (tracing `EnvFilter`). Default is `info` in debug builds and `warn` in
  release. The `tracing` dependency sets `release_max_level_info`, so `debug!`/`trace!` are
  **compiled out** of release and `fastdebug` builds. Use `dev` or `dev-opt` to see them.
  With the `systemd` feature, logs also go to the journal
  (`journalctl --user -b -t cosmic-comp` or similar).
- **Multiple nested outputs (fork-only):** the X11 backend opens one window per output listed
  in `COSMIC_X11_OUTPUTS`, given as a count (`2`) or sizes (`1280x720,1920x1080`). Outputs
  are laid out left to right by name (`X11-0`, `X11-1`, ...) by the fallback layout in
  `Config::read_outputs`. Moving the host pointer from one window to the next crosses into
  the next output. The winit backend is still single-output.
- **`scripts/nested.sh [client args...]`** builds (`PROFILE`, default `dev-opt`) and runs the
  nested compositor with two 1280x720 outputs, `RUST_LOG=info`, and an
  isolated `XDG_STATE_HOME`. That keeps nested layouts out of the real
  `~/.local/state/cosmic-comp/outputs.ron`. It waits for the socket, then launches the client
  (default `cosmic-term`) against it. The log goes to the console and to
  `$XDG_RUNTIME_DIR/cosmic-comp-nested/cosmic-comp.log`. Start more clients with the printed
  socket: `WAYLAND_DISPLAY=wayland-N cosmic-edit`. Ctrl+C or closing the windows stops it.
- **Don't pass clients as `cosmic-comp` arguments for testing.** `cosmic-comp <cmd>` is
  **kiosk mode**: it skips loading shortcuts (`Config::load`) and shuts the compositor down
  as soon as `<cmd>` exits. COSMIC apps fork into the background and exit immediately, so the
  compositor quits right away and the forked app then panics with "Connection reset by peer".
- **Rebuild quirk:** `build.rs` doesn't declare `rerun-if-changed`, so editing *any* file in
  the package (docs and scripts included) reruns it and recompiles the crate.
- **Shared config:** the nested
  instance shares your real cosmic-config (workspace mode, autotile, keybindings), so set
  "workspaces span displays" etc. in Settings to test those modes. D-Bus name requests
  (`com.system76.CosmicComp`, the a11y manager) fail harmlessly while the host compositor owns
  them.
- **What nested testing can't cover:** KMS-specific paths (DRM planes, direct scanout, VRR,
  mixed GPUs), real fractional-scale hardware (scale and position changes go through the generic output
  config path, so `cosmic-randr` against the nested socket should work, but this is
  unverified), and the host compositor grabbing `Super` shortcuts.
  Do final verification on real monitors through the custom login session, and keep the stock
  session available to recover from crashes.

## Repository layout

```
src/
  backend/        kms/ (DRM/GBM, per-surface render threads), x11.rs, winit.rs, headless{.rs,/control.rs}
                  (fork-only, tests), render/ (elements, shaders, cursor)
  shell/          Shell + Workspaces (mod.rs), workspace.rs, layout/{floating,tiling}, element/ (window, stack, surface),
                  grabs/ (move, resize, menu), focus/, zoom.rs
  input/          libinput/seat input handling, hit-testing (surface_under / element_under), actions, gestures
  wayland/        protocol handlers/ and custom protocols/ (workspace, toplevel_info, output_configuration, ...)
  config/         runtime config (cosmic-config watchers); persisted types live in cosmic-comp-config/
  dbus/, logger/, utils/ (prelude.rs, iced/ for compositor-side UI), xwayland.rs, state.rs
cosmic-comp-config/  serde config types shared with cosmic-settings (workspace member)
data/                keybindings.ron, tiling-exceptions.ron, session/systemd files
resources/i18n/      Fluent translations (managed by Weblate; only edit `en/`)
debian/              packaging (vendored build via `just build-vendored`)
test-harness/        fork-only integration tests (own Cargo.lock): test-client, harness lib, tests/
scripts/             fork-only: nested.sh (manual multi-output), test.sh (automated tests)
```

Notes relevant to multi-output work:

- `Workspaces.sets: IndexMap<Output, WorkspaceSet>` (`src/shell/mod.rs`). Each `Workspace` has
  exactly one `output`. `WorkspaceMode::Global` ("workspaces span displays") still keeps one
  set per output, with workspaces kept in step by index.
- `FloatingLayout` holds a smithay `Space` with one output mapped at `(0,0)`. Many call sites
  rely on `self.space.outputs().next().unwrap()`.
- Per-output rendering is built in `workspace_elements` / `render_output`
  (`src/backend/render/mod.rs`) and crops to the output. `render_input_order` defines the
  stacking order and **must stay in sync** with the input hit-testing functions that iterate
  surfaces (see the comments added in `f9e02f43`).
- `MoveGrab` (`src/shell/grabs/moving.rs`) already renders a window on every output it
  overlaps and manages `output_enter`/`output_leave` during a drag. Its `Drop` impl
  re-homes the window to the cursor's output.
- Coordinate spaces are typed: `Global` vs `Local` (output-relative) vs smithay's
  `Logical`/`Physical`. Convert explicitly with the helpers in `utils/prelude.rs`
  (`as_global`, `to_local(&output)`, `to_global(&output)`, `as_logical`, ...). Never mix them
  with raw `.loc` arithmetic across spaces.

## Code style

- `rustfmt` with `style_edition = "2024"` (`rustfmt.toml`). Formatting is enforced in CI.
- Every source file starts with `// SPDX-License-Identifier: GPL-3.0-only`. The license is
  GPL-3.0-only.
- Imports: a `use crate::{...}` block first, then external crates (`smithay`, `calloop`,
  `cosmic`, `std`, ...), using nested brace groups as `rustfmt` lays them out. Many modules use
  `crate::utils::prelude::*`.
- Edition-2024 idioms are used widely: `let ... else { return }`, let-chains
  (`if let Some(x) = a && cond`), `?` with `anyhow::Context`.
- Errors: `anyhow::Result` + `.context(...)` for fallible setup paths. `thiserror` for typed
  errors. `unwrap()` is common for real invariants. Maintainers prefer **crashing on a logic
  error** over silently ignoring it: don't turn an invariant into a swallowed error.
- Logging: `tracing` macros with structured fields and short messages, e.g.
  `error!(?err, "Failed to resume drm device");`. Don't write long or chatty log messages.
- Performance-sensitive paths (render, input) avoid needless heap allocation and locking.
  Prefer `SmallVec` for small collections, and don't take extra mutexes or `user_data` lookups
  in hot paths. Use `profiling` annotations where the surrounding code does.
- Reuse what exists before adding helpers. For example, use the `keyframe` crate for easing,
  smithay APIs (`data.update(...)` not `data.set(data.take())`), and existing prelude
  conversions.
- UI strings go through `fl!("message-id")` (Fluent, `i18n.toml`, `resources/i18n/en/`).
- Comments: short and only for non-obvious *why* (a link to an issue/PR is welcome).
  Rationale and history belong in the commit message, not code comments. Keep TODOs specific.
- Config (`cosmic-comp-config`): changes to persisted structs are reviewed very carefully.
  Keep serde naming consistent with existing types (no ad-hoc `rename_all = "lowercase"`), add
  `#[serde(default)]` for new fields so old configs still load, and migrate keys rather than
  breaking them.

## Design norms from maintainer reviews

Recurring feedback from upstream maintainers (Drakulix, ids1024, hojjatabdollahi). Apply it in
the fork too:

- **Fix the root cause, not the symptom.** Workarounds, magic numbers ("value determined by
  experimentation won't fly"), and patches that just make a symptom disappear get rejected.
  If the bug is in Smithay, the right fix is in Smithay.
- **Don't introduce error-prone call-site obligations.** If every caller must remember to call
  something (e.g. an alpha adjustment or a "blocked by modal" check), move the logic into the
  shared function instead (e.g. into `CosmicSurface::push_render_elements`).
- **Don't duplicate state or truth.** Derive from existing sources (e.g. the seat's keyboard
  focus) instead of tracking a parallel copy.
- **Respect existing abstractions.** For example, the focus module treats a stack as a single
  target, so don't special-case concrete target types inside generic logic.
- **Avoid duplicated code paths and early-return branches that copy logic.** Factor out
  shared parts. For recursive helpers use a public wrapper plus an `_internal` function.
- **Avoid hidden side effects.** Pass values (e.g. `DrmNode`) explicitly instead of using
  thread-locals or globals.
- **Backend correctness:** only advertise capabilities a backend can actually deliver. KMS
  changes must keep atomic commits all-or-nothing, with valid intermediate states.
- **Be multi-seat aware.** Iterate all seats rather than assuming one.
- Keep changes minimal and targeted. Drop unrelated renames and reformatting.

## Commits

Upstream history uses two subject styles, both short and imperative:

- Conventional-commit style, lowercase after the colon: `fix: ...`, `feat: ...`, `chore: ...`,
  `refactor: ...`, `perf: ...`, optionally scoped like `fix(kms): ...`.
- Module-path prefix, capitalised after the colon: `input: Update the pointer state ...`,
  `shell/focus: Don't dismiss ...`, `element/stack: ...`, `backend/kms: ...`.

Guidelines:

- One logical change per commit. Larger work lands as a series of focused commits (e.g. config
  types → behaviour → follow-ups), not one squashed blob. Upstream merges PRs with merge commits.
- Add a body when the *why* isn't obvious from the subject. Wrap the body at about 72 columns.
  Link related PRs or issues.
- Disclose agent assistance with a `Co-authored-by:` / `Co-Authored-By:` trailer.
- Commits must leave the tree building and passing `cargo fmt --check` and clippy.
- Translations arrive via Weblate (`i18n: translation updates from weblate`). Don't hand-edit
  non-English locales.

## Git workflow (fork)

- Work on feature branches in the fork (e.g. `span-outputs`) based on the shipped Pop commit.
  Don't commit to `master`; it mirrors upstream.
- Ask before pushing, force-pushing, or creating PRs, even on the fork.
- To pick up a new Pop release: find the new shipped SHA, `git fetch upstream`, rebase the
  feature branch onto that SHA, rebuild, and reinstall the custom binary.
