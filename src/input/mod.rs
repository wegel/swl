// SPDX-License-Identifier: GPL-3.0-only

pub mod keybindings;

use smithay::{
    backend::input::{
        AbsolutePositionEvent, ButtonState, Device, DeviceCapability, InputBackend, InputEvent, 
        KeyboardKeyEvent, PointerButtonEvent, PointerMotionEvent, PointerAxisEvent, 
        Axis, AxisSource,
        GestureBeginEvent, GestureEndEvent,
        GestureSwipeUpdateEvent as GestureSwipeUpdateEventTrait,
        GesturePinchUpdateEvent as GesturePinchUpdateEventTrait,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent,
            GestureSwipeBeginEvent as PointerSwipeBeginEvent,
            GestureSwipeUpdateEvent as PointerSwipeUpdateEvent,
            GestureSwipeEndEvent as PointerSwipeEndEvent,
            GesturePinchBeginEvent as PointerPinchBeginEvent,
            GesturePinchUpdateEvent as PointerPinchUpdateEvent,
            GesturePinchEndEvent as PointerPinchEndEvent,
            GestureHoldBeginEvent as PointerHoldBeginEvent,
            GestureHoldEndEvent as PointerHoldEndEvent,
        },
        Seat, SeatState, SeatHandler,
    },
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{Point, SERIAL_COUNTER},
    wayland::selection::{data_device::set_data_device_focus, primary_selection::set_primary_focus},
};
use tracing::{debug, info, trace};
use std::process::Command;

use crate::State;
use self::keybindings::Action;

