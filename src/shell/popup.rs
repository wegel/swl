// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    desktop::{get_popup_toplevel_coords, PopupKind},
    utils::{Point, Rectangle, Size},
    wayland::{
        seat::WaylandFocus,
        shell::xdg::PopupSurface,
    },
};

use super::Shell;
use crate::utils::coordinates::{GlobalPoint, GlobalRect};

impl Shell {
    /// Adjusts popup position to ensure it fits within screen boundaries
    pub fn unconstrain_popup(&self, surface: &PopupSurface) {
        tracing::debug!("Shell::unconstrain_popup called");
        
        // get the toplevel parent surface
        let parent = surface.get_parent_surface();
        if parent.is_none() {
            tracing::warn!("Popup has no parent surface");
            return;
        }
        let parent = parent.unwrap();
        tracing::debug!("Popup parent surface found");
        
        // find the window that contains this parent surface
        let window = self.space.elements().find(|w| {
            w.wl_surface().as_ref().map(|s| s.as_ref()) == Some(&parent)
        });
        
        if let Some(window) = window {
            // get window location in the space
            let window_loc = GlobalPoint(self.space.element_location(window)
                .unwrap_or_else(|| Point::from((0, 0))));
            let window_geo = window.geometry();
            
            // get the output containing the window
            let output = self.visible_output_for_surface(&parent);
            if let Some(output) = output {
                // get output geometry from the space
                let output_rect = self.space.output_geometry(output)
                    .map(GlobalRect)
                    .unwrap_or_else(|| GlobalRect::new(GlobalPoint::new(0, 0), Size::from((1920, 1080))));
                
                // Calculate relative rectangle (output bounds relative to window)
                let relative_rect = Rectangle::new(
                    Point::from((output_rect.0.loc.x - window_loc.0.x - window_geo.loc.x, 
                                 output_rect.0.loc.y - window_loc.0.y - window_geo.loc.y)),
                    output_rect.0.size
                );
                
                // Adjust for popup chain positioning
                let popup_offset = get_popup_toplevel_coords(&PopupKind::Xdg(surface.clone()));
                let mut adjusted_rect = relative_rect;
                adjusted_rect.loc -= popup_offset;
                
                // Get unconstrained geometry from the positioner
                let geometry = surface.with_pending_state(|state| {
                    state.positioner.get_unconstrained_geometry(adjusted_rect)
                });
                
                // Update the popup's geometry
                surface.with_pending_state(|state| {
                    state.geometry = geometry;
                });
                
                tracing::debug!(
                    "Unconstrained popup to geometry: {:?} within bounds: {:?}", 
                    geometry, adjusted_rect
                );
            }
        } else {
            tracing::warn!("Could not find window for popup parent surface");
        }
    }
}