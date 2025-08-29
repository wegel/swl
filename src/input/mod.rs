// SPDX-License-Identifier: GPL-3.0-only

mod keybindings;

use smithay::{
    backend::input::{
        AbsolutePositionEvent, ButtonState, Device, DeviceCapability, InputBackend, InputEvent, 
        KeyboardKeyEvent, PointerButtonEvent, PointerMotionEvent, PointerAxisEvent, 
        Axis, AxisSource,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
        Seat, SeatState, SeatHandler,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Point, SERIAL_COUNTER},
};
use tracing::{debug, info, trace};
use std::process::Command;

use crate::State;
use self::keybindings::{Keybindings, Action};

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
                    // check for keybindings
                    let keybindings = Keybindings::new();
                    
                    keyboard.input(
                        self,
                        keycode,
                        state,
                        serial,
                        time,
                        |state, modifiers, keysym| {
                            debug!(
                                "Key press: keycode={:?}, keysym={:?}, modifiers={:?}, state={:?}",
                                keycode,
                                keysym.modified_sym(),
                                modifiers,
                                event.state()
                            );
                            
                            // check if this is a keybinding
                            if let Some(action) = keybindings.check(modifiers, keysym.modified_sym(), event.state()) {
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
                    
                    // clamp to screen bounds (we'll improve this later with output tracking)
                    location.x = location.x.max(0.0);
                    location.y = location.y.max(0.0);
                    
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
                    // for now use a default size
                    let output_size = (1920.0, 1080.0);
                    let location = Point::from((
                        event.x() * output_size.0,
                        event.y() * output_size.1,
                    ));
                    
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
                        // update focused window in shell
                        self.shell.write().unwrap().focused_window = Some(window.clone());
                        
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
                }
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
                    shell.focus_next();
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
                    shell.focus_prev();
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
                shell.zoom();
            }
            CloseWindow => {
                let mut shell = self.shell.write().unwrap();
                shell.close_focused();
            }
            ToggleFloating => {
                let mut shell = self.shell.write().unwrap();
                if let Some(window) = shell.focused_window.clone() {
                    shell.toggle_floating(&window);
                }
            }
            
            // layout control
            IncreaseMasterWidth => {
                {
                    let mut shell = self.shell.write().unwrap();
                    shell.tiling.set_master_factor(0.05);
                    shell.arrange();
                }
            }
            DecreaseMasterWidth => {
                {
                    let mut shell = self.shell.write().unwrap();
                    shell.tiling.set_master_factor(-0.05);
                    shell.arrange();
                }
            }
            IncreaseMasterCount => {
                {
                    let mut shell = self.shell.write().unwrap();
                    shell.tiling.inc_n_master(1);
                    shell.arrange();
                }
            }
            DecreaseMasterCount => {
                {
                    let mut shell = self.shell.write().unwrap();
                    shell.tiling.inc_n_master(-1);
                    shell.arrange();
                }
            }
            
            // applications
            LaunchTerminal => {
                info!("Launching terminal");
                if let Err(e) = Command::new("foot").spawn() {
                    tracing::error!("Failed to launch terminal: {}", e);
                }
            }
            LaunchMenu => {
                info!("Launching menu");
                // try common menu programs
                if Command::new("rofi").arg("-show").arg("drun").spawn().is_err() {
                    if Command::new("dmenu_run").spawn().is_err() {
                        tracing::warn!("No menu program found (tried rofi, dmenu_run)");
                    }
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
        
        // also store in shell for rendering
        self.shell.write().unwrap().cursor_status = image;
        
        // schedule render for the output containing the cursor
        let cursor_position = self.shell.read().unwrap().cursor_position;
        if let Some(output) = self.shell.read().unwrap().output_at(cursor_position) {
            self.backend.schedule_render(&output);
        }
    }
    
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&Self::KeyboardFocus>) {
        // we'll handle focus changes when we have windows
    }
}