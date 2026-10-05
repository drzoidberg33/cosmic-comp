// SPDX-License-Identifier: GPL-3.0-only

//! Fork-only: `WxH[@scale][+X+Y]` output specs for the headless backend, so tests can use
//! arbitrary output arrangements.

use crate::{state::State, utils::prelude::*};
use anyhow::{Context, Result, bail};
use smithay::output::Output;

/// One `WxH[@scale][+X+Y]` entry of an output list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputSpec {
    pub size: (i32, i32),
    pub scale: f64,
    /// Global logical position. Outputs without one are laid out left to right.
    pub position: Option<(u32, u32)>,
}

/// Parses a comma separated list of `WxH[@scale][+X+Y]` entries.
pub fn parse_output_specs(value: &str) -> Result<Vec<OutputSpec>> {
    value
        .split(',')
        .map(|entry| {
            let entry = entry.trim();
            let (mode, position) = match entry.split_once('+') {
                Some((mode, position)) => {
                    let (x, y) = position
                        .split_once('+')
                        .with_context(|| format!("Expected `+X+Y` in `{entry}`"))?;
                    let parse = |v: &str| {
                        v.parse::<u32>()
                            .with_context(|| format!("Invalid position in `{entry}`"))
                    };
                    (mode, Some((parse(x)?, parse(y)?)))
                }
                None => (entry, None),
            };
            let (size, scale) = match mode.split_once('@') {
                Some((size, scale)) => (
                    size,
                    scale
                        .parse::<f64>()
                        .with_context(|| format!("Invalid scale in `{entry}`"))?,
                ),
                None => (mode, 1.0),
            };
            let (w, h) = size
                .split_once('x')
                .with_context(|| format!("Expected `WxH[@scale][+X+Y]`, got `{entry}`"))?;
            let size = (
                w.parse::<i32>()
                    .with_context(|| format!("Invalid width in `{entry}`"))?,
                h.parse::<i32>()
                    .with_context(|| format!("Invalid height in `{entry}`"))?,
            );
            if size.0 <= 0 || size.1 <= 0 || scale <= 0.0 {
                bail!("Output size and scale must be positive in `{entry}`");
            }
            Ok(OutputSpec {
                size,
                scale,
                position,
            })
        })
        .collect()
}

/// Moves outputs to their requested positions. Without a stored config `read_outputs` lays
/// outputs out left to right, so this runs afterwards, through the same path as an output
/// configuration change.
pub fn apply_positions(
    state: &mut State,
    positions: &[(Output, Option<(u32, u32)>)],
) -> Result<()> {
    if positions.iter().all(|(_, position)| position.is_none()) {
        return Ok(());
    }
    for (output, position) in positions {
        if let Some(position) = position {
            output.config_mut().position = *position;
        }
    }
    let common = &mut state.common;
    state
        .backend
        .lock()
        .apply_config_for_outputs(
            false,
            &common.event_loop_handle,
            common.config.dynamic_conf.screen_filter(),
            common.shell.clone(),
            &mut common.workspace_state.update(),
            &common.xdg_activation_state,
            common.startup_done.clone(),
            &common.clock,
        )
        .context("Failed to apply output positions")?;
    common.output_configuration_state.update();
    Ok(())
}

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
                    scale: 1.0,
                    position: None,
                },
                OutputSpec {
                    size: (1920, 1080),
                    scale: 1.5,
                    position: None,
                },
            ]
        );
        assert_eq!(
            parse_output_specs("1920x1080+0+1080,2560x1440@2+960+0").unwrap(),
            vec![
                OutputSpec {
                    size: (1920, 1080),
                    scale: 1.0,
                    position: Some((0, 1080)),
                },
                OutputSpec {
                    size: (2560, 1440),
                    scale: 2.0,
                    position: Some((960, 0)),
                },
            ]
        );
        assert!(parse_output_specs("1280x720+10").is_err());
        assert!(parse_output_specs("1280x720+-10+0").is_err());
        assert!(parse_output_specs("1280").is_err());
        assert!(parse_output_specs("0x720").is_err());
        assert!(parse_output_specs("1280x720@0").is_err());
    }
}
