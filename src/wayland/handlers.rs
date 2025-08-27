// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    reexports::wayland_server::backend::ClientData,
    wayland::compositor::CompositorClientState,
};

/// Client data stored for each connected client
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: smithay::reexports::wayland_server::backend::ClientId) {}
    fn disconnected(&self, _client_id: smithay::reexports::wayland_server::backend::ClientId, _reason: smithay::reexports::wayland_server::backend::DisconnectReason) {}
}

impl ClientState {
    pub fn new() -> Self {
        Self {
            compositor_state: CompositorClientState::default(),
        }
    }
}