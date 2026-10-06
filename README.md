# cosmic-comp: windows spanning multiple displays

This is a personal fork of [cosmic-comp](https://github.com/pop-os/cosmic-comp), the
compositor of the COSMIC desktop (Pop!_OS 24.04). On the `span-outputs` branch, floating
windows can span several displays. A window placed across the boundary between two
monitors is shown, and can be used, on both, instead of being cut off at the edge of the
display it belongs to.

This addresses [pop-os/cosmic-epoch#2273](https://github.com/pop-os/cosmic-epoch/issues/2273),
e.g. remote desktop clients (`xfreerdp /multimon`, Horizon, Citrix) or spreadsheets stretched
across monitors.

> **Not an upstream contribution.** This branch was developed with the help of an AI
> assistant. The Pop!_OS contribution policy doesn't accept AI-generated code, so please
> don't submit it to `pop-os/*`. Report problems with this branch in this fork instead.

## What works

- **Drawing:** floating windows straddling displays are drawn on every display they
  overlap, including the title bar, border and shadow, at each display's own scale.
- **Input:** pointer input, focus and title-bar drags work on the part of the window that
  hangs over onto the other display.
- **Output membership:** apps get `wl_surface.enter`/`leave` for every display their window
  is on.
- **Resizing:** edges follow the pointer across display boundaries. A window that ends up
  entirely on another display moves there in place.
- **Layouts:** any arrangement, side by side or stacked, and both workspace modes (per
  display, or spanning displays).
- **Cost:** none for setups without spanning windows (see [Benchmarks](#benchmarks)).

**Known limitations:**
- Only floating windows span; tiled, maximized and sticky windows don't.
- Popups of a spanning window aren't shown on the other display yet.
- An overhanging window is always stacked above the other display's own windows.
- When workspaces are switched, the overhang doesn't follow the slide animation.
- Panels and docks only see the window on its home display.

## Building

The branch is based on upstream commit
[`0fbd457`](https://github.com/pop-os/cosmic-comp/commit/0fbd4574ef4caf74769a617d205fd1fc909ac9b1),
the version Pop!_OS shipped as `cosmic-comp 0.1~1790118748~24.04~0fbd457`. The compositor
talks to the panel, settings and other COSMIC components. Check your installed version with
`dpkg-query -W cosmic-comp`; if it's much newer, expect possible incompatibilities.

### Dependencies (Pop!_OS 24.04 / Ubuntu 24.04)

```sh
sudo apt install rustup git pkg-config cmake \
    libudev-dev libgbm-dev libxkbcommon-dev libegl1-mesa-dev libwayland-dev libinput-dev \
    libdbus-1-dev libsystemd-dev libseat-dev libdisplay-info-dev libpixman-1-dev \
    libfontconfig-dev libxcb1-dev
```

Use `rustup`, not Ubuntu's `cargo`/`rustc` packages, which conflict with it. The Rust
toolchain is pinned in `rust-toolchain.toml` (1.93) and is downloaded automatically by the
first `cargo` command in the repository.

### Build

```sh
git clone -b span-outputs https://github.com/drzoidberg33/cosmic-comp.git
cd cosmic-comp
cargo build --release
```

The release build uses fat LTO and takes about 5–8 minutes. The binary is
`target/release/cosmic-comp`.

## Installing and trying it

`scripts/install-session.sh` installs the build as a separate login session, **"COSMIC
(span-outputs)"**. The packaged `/usr/bin/cosmic-comp` and the stock "COSMIC" session are
not touched, so you can always go back by picking "COSMIC" on the login screen.

```sh
scripts/install-session.sh            # asks for sudo
```

Then log out, pick **COSMIC (span-outputs)** on the login screen and log in. To check that
the custom build is running, this should print `/opt/cosmic-comp-custom/bin/cosmic-comp`:

```sh
readlink /proc/$(pgrep -x cosmic-comp)/exe
```

What the script installs:

| Path | Purpose |
|---|---|
| `/opt/cosmic-comp-custom/bin/cosmic-comp` | the build |
| `/usr/local/bin/start-cosmic-custom` | wrapper: puts the directory above first on `PATH`, then runs the stock `/usr/bin/start-cosmic` (`cosmic-session` starts `cosmic-comp` from `PATH`) |
| `/usr/share/wayland-sessions/cosmic-custom.desktop` | the login screen entry |

- **Updating:** after `git pull && cargo build --release`, run `scripts/install-session.sh`
  again and log in again. An existing wrapper is kept, so environment variables you added
  to it survive.
- **Removing:** `scripts/install-session.sh uninstall`.

### AMD GPUs: flashing lines

If lines flash across the screen when clicking, which is an upstream amdgpu
overlay-plane issue ([pop-os/cosmic-comp#2152](https://github.com/pop-os/cosmic-comp/issues/2152),
[#1956](https://github.com/pop-os/cosmic-comp/issues/1956)) and not specific to this branch,
add the following above the `PATH` line in `/usr/local/bin/start-cosmic-custom` and log in
again:

```sh
export COSMIC_DISABLE_OVERLAY_SCANOUT=1
```

### Reporting problems with spanning windows

If a window spanning displays stutters or stops updating, a Wayland trace of the affected
app helps most: `WAYLAND_DEBUG=1 <app> 2> trace.log`, then reproduce the problem. It shows
when the compositor asks the app to redraw. Include the compositor's log too, from
`journalctl --user -b -t cosmic-comp`.

## Trying it without logging out

`scripts/nested.sh` runs the compositor in windows inside your current session: three
960x540 displays, two at the bottom and one centred above them, with `cosmic-term` inside.
Each display is its own window on your desktop. Arrange them in the same shape before
dragging windows between them. Change the layout with
`COSMIC_X11_OUTPUTS=WxH[@scale][+X+Y],...`, e.g. `COSMIC_X11_OUTPUTS=1280x720,1280x720`.

## Tests and benchmarks

Everything runs on a headless backend, shows nothing on screen and doesn't touch your
session:

```sh
scripts/test.sh     # integration tests (compositor + Wayland test clients), ~2s
scripts/bench.sh    # A/B benchmark against the `span-baseline` tag, fails on regressions
```

[AGENTS.md](AGENTS.md) documents the test harness, the benchmarks, the design of the
feature and the development conventions in detail.

## Benchmarks

A/B comparison of the branch's final code (`76fc4c00`) against the code the work started
from.

**Baseline:** the starting commit `0fbd457` can't be benchmarked directly, because it has no
headless backend. The baseline is the `span-baseline` tag (`7c86021`): `0fbd457` plus only
the fork's test and benchmark tooling. Its compositor code is identical to `0fbd457` apart
from the added headless backend and nested-X11 testing support, so it measures what users
run.

**Method:** `scripts/bench.sh --rounds 5`. Both builds are release-optimised (`bench-opt`),
and baseline and candidate alternate within each round. Each value is the median over 5
rounds of the median of 240 frames, 20,000 pointer lookups or 2,000 refreshes, measured
inside the compositor. Machine: AMD Ryzen 9 3950X, AMD Radeon RX 580. An A/A run (same
binary on both sides) stays within ±4%.

**What's measured:**
- **Frame:** full redraw of an output, until the GPU finished (`glFinish`).
- **Pointer lookup:** finding the surface under the pointer, done on every pointer event.
- **Refresh:** the compositor's periodic bookkeeping, at most every 150ms.

Sending frame callbacks after each frame isn't benchmarked.

### Without windows spanning displays

| Scenario | Metric | Baseline | Final | Change |
|---|---|---:|---:|---:|
| 1 display, 1 window | frame | 183.0µs | 181.7µs | −0.7% |
| | pointer lookup | 2.60µs | 2.60µs | 0.0% |
| | refresh | 1.38µs | 1.50µs | +0.12µs |
| 2 displays, 10 windows | frame, display with windows | 498.1µs | 493.4µs | −0.9% |
| | frame, empty display | 109.6µs | 109.8µs | +0.2% |
| | pointer lookup | 6.60µs | 6.68µs | +1.2% |
| | refresh | 8.07µs | 9.43µs | +1.36µs |

Rendering and input are within noise. Refresh is up 0.1–1.4µs; it runs at most every
150ms, so that's about 0.001% of a CPU core.

### With windows spanning displays

| Scenario | Metric | Baseline | Final | Change |
|---|---|---:|---:|---:|
| 2 displays, 10 windows, 1 spanning | frame, display the window overhangs | 107.0µs | 176.6µs | +70µs |
| | pointer lookup | 6.55µs | 6.66µs | +0.11µs |
| 2 displays, 5 windows all spanning | frame, display the windows overhang | 117.0µs | 270.4µs | +153µs |
| | pointer lookup | 1.32µs | 2.48µs | +1.2µs |
| 4 displays, 20 windows, 4 spanning | frame, worst display | 305.6µs | 374.3µs | +69µs |
| | pointer lookup | 4.56µs | 7.64µs | +3.1µs |
| | refresh | 16.23µs | 22.93µs | +6.7µs |
| 3 stacked displays, 6 windows, 3 spanning | frame, worst display | 169.3µs | 234.1µs | +65µs |
| 2 displays (1x + 2x scale), 1 spanning | frame, 2x display | 151.0µs | 234.3µs | +83µs |

The extra frame time is the overhanging windows actually being drawn on the second display.
That costs about as much as drawing them on their own display: 5 windows cost +153µs on the
display they overhang, against ~155µs on their home display. The slowest frame measured,
374µs, is 2.2% of a 60Hz frame (16.7ms).

The pointer lookups' extra time is partly real hit-testing: in the baseline, the points
over the overhang hit empty desktop. At most it's +3.1µs per pointer event, which is about
0.3% of a CPU core at 1000Hz.

All results are within the budgets in `test-harness/src/bin/bench.rs`:
- **Without spanning windows:** no more than +10–15% and a few µs over the baseline.
- **With spanning windows:** +300µs per frame, +5µs per lookup and +15µs per refresh.

## License

GPL-3.0-only, like upstream cosmic-comp.
