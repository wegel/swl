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
        // Try to find which output this surface is on
        let scale = {
            // First check if this surface belongs to a window that's already mapped
            let shell = self.shell.read().unwrap();
            let maybe_output = shell.visible_output_for_surface(&surface);
            
            if let Some(output) = maybe_output {
                // Use the scale of the output the surface is on
                output.current_scale().fractional_scale()
            } else {
                // Fallback to first output or 1.0
                self.outputs.first()
                    .map(|output| output.current_scale().fractional_scale())
                    .unwrap_or(1.0)
            }
        };
        
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional_scale| {
                fractional_scale.set_preferred_scale(scale);
            });
        });
    }
}

delegate_fractional_scale!(State);