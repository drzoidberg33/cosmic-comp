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
`#[serde(default)]` works. Behaviour is verified by running the compositor.

## Running and manual testing

- **Backend selection:** `COSMIC_BACKEND=kms|x11|winit`. If unset and `DISPLAY` or
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
  backend/        kms/ (DRM/GBM, per-surface render threads), x11.rs, winit.rs, render/ (elements, shaders, cursor)
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
