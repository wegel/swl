// SPDX-License-Identifier: GPL-3.0-only

pub mod handlers;

use smithay::{
    delegate_compositor, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::Window,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface,
            XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
    reexports::wayland_server::{
        protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
        Client,
    },
    utils::Serial,
};

use crate::State;
use self::handlers::ClientState;

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }
    
    fn commit(&mut self, surface: &WlSurface) {
        // handle window surface commits  
        if let Some(window) = self.shell.space.elements().find(|w| {
            w.toplevel().unwrap().wl_surface() == surface
        }) {
            window.on_commit();
            tracing::debug!("Window surface commit handled");
        }
        
        // refresh the space to update damage tracking
        self.shell.refresh();
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface.clone());
        
        // send initial configure
        surface.send_configure();
        
        // add the window to our shell
        // for now, map to the first available output
        if let Some(output) = self.outputs.first() {
            tracing::info!("New window created, adding to output {}", output.name());
            self.shell.add_window(window, output);
        } else {
            tracing::warn!("No outputs available for new window");
        }
    }
    
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        // we'll handle popups later
    }
    
    fn move_request(&mut self, _surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        // we'll handle move requests later
    }
    
    fn resize_request(&mut self, _surface: ToplevelSurface, _seat: WlSeat, _serial: Serial, _edges: xdg_toplevel::ResizeEdge) {
        // we'll handle resize requests later
    }
    
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // we'll handle popup grabs later
    }
    
    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {
        // we'll handle repositioning later
    }
}

// delegate protocol handling to smithay
delegate_compositor!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_xdg_shell!(State);

// we already implement SeatHandler in input/mod.rs