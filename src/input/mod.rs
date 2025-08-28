// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Device, DeviceCapability, InputBackend, InputEvent, 
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

use crate::State;

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
                        |_, modifiers, keysym| {
                            debug!(
                                "Key press: keycode={:?}, keysym={:?}, modifiers={:?}, state={:?}",
                                keycode,
                                keysym.modified_sym(),
                                modifiers,
                                state
                            );
                            
                            // for now, just forward all keys
                            // later we'll add keybindings here
                            FilterResult::<()>::Forward
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
                    
                    pointer.motion(
                        self,
                        None, // no surface under for now
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
                    
                    pointer.motion(
                        self,
                        None,
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