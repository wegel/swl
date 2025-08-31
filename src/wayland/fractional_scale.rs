// SPDX-License-Identifier: GPL-3.0-only

use crate::State;
use smithay::{
    delegate_fractional_scale,
    wayland::{
        fractional_scale::{with_fractional_scale, FractionalScaleHandler},
        compositor::with_states,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
};

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Set initial fractional scale based on the output the surface is on
        // For now, we'll use the scale of the first output or 1.0 as fallback
        let scale = self.outputs.first()
            .map(|output| output.current_scale().fractional_scale())
            .unwrap_or(1.0);
        
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional_scale| {
                fractional_scale.set_preferred_scale(scale);
            });
        });
    }
}

delegate_fractional_scale!(State);