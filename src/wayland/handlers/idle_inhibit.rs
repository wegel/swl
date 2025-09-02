// SPDX-License-Identifier: GPL-3.0-only

use crate::state::State;
use smithay::{
    delegate_idle_inhibit,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::idle_inhibit::IdleInhibitHandler,
};
use std::collections::HashSet;

/// Tracks surfaces that are inhibiting idle
pub struct IdleInhibitState {
    inhibiting_surfaces: HashSet<WlSurface>,
}

impl IdleInhibitState {
    pub fn new() -> Self {
        Self {
            inhibiting_surfaces: HashSet::new(),
        }
    }
    
    pub fn is_inhibited(&self) -> bool {
        !self.inhibiting_surfaces.is_empty()
    }
}

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        tracing::debug!("Surface {:?} requesting idle inhibit", surface.id());
        self.idle_inhibit_state.inhibiting_surfaces.insert(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        tracing::debug!("Surface {:?} releasing idle inhibit", surface.id());
        self.idle_inhibit_state.inhibiting_surfaces.remove(&surface);
    }
}

delegate_idle_inhibit!(State);