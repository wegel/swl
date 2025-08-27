// SPDX-License-Identifier: GPL-3.0-only

pub mod kms;

use crate::state::State;
use anyhow::Result;
use smithay::reexports::{
    calloop::EventLoop,
    wayland_server::DisplayHandle,
};

/// Initialize the backend based on environment
pub fn init_backend(
    dh: &DisplayHandle,
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
) -> Result<()> {
    // for now, we only support KMS backend
    kms::init_backend(dh, event_loop, state)
}