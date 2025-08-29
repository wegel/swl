// SPDX-License-Identifier: GPL-3.0-only

pub mod handlers;
pub mod layer_shell;

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    delegate_compositor, delegate_data_device, delegate_output, delegate_presentation, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_decoration,
    desktop::{Window, WindowSurfaceType, utils::send_frames_surface_tree, space::SpaceElement},
    output::Output,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
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
            decoration::XdgDecorationHandler,
            PopupSurface, PositionerState, ToplevelSurface,
            XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
    reexports::wayland_server::{
        protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
        Client,
    },
    utils::Serial,
};

use crate::State;
use self::handlers::ClientState;
use tracing::debug;

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
        
        // check if this is a layer surface commit
        let outputs = self.outputs.clone();
        for output in &outputs {
            let layer_map = smithay::desktop::layer_map_for_output(output);
            if let Some(layer_surface) = layer_map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                // layer surface committed, trigger render
                layer_surface.cached_state();
                
                // check if it wants keyboard focus
                if layer_surface.can_receive_keyboard_focus() {
                    tracing::debug!("Layer surface requests keyboard focus");
                    let keyboard = self.seat.get_keyboard().unwrap();
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    keyboard.set_focus(self, Some(surface.clone()), serial);
                }
                
                // send frame callback so the layer surface knows it can render again
                let clock = Clock::<Monotonic>::new();
                send_frames_surface_tree(surface, output, clock.now(), None, |_, _| None);
                
                self.backend.schedule_render(output);
                tracing::debug!("Layer surface committed, scheduling render for output {}", output.name());
                return; // handled as layer surface
            }
        }
        
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
                
                if let Some(output) = self.outputs.first().cloned() {
                    tracing::info!("Mapping pending window to output {} (geometry: {:?})", 
                                  output.name(), window.geometry());
                    
                    // check if window should be fullscreen
                    let is_fullscreen = toplevel.with_pending_state(|state| {
                        state.states.contains(xdg_toplevel::State::Fullscreen)
                    });
                    
                    let mut shell = self.shell.write().unwrap();
                    shell.add_window(window.clone(), &output);
                    
                    if is_fullscreen {
                        tracing::debug!("Window is fullscreen, updating shell state");
                        shell.set_fullscreen(window.clone(), true);
                    }
                    drop(shell); // release lock before setting keyboard focus
                    
                    // set keyboard focus to the new window
                    let keyboard = self.seat.get_keyboard().unwrap();
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    keyboard.set_focus(self, Some(toplevel.wl_surface().clone()), serial);
                    tracing::debug!("Set keyboard focus to new window");
                    
                    // send initial frame callback
                    let clock = Clock::<Monotonic>::new();
                    send_frames_surface_tree(surface, &output, clock.now(), None, |_, _| None);
                    
                    self.backend.schedule_render(&output);
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
        
        // check if fullscreen was already requested (e.g., foot -F)
        let is_fullscreen = surface.with_pending_state(|state| {
            state.states.contains(xdg_toplevel::State::Fullscreen)
        });
        
        if is_fullscreen {
            tracing::debug!("Window requested fullscreen before mapping");
            // fullscreen state already set by fullscreen_request
            // just need to set activated
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Activated);
                // size should already be set by fullscreen_request
            });
        } else {
            // normal window - send initial configure with size and activated state
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Activated);
                state.size = Some((800, 600).into());
            });
        }
        
        surface.send_configure();
        tracing::debug!("Sent initial configure to toplevel (fullscreen: {})", is_fullscreen);
        
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
    
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // find and remove the window from our shell
        let (output, was_focused) = {
            let mut shell = self.shell.write().unwrap();
            
            // find which output the window is on
            let output = self.outputs.first().cloned();
            
            let mut was_focused = false;
            
            // remove from space
            if let Some(window) = shell.windows.values().find(|w| {
                w.toplevel().map_or(false, |t| t == &surface)
            }).cloned() {
                shell.space.unmap_elem(&window);
                
                // remove from our tracking
                shell.windows.retain(|_id, w| {
                    w.toplevel().map_or(true, |t| t != &surface)
                });
                
                // remove from focus stack
                shell.focus_stack.retain(|w| {
                    w.toplevel().map_or(true, |t| t != &surface)
                });
                
                // remove from floating windows
                shell.floating_windows.retain(|w| {
                    w.toplevel().map_or(true, |t| t != &surface)
                });
                
                // check if focused window was destroyed
                was_focused = shell.focused_window.as_ref().map_or(false, |w| {
                    w.toplevel().map_or(false, |t| t == &surface)
                });
                
                // update fullscreen window if it was destroyed
                if shell.fullscreen_window.as_ref().map_or(false, |w| {
                    w.toplevel().map_or(false, |t| t == &surface)
                }) {
                    shell.fullscreen_window = None;
                    shell.fullscreen_restore = None;
                }
                
                // re-arrange remaining windows
                shell.arrange();
            }
            
            (output, was_focused)
        };
        
        // if the destroyed window was focused, mark for focus refresh
        if was_focused {
            self.needs_focus_refresh = true;
            tracing::debug!("Focused window destroyed, marked for focus refresh");
        }
        
        // schedule render for the output
        if let Some(output) = output {
            self.backend.schedule_render(&output);
        }
    }
    
    fn fullscreen_request(&mut self, surface: ToplevelSurface, wl_output: Option<WlOutput>) {
        // handle fullscreen state change - fullscreen_request always means go fullscreen
        debug!("fullscreen_request called with output: {:?}", wl_output.is_some());
        let mut shell = self.shell.write().unwrap();
        
        // find output first - we'll need it either way
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| {
                // fallback to the output containing this surface or just the first one
                shell.visible_output_for_surface(surface.wl_surface()).cloned()
            })
            .or_else(|| shell.space.outputs().next().cloned());
        
        if let Some(output) = output {
            debug!("Will set fullscreen on output: {}", output.name());
            
            // always configure the surface state for fullscreen, even if window not yet mapped
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.fullscreen_output = wl_output;
                // set fullscreen size to output size
                let mode = output.current_mode().unwrap();
                // convert physical size to logical
                let scale = output.current_scale().fractional_scale();
                let logical_size = mode.size.to_f64().to_logical(scale).to_i32_round();
                debug!("Fullscreen size will be: {:?}", logical_size);
                state.size = Some(logical_size);
            });
            surface.send_configure();
            
            // now try to find the window to update shell state
            debug!("Searching for window among {} elements", shell.space.elements().count());
            let window = shell.space.elements().find(|w| {
                w.toplevel().unwrap() == &surface
            }).cloned();
            
            if let Some(window) = window {
                debug!("Found window, updating shell fullscreen state");
                shell.set_fullscreen(window, true);
            } else {
                debug!("Window not yet mapped - fullscreen state will be applied when window is created");
                // the window will pick up the fullscreen state when it's created
            }
        } else {
            debug!("No output found for fullscreen request");
        }
    }
    
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        // handle unfullscreen
        let mut shell = self.shell.write().unwrap();
        
        // find the window - clone to avoid borrow issues
        let window = shell.space.elements().find(|w| {
            w.toplevel().unwrap() == &surface
        }).cloned();
        
        if let Some(window) = window {
            // get the restore geometry before clearing fullscreen state
            let restore_size = shell.take_fullscreen_restore()
                .map(|rect| rect.size)
                .unwrap_or_else(|| (800, 600).into());
            
            surface.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.fullscreen_output = None;
                state.size = Some(restore_size);
            });
            surface.send_configure();
            
            shell.set_fullscreen(window, false);
        }
    }
    
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // we'll handle popup grabs later
    }
    
    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {
        // we'll handle repositioning later
    }
}

// delegate protocol handling to smithay
impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // always use server-side decorations (no client decorations)
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }
    
    fn request_mode(&mut self, _toplevel: ToplevelSurface, _mode: Mode) {
        // ignore client requests - we control decoration mode
    }
    
    fn unset_mode(&mut self, _toplevel: ToplevelSurface) {
        // ignore unset requests
    }
}

delegate_compositor!(State);
delegate_xdg_decoration!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_xdg_shell!(State);
delegate_presentation!(State);

// we already implement SeatHandler in input/mod.rs