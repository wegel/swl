// SPDX-License-Identifier: GPL-3.0-only

pub mod data_control;
pub mod fractional_scale;
pub mod handlers;
pub mod layer_shell;
pub mod output_configuration;
pub mod primary_selection;
pub mod xdg_activation;

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_output,
    delegate_pointer_gestures, delegate_presentation, delegate_relative_pointer, delegate_seat,
    delegate_shm, delegate_text_input_manager, delegate_viewporter, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, space::SpaceElement, PopupKeyboardGrab, PopupKind,
        PopupPointerGrab, PopupUngrabStrategy, Window, WindowSurfaceType,
    },
    input::{pointer::Focus, Seat},
    output::Output,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    reexports::wayland_server::{
        protocol::{
            wl_buffer::WlBuffer, wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface,
        },
        Client,
    },
    utils::Serial,
    utils::Size,
    wayland::{
        buffer::BufferHandler,
        compositor::{get_parent, CompositorClientState, CompositorHandler, CompositorState},
        output::OutputHandler,
        selection::{
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::xdg::{
            decoration::XdgDecorationHandler, PopupSurface, PositionerState, ToplevelSurface,
            XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};

use self::handlers::ClientState;
use crate::State;
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
            if let Some(layer_surface) =
                layer_map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            {
                // layer surface committed, trigger render
                layer_surface.cached_state();

                // check if it wants keyboard focus
                let wants_focus = layer_surface.can_receive_keyboard_focus();

                // drop the immutable borrow before we get a mutable one
                drop(layer_map);

                // re-arrange layers as the surface may have changed size
                tracing::trace!(
                    "Layer surface committing on output {}, re-arranging",
                    output.name()
                );
                let changed = {
                    let mut layer_map = smithay::desktop::layer_map_for_output(output);
                    layer_map.arrange()
                }; // layer_map dropped here, mutex released

                // Debug: check geometry after commit arrange
                if let Some(layer_surface) = {
                    let layer_map = smithay::desktop::layer_map_for_output(output);
                    layer_map
                        .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                        .cloned()
                } {
                    let layer_map = smithay::desktop::layer_map_for_output(output);
                    let geometry = layer_map.layer_geometry(&layer_surface);
                    tracing::debug!(
                        "Layer surface geometry after commit arrange: {:?}",
                        geometry
                    );
                }

                if changed {
                    tracing::trace!(
                        "Layer arrangement changed after commit on output {}",
                        output.name()
                    );
                    // mark that windows need to be re-arranged
                    let mut shell = self.shell.write().unwrap();
                    shell.apply_to_all_workspaces_on_output(output, |workspace| {
                        workspace.needs_arrange = true;
                        tracing::debug!("Marked workspace {} for re-arrangement", workspace.name);
                    });
                    drop(shell);
                    self.backend.schedule_render(output);
                } else {
                    tracing::trace!("Layer surface committed but no arrangement change needed");
                }

                if wants_focus {
                    //tracing::debug!("Layer surface requests keyboard focus");
                    let keyboard = self.seat.get_keyboard().unwrap();
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    keyboard.set_focus(self, Some(surface.clone()), serial);
                }

                // Don't send frame callbacks here - let the rendering pipeline handle it
                // This prevents double callbacks and timing issues

                self.backend.schedule_render(output);
                //tracing::debug!("Layer surface committed, scheduling render for output {}", output.name());
                return; // handled as layer surface
            }
        }

        // check if this is a pending window that should be mapped
        let mut mapped = false;
        if let Some(index) = self
            .pending_windows
            .iter()
            .position(|(toplevel, _)| toplevel.wl_surface() == surface)
        {
            // check if surface now has a buffer
            if with_renderer_surface_state(surface, |state| state.buffer().is_some())
                .unwrap_or(false)
            {
                let (toplevel, window) = self.pending_windows.remove(index);

                // the window is ready to be mapped - call on_commit to update geometry
                window.on_commit();
                window.refresh();

                if let Some(output) = self.outputs.first().cloned() {
                    // Get app_id and title for debugging
                    use smithay::wayland::compositor::with_states;
                    use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

                    let (app_id, title) = with_states(toplevel.wl_surface(), |states| {
                        if let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() {
                            let data = data.lock().unwrap();
                            (data.app_id.clone(), data.title.clone())
                        } else {
                            (None, None)
                        }
                    });
                    let geometry = window.geometry();

                    tracing::info!(
                        "Mapping window with first buffer - app_id: {:?}, title: {:?}, geometry: {:?}",
                        app_id, title, geometry
                    );

                    // check if window should be fullscreen
                    let is_fullscreen = toplevel.with_pending_state(|state| {
                        state.states.contains(xdg_toplevel::State::Fullscreen)
                    });

                    // Get cursor position to determine correct virtual output
                    let cursor_pos = self.seat.get_pointer().unwrap().current_location();

                    let mut shell = self.shell.write().unwrap();

                    // Find virtual output containing cursor position
                    if let Some(virtual_output_id) = shell.virtual_output_at_point(cursor_pos) {
                        tracing::debug!(
                            "Adding window to virtual output {:?} at cursor position {:?}",
                            virtual_output_id,
                            cursor_pos
                        );
                        shell.add_window_to_virtual_output(window.clone(), virtual_output_id);
                    } else {
                        // Fallback to legacy method if cursor not over any virtual output
                        tracing::debug!("Cursor not over any virtual output, using fallback");
                        shell.add_window(window.clone(), &output);
                    }

                    if is_fullscreen {
                        tracing::debug!("Window is fullscreen, updating shell state");
                        shell.set_fullscreen(window.clone(), true, &output);
                    }
                    drop(shell); // release lock before setting keyboard focus

                    // set keyboard focus to the new window
                    let keyboard = self.seat.get_keyboard().unwrap();
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    keyboard.set_focus(self, Some(toplevel.wl_surface().clone()), serial);
                    //tracing::debug!("Set keyboard focus to new window");

                    // Don't send frame callbacks here - let the rendering pipeline handle it
                    // The render scheduled below will trigger proper frame callbacks

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
                // First try to find output for this surface directly
                let mut output = shell.visible_output_for_surface(surface).cloned();

                // If not found, this might be a subsurface - check parent surfaces
                if output.is_none() {
                    if let Some(parent) = get_parent(surface) {
                        output = shell.visible_output_for_surface(&parent).cloned();
                        if output.is_some() {
                            tracing::trace!("Found output for subsurface via parent");
                        }
                    }
                }

                let geometry_changed = if let Some(window) = shell
                    .space
                    .elements()
                    .find(|w| w.toplevel().unwrap().wl_surface() == surface)
                {
                    // Store old geometry to check if it changed
                    let old_geom = window.geometry();

                    window.on_commit();

                    // Check if geometry changed (e.g. CSD shadows became available)
                    let new_geom = window.geometry();
                    let changed = old_geom != new_geom;

                    // Don't send frame callbacks here - they'll be sent by the rendering pipeline
                    // after the scheduled render completes

                    changed
                } else {
                    false
                };

                // Mark for re-arrange if geometry changed
                if geometry_changed {
                    if let Some(ref output) = output {
                        shell.apply_to_all_workspaces_on_output(output, |workspace| {
                            workspace.needs_arrange = true;
                        });
                    }
                }

                // refresh the space to update damage tracking
                shell.refresh();

                output
            };

            // schedule render on the output showing this surface
            if let Some(output) = output {
                // Always schedule renders for all commits
                // This ensures Firefox and other applications that use subsurfaces render correctly
                // This results in ~60fps with Firefox, ~2fps with terminals
                self.backend.schedule_render(&output);
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
        let window = Window::new_wayland_window(surface.clone());

        // Log window properties to understand temporary windows
        let parent = surface.parent();

        // check if fullscreen was already requested (e.g., foot -F)
        let is_fullscreen = surface
            .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Fullscreen));

        // Get app_id and title if already set
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let (app_id, title) = with_states(surface.wl_surface(), |states| {
            if let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() {
                let data = data.lock().unwrap();
                (data.app_id.clone(), data.title.clone())
            } else {
                (None, None)
            }
        });

        tracing::info!(
            "New toplevel window - has_parent: {}, fullscreen: {}, app_id: {:?}, title: {:?}",
            parent.is_some(),
            is_fullscreen,
            app_id,
            title
        );

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
            // Use scale-aware initial size based on the first output
            let initial_size = if let Some(output) = self.outputs.first() {
                let scale = output.current_scale().fractional_scale();
                // Use a reasonable default that accounts for scale
                // This ensures the window fits on high-DPI displays
                let base_size = Size::from((800, 600));
                // For high DPI, we might want smaller logical sizes
                if scale > 1.5 {
                    Size::from((640, 480))
                } else {
                    base_size
                }
            } else {
                Size::from((800, 600))
            };

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Activated);
                state.size = Some(initial_size);
            });
        }

        surface.send_configure();
        tracing::debug!(
            "Sent initial configure to toplevel (fullscreen: {})",
            is_fullscreen
        );

        // store as pending window - will be mapped after first commit with buffer
        self.pending_windows.push((surface, window));
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        tracing::info!(
            "New popup surface - parent: {:?}, geometry: {:?}",
            surface.get_parent_surface().is_some(),
            positioner.get_geometry()
        );

        // Configure the popup with the requested geometry
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });

        // Send the configure event to acknowledge the popup
        if let Err(err) = surface.send_configure() {
            tracing::warn!("Failed to configure popup: {:?}", err);
        } else {
            // Track the popup for proper rendering and input handling
            if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
                tracing::warn!("Failed to track popup: {:?}", err);
            }
        }
    }

    fn move_request(&mut self, _surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        // we'll handle move requests later
    }

    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
        // we'll handle resize requests later
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Log destruction to understand window lifetime
        tracing::info!("Toplevel destroyed");

        // find and remove the window from our shell
        let (outputs, was_focused) = {
            let mut shell = self.shell.write().unwrap();

            let mut was_focused = false;
            let mut found_outputs = Vec::new();

            // Find the window in any workspace
            let window_to_remove = shell
                .space
                .elements()
                .find(|w| w.toplevel().map_or(false, |t| t == &surface))
                .cloned();

            if let Some(window) = window_to_remove {
                // check if focused window was destroyed
                was_focused = shell.focused_window.as_ref() == Some(&window);

                // Remove from all workspaces and get the outputs it was on
                found_outputs = shell.remove_window(&window);
            }

            (found_outputs, was_focused)
        };

        // if the destroyed window was focused, clear keyboard focus and mark for refresh
        if was_focused {
            // clear keyboard focus immediately to ensure refresh_focus works properly
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(
                self,
                Option::<WlSurface>::None,
                smithay::utils::SERIAL_COUNTER.next_serial(),
            );

            self.needs_focus_refresh = true;
        }

        // schedule render for all affected outputs
        for output in outputs {
            self.backend.schedule_render(&output);
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, wl_output: Option<WlOutput>) {
        // handle fullscreen state change - fullscreen_request always means go fullscreen
        debug!(
            "fullscreen_request called with output: {:?}",
            wl_output.is_some()
        );
        let mut shell = self.shell.write().unwrap();

        // find output first - we'll need it either way
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| {
                // fallback to the output containing this surface or just the first one
                shell
                    .visible_output_for_surface(surface.wl_surface())
                    .cloned()
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
                let mut logical_size = mode.size.to_f64().to_logical(scale).to_i32_round();

                // account for rotation - swap dimensions if rotated 90 or 270 degrees
                let transform = output.current_transform();
                use smithay::utils::Transform;
                match transform {
                    Transform::_90
                    | Transform::_270
                    | Transform::Flipped90
                    | Transform::Flipped270 => {
                        logical_size = Size::from((logical_size.h, logical_size.w));
                    }
                    _ => {}
                }

                debug!(
                    "Fullscreen size will be: {:?} (transform: {:?})",
                    logical_size, transform
                );
                state.size = Some(logical_size);
            });
            surface.send_configure();

            // now try to find the window to update shell state
            debug!(
                "Searching for window among {} elements",
                shell.space.elements().count()
            );
            let window = shell
                .space
                .elements()
                .find(|w| w.toplevel().unwrap() == &surface)
                .cloned();

            if let Some(window) = window {
                debug!("Found window, updating shell fullscreen state");
                // Get output for the window
                if let Some(output) = self.outputs.first() {
                    shell.set_fullscreen(window, true, output);
                }
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
        let window = shell
            .space
            .elements()
            .find(|w| w.toplevel().unwrap() == &surface)
            .cloned();

        if let Some(window) = window {
            // use scale-aware restore size
            let restore_size = if let Some(output) = self.outputs.first() {
                let scale = output.current_scale().fractional_scale();
                if scale > 1.5 {
                    Size::from((640, 480))
                } else {
                    Size::from((800, 600))
                }
            } else {
                Size::from((800, 600))
            };

            surface.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.fullscreen_output = None;
                state.size = Some(restore_size);
            });
            surface.send_configure();

            // Get output for the window
            if let Some(output) = self.outputs.first() {
                shell.set_fullscreen(window, false, output);
            }
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);

        // Find the root surface for this popup
        let maybe_root = find_popup_root_surface(&kind).ok();
        if maybe_root.is_none() {
            tracing::warn!("No root surface found for popup grab");
            return;
        }

        // For our compositor, we use WlSurface as the KeyboardFocus type
        // So we pass the root surface directly
        let root_surface = maybe_root.unwrap();

        // Create the popup grab
        let ret = self
            .popups
            .grab_popup(root_surface.clone(), kind, &seat, serial);

        match ret {
            Ok(mut grab) => {
                // Set keyboard grab
                if let Some(keyboard) = seat.get_keyboard() {
                    if keyboard.is_grabbed()
                        && !(keyboard.has_grab(serial)
                            || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
                }

                // Set pointer grab
                if let Some(pointer) = seat.get_pointer() {
                    if pointer.is_grabbed()
                        && !(pointer.has_grab(serial)
                            || pointer.has_grab(grab.previous_serial().unwrap_or(serial)))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
                }
            }
            Err(err) => {
                tracing::warn!("Failed to grab popup: {:?}", err);
            }
        }
    }

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
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
delegate_cursor_shape!(State);
delegate_xdg_shell!(State);
delegate_presentation!(State);

// Additional protocol support - these work out of the box
delegate_viewporter!(State);
delegate_pointer_gestures!(State);
delegate_relative_pointer!(State);
delegate_text_input_manager!(State);

// we already implement SeatHandler in input/mod.rs

// delegate output configuration protocol
use crate::delegate_output_configuration;
delegate_output_configuration!(State);