impl State {
    /// Process input events from the backend
    pub fn process_input_event_impl<B: InputBackend>(&mut self, event: InputEvent<B>)
    where
        <B as InputBackend>::Device: 'static,
    {
        use smithay::backend::input::Event;
        
        match event {
            InputEvent::DeviceAdded { device } => {
                info!("Device added: {:?}", device.name());
                
                // add device to our main seat
                {
                    let seat = &self.seat;
                    // configure keyboard if device has keyboard capability
                    if device.has_capability(DeviceCapability::Keyboard) {
                        let _keyboard = seat.get_keyboard().unwrap();
                        // keyboard config is already set in State::new
                    }
                }
            }
            
            InputEvent::DeviceRemoved { device } => {
                info!("Device removed: {:?}", device.name());
            }
            
            InputEvent::Keyboard { event, .. } => {
                let keycode = event.key_code();
                let state = event.state();
                trace!(?keycode, ?state, "Keyboard event");
                
                // use our main seat
                {
                    let seat = &self.seat;
                    let serial = SERIAL_COUNTER.next_serial();
                    let time = Event::time_msec(&event);
                    let keyboard = seat.get_keyboard().unwrap();
                    
                    // process the key input
                    keyboard.input(
                        self,
                        keycode,
                        state,
                        serial,
                        time,
                        |state, modifiers, keysym| {
                            // check if this is a keybinding
                            // Use raw_latin_sym_or_raw_current_sym() to get the unshifted key for bindings
                            let key = keysym.raw_latin_sym_or_raw_current_sym().unwrap_or(keysym.modified_sym());
                            if let Some(action) = state.keybindings.check(modifiers, key, event.state()) {
                                trace!("Key intercepted for action: {:?}", action);
                                state.handle_action(action);
                                FilterResult::Intercept(())
                            } else {
                                // forward to client
                                FilterResult::Forward
                            }
                        },
                    );
                }
            }
            
            InputEvent::PointerMotion { event, .. } => {
                let delta = event.delta();
                trace!("Pointer motion: {:?}", delta);
                
                {
                    let seat = &self.seat;
                    let pointer = seat.get_pointer().unwrap();
                    
                    // update pointer position
                    let mut location = pointer.current_location();
                    location += delta;
                    
                    // get output bounds and clamp pointer to screen
                    if let Some(output) = self.outputs.first() {
                        if let Some(mode) = output.current_mode() {
                            let scale = output.current_scale().fractional_scale();
                            let max_x = (mode.size.w as f64 / scale) - 1.0;
                            let max_y = (mode.size.h as f64 / scale) - 1.0;
                            
                            location.x = location.x.clamp(0.0, max_x);
                            location.y = location.y.clamp(0.0, max_y);
                        }
                    } else {
                        // fallback if no output
                        location.x = location.x.max(0.0);
                        location.y = location.y.max(0.0);
                    }
                    
                    let serial = SERIAL_COUNTER.next_serial();
                    let time = Event::time_msec(&event);
                    
                    // find surface under cursor (including decorations)
                    let surface_under = self.shell.read().unwrap().surface_under(location);
                    
                    pointer.motion(
                        self,
                        surface_under,
                        &MotionEvent {
                            location,
                            serial,
                            time,
                        },
                    );
                    
                    // update cursor position in shell (for rendering)
                    self.shell.write().unwrap().cursor_position = location;
                    
                    // schedule render for the output containing the cursor
                    if let Some(output) = self.shell.read().unwrap().output_at(location) {
                        self.backend.schedule_render(&output);
                    }
                }
            }
            
            InputEvent::PointerMotionAbsolute { event, .. } => {
                trace!("Pointer absolute motion");
                
                {
                    let seat = &self.seat;
                    let pointer = seat.get_pointer().unwrap();
                    
                    // for absolute motion, we need output dimensions
                    let location = if let Some(output) = self.outputs.first() {
                        if let Some(mode) = output.current_mode() {
                            let scale = output.current_scale().fractional_scale();
                            let width = mode.size.w as f64 / scale;
                            let height = mode.size.h as f64 / scale;
                            Point::from((
                                (event.x() * width).clamp(0.0, width - 1.0),
                                (event.y() * height).clamp(0.0, height - 1.0),
                            ))
                        } else {
                            // fallback if no mode
                            Point::from((
                                event.x() * 1920.0,
                                event.y() * 1080.0,
                            ))
                        }
                    } else {
                        // fallback if no output
                        Point::from((
                            event.x() * 1920.0,
                            event.y() * 1080.0,
                        ))
                    };
                    
                    let serial = SERIAL_COUNTER.next_serial();
                    let time = Event::time_msec(&event);
                    
                    // find surface under cursor (including decorations)
                    let surface_under = self.shell.read().unwrap().surface_under(location);
                    
                    pointer.motion(
                        self,
                        surface_under,
                        &MotionEvent {
                            location,
                            serial,
                            time,
                        },
                    );
                    
                    // update cursor position in shell (for rendering)
                    self.shell.write().unwrap().cursor_position = location;
                    
                    // schedule render for the output containing the cursor
                    if let Some(output) = self.shell.read().unwrap().output_at(location) {
                        self.backend.schedule_render(&output);
                    }
                }
            }
            
            InputEvent::PointerButton { event, .. } => {
                let button = event.button_code();
                let state = event.state();
                debug!("Pointer button: {} {:?}", button, state);
                
                // on button press, check if we need to focus a different window
                if state == ButtonState::Pressed {
                    let pointer_loc = self.seat.get_pointer().unwrap().current_location();
                    debug!("Button pressed at location: {:?}", pointer_loc);
                    
                    // find window under cursor and focus it
                    let window_to_focus = {
                        let shell = self.shell.read().unwrap();
                        let window = shell.window_under(pointer_loc);
                        debug!("Window under cursor: {:?}", window.is_some());
                        window
                    };
                    
                    if let Some(window) = window_to_focus {
                        // update focus stack and focused window
                        self.shell.write().unwrap().set_focus(window.clone());
                        
                        // set keyboard focus
                        if let Some(surface) = window.toplevel().and_then(|t| Some(t.wl_surface().clone())) {
                            let keyboard = self.seat.get_keyboard().unwrap();
                            let serial = SERIAL_COUNTER.next_serial();
                            keyboard.set_focus(self, Some(surface), serial);
                            debug!("Set keyboard focus to clicked window");
                        }
                    } else {
                        debug!("No window found under cursor for focus");
                    }
                }
                
                {
                    let seat = &self.seat;
                    let pointer = seat.get_pointer().unwrap();
                    let serial = SERIAL_COUNTER.next_serial();
                    let time = Event::time_msec(&event);
                    
                    pointer.button(
                        self,
                        &ButtonEvent {
                            button,
                            state: state.into(),
                            serial,
                            time,
                        },
                    );
                }
            }
            
            InputEvent::PointerAxis { event, .. } => {
                trace!("Pointer axis");
                
                {
                    let seat = &self.seat;
                    let pointer = seat.get_pointer().unwrap();
                    let source = event.source();
                    
                    let mut frame = AxisFrame::new(Event::time_msec(&event))
                        .source(source);
                    
                    if let Some(horizontal) = event.amount(Axis::Horizontal) {
                        frame = frame.value(Axis::Horizontal, horizontal);
                        if let Some(discrete) = event.amount_v120(Axis::Horizontal) {
                            frame = frame.v120(Axis::Horizontal, discrete as i32);
                        }
                    }
                    
                    if let Some(vertical) = event.amount(Axis::Vertical) {
                        frame = frame.value(Axis::Vertical, vertical);
                        if let Some(discrete) = event.amount_v120(Axis::Vertical) {
                            frame = frame.v120(Axis::Vertical, discrete as i32);
                        }
                    }
                    
                    if source == AxisSource::Finger {
                        if event.amount(Axis::Horizontal) == Some(0.0)
                            && event.amount(Axis::Vertical) == Some(0.0)
                        {
                            frame = frame.stop(Axis::Horizontal).stop(Axis::Vertical);
                        }
                    }
                    
                    pointer.axis(self, frame);
                    pointer.frame(self);
                }
            }
            
            // Gesture events for touchpad support
            InputEvent::GestureSwipeBegin { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_swipe_begin(
                    self,
                    &PointerSwipeBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
            
            InputEvent::GestureSwipeUpdate { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_swipe_update(
                    self,
                    &PointerSwipeUpdateEvent {
                        time: event.time_msec(),
                        delta: event.delta(),
                    },
                );
            }
            
            InputEvent::GestureSwipeEnd { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_swipe_end(
                    self,
                    &PointerSwipeEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
            
            InputEvent::GesturePinchBegin { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_pinch_begin(
                    self,
                    &PointerPinchBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
            
            InputEvent::GesturePinchUpdate { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_pinch_update(
                    self,
                    &PointerPinchUpdateEvent {
                        time: event.time_msec(),
                        delta: event.delta(),
                        scale: event.scale(),
                        rotation: event.rotation(),
                    },
                );
            }
            
            InputEvent::GesturePinchEnd { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_pinch_end(
                    self,
                    &PointerPinchEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
            
            InputEvent::GestureHoldBegin { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_hold_begin(
                    self,
                    &PointerHoldBeginEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
            }
            
            InputEvent::GestureHoldEnd { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.gesture_hold_end(
                    self,
                    &PointerHoldEndEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
            
            _ => {
                // ignore other events for now
                trace!("Unhandled input event");
            }
        }
    }
    
    /// Handle a keybinding action
    fn handle_action(&mut self, action: Action) {
        use Action::*;
        
        match action {
            // window management
            FocusNext => {
                let surface = {
                    let mut shell = self.shell.write().unwrap();
                    // Use first output for now (single monitor)
                    if let Some(output) = self.outputs.first() {
                        shell.focus_next(output);
                    }
                    // get surface to focus
                    shell.focused_window.as_ref()
                        .and_then(|w| w.toplevel())
                        .map(|t| t.wl_surface().clone())
                };
                // update keyboard focus
                if let Some(surface) = surface {
                    let keyboard = self.seat.get_keyboard().unwrap();
                    let serial = SERIAL_COUNTER.next_serial();
                    keyboard.set_focus(self, Some(surface), serial);
                }
            }
            FocusPrev => {
                let surface = {
                    let mut shell = self.shell.write().unwrap();
                    // Use first output for now (single monitor)
                    if let Some(output) = self.outputs.first() {
                        shell.focus_prev(output);
                    }
                    // get surface to focus
                    shell.focused_window.as_ref()
                        .and_then(|w| w.toplevel())
                        .map(|t| t.wl_surface().clone())
                };
                // update keyboard focus
                if let Some(surface) = surface {
                    let keyboard = self.seat.get_keyboard().unwrap();
                    let serial = SERIAL_COUNTER.next_serial();
                    keyboard.set_focus(self, Some(surface), serial);
                }
            }
            Zoom => {
                let mut shell = self.shell.write().unwrap();
                // Use first output for now (single monitor)
                if let Some(output) = self.outputs.first() {
                    shell.zoom(output);
                    drop(shell);
                    self.backend.schedule_render(output);
                }
            }
            CloseWindow => {
                let mut shell = self.shell.write().unwrap();
                shell.close_focused();
            }
            ToggleFloating => {
                let mut shell = self.shell.write().unwrap();
                if let Some(window) = shell.focused_window.clone() {
                    // Use first output for now (single monitor)
                    if let Some(output) = self.outputs.first() {
                        shell.toggle_floating(&window, output);
                        drop(shell);
                        self.backend.schedule_render(output);
                    }
                }
            }
            
            // layout control
            IncreaseMasterWidth => {
                let mut shell = self.shell.write().unwrap();
                // Apply to active workspace on first output
                if let Some(output) = self.outputs.first() {
                    if let Some(workspace) = shell.active_workspace_mut(output) {
                        workspace.tiling.set_master_factor(0.05);
                        workspace.needs_arrange = true;
                    }
                    drop(shell);
                    self.backend.schedule_render(output);
                }
            }
            DecreaseMasterWidth => {
                let mut shell = self.shell.write().unwrap();
                // Apply to active workspace on first output
                if let Some(output) = self.outputs.first() {
                    if let Some(workspace) = shell.active_workspace_mut(output) {
                        workspace.tiling.set_master_factor(-0.05);
                        workspace.needs_arrange = true;
                    }
                    drop(shell);
                    self.backend.schedule_render(output);
                }
            }
            IncreaseMasterCount => {
                let mut shell = self.shell.write().unwrap();
                // Apply to active workspace on first output
                if let Some(output) = self.outputs.first() {
                    if let Some(workspace) = shell.active_workspace_mut(output) {
                        workspace.tiling.inc_n_master(1);
                        workspace.needs_arrange = true;
                    }
                    drop(shell);
                    self.backend.schedule_render(output);
                }
            }
            DecreaseMasterCount => {
                let mut shell = self.shell.write().unwrap();
                // Apply to active workspace on first output
                if let Some(output) = self.outputs.first() {
                    if let Some(workspace) = shell.active_workspace_mut(output) {
                        workspace.tiling.inc_n_master(-1);
                        workspace.needs_arrange = true;
                    }
                    drop(shell);
                    self.backend.schedule_render(output);
                }
            }
            
            // applications
            LaunchTerminal => {
                info!("Launching terminal");
                if let Err(e) = Command::new("foot")
                    .env("WAYLAND_DISPLAY", &self.socket_name)
                    .spawn() {
                    tracing::error!("Failed to launch terminal: {}", e);
                }
            }
            LaunchMenu => {
                info!("Launching menu");
                // try common menu programs
                if Command::new("wofi")
                    .arg("--show")
                    .arg("drun")
                    .env("WAYLAND_DISPLAY", &self.socket_name)
                    .spawn()
                    .is_err() 
                {
                    if Command::new("dmenu_run")
                        .env("WAYLAND_DISPLAY", &self.socket_name)
                        .spawn()
                        .is_err() 
                    {
                        tracing::warn!("No menu program found (tried wofi, dmenu_run)");
                    }
                }
            }
            
            // workspace management
            SwitchToWorkspace(name) => {
                if let Some(output) = self.outputs.first().cloned() {
                    // Switch workspace and get the focused window
                    let focused_window = {
                        let mut shell = self.shell.write().unwrap();
                        shell.switch_to_workspace(&output, name);
                        shell.focused_window.clone()
                    }; // shell lock dropped here
                    
                    // Now update keyboard focus
                    if let Some(window) = focused_window {
                        if let Some(surface) = window.toplevel().map(|t| t.wl_surface().clone()) {
                            let keyboard = self.seat.get_keyboard().unwrap();
                            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                            keyboard.set_focus(self, Some(surface), serial);
                            tracing::debug!("Updated keyboard focus after workspace switch");
                        }
                    } else {
                        // Clear keyboard focus when no window is focused
                        let keyboard = self.seat.get_keyboard().unwrap();
                        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                        keyboard.set_focus(self, None, serial);
                        tracing::debug!("Cleared keyboard focus after workspace switch");
                    }
                    
                    self.backend.schedule_render(&output);
                }
            }
            MoveToWorkspace(name) => {
                if let Some(output) = self.outputs.first().cloned() {
                    // Move window and get the focused window
                    let focused_window = {
                        let mut shell = self.shell.write().unwrap();
                        
                        // Get the focused window
                        if let Some(window) = shell.focused_window.clone() {
                            // Remove window from current workspace
                            shell.remove_window(&window);
                            
                            // Switch to target workspace
                            shell.switch_to_workspace(&output, name.clone());
                            
                            // Add window to new workspace
                            shell.add_window(window.clone(), &output);
                            
                            Some(window)
                        } else {
                            None
                        }
                    }; // shell lock dropped here
                    
                    // Update keyboard focus to ensure it follows the moved window
                    if let Some(window) = focused_window {
                        if let Some(surface) = window.toplevel().map(|t| t.wl_surface().clone()) {
                            let keyboard = self.seat.get_keyboard().unwrap();
                            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                            keyboard.set_focus(self, Some(surface), serial);
                            tracing::debug!("Updated keyboard focus after moving window to workspace");
                        }
                    }
                    
                    self.backend.schedule_render(&output);
                }
            }
            
            // system
            Quit => {
                info!("Quit requested via keybinding");
                self.loop_signal.stop();
                self.should_stop = true;
            }
        }
    }
}

// implement SeatHandler for State
impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    
    fn cursor_image(&mut self, seat: &Seat<Self>, image: smithay::input::pointer::CursorImageStatus) {
        // store cursor status in seat user data (following cosmic-comp)
        let cursor_status = seat.user_data().get::<std::sync::Mutex<smithay::input::pointer::CursorImageStatus>>().unwrap();
        *cursor_status.lock().unwrap() = image.clone();
        
        // also update cursor theme state if it's a named cursor
        if let smithay::input::pointer::CursorImageStatus::Named(icon) = &image {
            let cursor_state = seat.user_data().get::<crate::backend::render::cursor::CursorState>().unwrap();
            cursor_state.lock().unwrap().current_cursor = Some(*icon);
        }
        
        // also store in shell for rendering
        self.shell.write().unwrap().cursor_status = image;
        
        // schedule render for the output containing the cursor
        let cursor_position = self.shell.read().unwrap().cursor_position;
        if let Some(output) = self.shell.read().unwrap().output_at(cursor_position) {
            self.backend.schedule_render(&output);
        }
    }
    
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Self::KeyboardFocus>) {
        // Update clipboard focus when keyboard focus changes
        let client = focused
            .and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }
}

// implement TabletSeatHandler for State
impl smithay::wayland::tablet_manager::TabletSeatHandler for State {
    fn tablet_tool_image(&mut self, _tool: &smithay::backend::input::TabletToolDescriptor, _image: smithay::input::pointer::CursorImageStatus) {
        // we don't handle tablet tools yet
    }
}