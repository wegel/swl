// SPDX-License-Identifier: GPL-3.0-only

pub mod handlers;

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{Window, utils::send_frames_surface_tree, space::SpaceElement},
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Clock, Monotonic},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::OutputHandler,
        selection::{
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
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
        // first load the buffer for various smithay helper functions (which also initializes the RendererSurfaceState)
        on_commit_buffer_handler::<Self>(surface);
        
        // check if this is a pending window that should be mapped
        let mut mapped = false;
        if let Some(index) = self.pending_windows.iter().position(|(toplevel, _)| {
            toplevel.wl_surface() == surface
        }) {
            // check if surface now has a buffer
            if with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false) {
                let (toplevel, window) = self.pending_windows.remove(index);
                
                // the window is ready to be mapped - call on_commit to update geometry
                window.on_commit();
                window.refresh();
                
                if let Some(output) = self.outputs.first() {
                    tracing::info!("Mapping pending window to output {} (geometry: {:?})", 
                                  output.name(), window.geometry());
                    self.shell.write().unwrap().add_window(window.clone(), output);
                    
                    // send initial frame callback
                    let clock = Clock::<Monotonic>::new();
                    send_frames_surface_tree(surface, output, clock.now(), None, |_, _| None);
                    
                    self.backend.schedule_render(output);
                    mapped = true;
                } else {
                    tracing::warn!("No outputs available for window mapping");
                    // put it back in pending
                    self.pending_windows.push((toplevel, window));
                }
            } else {
                tracing::debug!("Pending window surface committed but no buffer yet");
            }
        }
        
        if !mapped {
            // handle regular window surface commits  
            let output = {
                let mut shell = self.shell.write().unwrap();
                let output = shell.visible_output_for_surface(surface).cloned();
                
                if let Some(window) = shell.space.elements().find(|w| {
                    w.toplevel().unwrap().wl_surface() == surface
                }) {
                    window.on_commit();
                    tracing::debug!("Window surface commit handled");
                    
                    // send frame callback to let client know it can render the next frame
                    if let Some(ref output) = output {
                        let clock = Clock::<Monotonic>::new();
                        send_frames_surface_tree(surface, output, clock.now(), None, |_, _| None);
                        tracing::debug!("Sent frame callback to window surface");
                    }
                }
                
                // refresh the space to update damage tracking
                shell.refresh();
                
                output
            };
            
            // schedule render on the output showing this surface
            if let Some(output) = output {
                tracing::debug!("Scheduling render for output {} after surface commit", output.name());
                self.backend.schedule_render(&output);
            } else {
                tracing::debug!("No output found for committed surface");
            }
        }
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

impl OutputHandler for State {}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}
impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        tracing::debug!("xdg_shell_state accessed");
        &mut self.xdg_shell_state
    }
    
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::info!("New toplevel surface requested");
        let window = Window::new_wayland_window(surface.clone());
        
        // send initial configure with size and activated state
        // tell the window it's activated and suggest a size
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
            state.size = Some((800, 600).into());
        });
        surface.send_configure();
        tracing::debug!("Sent initial configure to toplevel (800x600, activated)");
        
        // store as pending window - will be mapped after first commit with buffer
        self.pending_windows.push((surface, window));
        tracing::info!("Window added to pending list, waiting for initial commit with buffer");
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
delegate_data_device!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_xdg_shell!(State);

// we already implement SeatHandler in input/mod.rs