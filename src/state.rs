// SPDX-License-Identifier: GPL-3.0-only

use smithay::reexports::{
    calloop::{LoopHandle, LoopSignal},
    wayland_server::{Display, DisplayHandle},
};

/// The main compositor state
pub struct State {
    pub display_handle: DisplayHandle,
    #[allow(dead_code)] // will be used in Phase 2
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub should_stop: bool,
    pub socket_name: String,
}

// suppress warnings for now - we'll use these soon
#[allow(dead_code)]
impl State {
    pub fn socket_name(&self) -> &str {
        &self.socket_name
    }
}

impl State {
    pub fn new(
        display: &Display<State>,
        socket_name: String,
        loop_handle: LoopHandle<'static, State>,
        loop_signal: LoopSignal,
    ) -> Self {
        let display_handle = display.handle();
        
        Self {
            display_handle,
            loop_handle,
            loop_signal,
            should_stop: false,
            socket_name,
        }
    }
}