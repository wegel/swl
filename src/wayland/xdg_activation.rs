// SPDX-License-Identifier: GPL-3.0-only

use crate::State;
use smithay::{
    delegate_xdg_activation,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::xdg_activation::{
        XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
    },
};

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, _data: XdgActivationTokenData) -> bool {
        // For now, always allow token creation
        // In the future, we might want to check if the client is privileged
        true
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // For now, just log the request
        // In the future, we would handle urgent window activation here
        tracing::debug!("XDG activation requested for surface: {:?}", surface);
    }
}

delegate_xdg_activation!(State);
