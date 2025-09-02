// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    backend::kms::{KmsState, Device},
    backend::render::cursor::{CursorState, CursorStateInner},
    input::keybindings::Keybindings,
    shell::Shell,
};
use std::sync::{Arc, Mutex, RwLock};
use smithay::{
    backend::{
        drm::DrmNode,
        input::InputEvent,
        renderer::element::{RenderElementStates, default_primary_scanout_output_compare},
        session::Session,
    },
    desktop::{
        Window,
        PopupManager,
        utils::{
            update_surface_primary_scanout_output,
            with_surfaces_surface_tree,
        },
    },
    input::{Seat, SeatState},
    output::Output,
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufState, DmabufFeedbackBuilder},
        fractional_scale::with_fractional_scale,
        output::OutputManagerState,
        presentation::PresentationState,
        selection::{
            data_device::DataDeviceState,
            primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::{
            xdg::{XdgShellState, ToplevelSurface},
            wlr_layer::WlrLayerShellState,
        },
        shm::ShmState,
        viewporter::ViewporterState,
        pointer_gestures::PointerGesturesState,
        relative_pointer::RelativePointerManagerState,
        text_input::TextInputManagerState,
        xdg_activation::XdgActivationState,
        fractional_scale::FractionalScaleManagerState,
        cursor_shape::CursorShapeManagerState,
    },
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface},
    },
};

/// Backend data enum
pub enum BackendData {
    Uninitialized,
    Kms(KmsState),
    // we could add other backends later
}

/// The main compositor state
pub struct State {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub should_stop: bool,
    pub socket_name: String,
    pub backend: BackendData,
    pub seat_state: SeatState<State>,
    pub seat: Seat<State>,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    #[allow(dead_code)] // used by delegate_xdg_decoration macro
    pub xdg_decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub data_device_state: DataDeviceState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<smithay::wayland::dmabuf::DmabufGlobal>,
    #[allow(dead_code)] // will be used for output configuration protocol
    pub output_manager_state: OutputManagerState,
    #[allow(dead_code)] // used by presentation feedback protocol
    pub presentation_state: PresentationState,
    pub shell: Arc<RwLock<Shell>>,
    pub outputs: Vec<Output>,
    pub pending_windows: Vec<(ToplevelSurface, Window)>,
    pub popups: PopupManager,
    #[allow(dead_code)] // will be used for server-side cursor rendering
    pub cursor_state: CursorState,
    pub keybindings: Keybindings,
    session_active: bool,
    pub needs_focus_refresh: bool,
    // Additional protocol support
    #[allow(dead_code)]
    pub viewporter_state: ViewporterState,
    #[allow(dead_code)]
    pub pointer_gestures_state: PointerGesturesState,
    #[allow(dead_code)]
    pub relative_pointer_manager_state: RelativePointerManagerState,
    #[allow(dead_code)]
    pub text_input_manager_state: TextInputManagerState,
    #[allow(dead_code)]
    pub primary_selection_state: PrimarySelectionState,
    #[allow(dead_code)]
    pub data_control_state: DataControlState,
    #[allow(dead_code)]
    pub xdg_activation_state: XdgActivationState,
    #[allow(dead_code)]
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    #[allow(dead_code)]
    pub cursor_shape_manager_state: CursorShapeManagerState,
}

// suppress warnings for now - we'll use these soon
#[allow(dead_code)]
impl State {
    pub fn socket_name(&self) -> &str {
        &self.socket_name
    }
}

impl BackendData {
    /// Schedule a render for the given output
    pub fn schedule_render(&mut self, output: &Output) {
        match self {
            BackendData::Kms(kms) => kms.schedule_render(output),
            BackendData::Uninitialized => {},
        }
    }
}

impl State {
    pub fn new(
        display_handle: DisplayHandle,
        socket_name: String,
        loop_handle: LoopHandle<'static, State>,
        loop_signal: LoopSignal,
    ) -> Self {
        
        // create compositor state
        let compositor_state = CompositorState::new::<State>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<State>(&display_handle);
        let xdg_decoration_state = smithay::wayland::shell::xdg::decoration::XdgDecorationState::new::<State>(&display_handle);
        let layer_shell_state = WlrLayerShellState::new::<State>(&display_handle);
        let shm_state = ShmState::new::<State>(&display_handle, vec![]);
        let data_device_state = DataDeviceState::new::<State>(&display_handle);
        let dmabuf_state = DmabufState::new();
        let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&display_handle);
        
        // create seat state and the default seat
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, "seat0");
        
        // add pointer and keyboard capabilities
        seat.add_keyboard(Default::default(), 200, 25).unwrap();
        seat.add_pointer();
        
        // add cursor status to seat user data (following cosmic-comp)
        seat.user_data().insert_if_missing_threadsafe(|| {
            Mutex::new(smithay::input::pointer::CursorImageStatus::default_named())
        });
        // add cursor theme state
        seat.user_data().insert_if_missing_threadsafe(crate::backend::render::cursor::CursorState::default);
        
        // create the shell
        let shell = Arc::new(RwLock::new(Shell::new()));
        
        // create presentation state
        // using CLOCK_MONOTONIC (id = 1) as the clock
        let presentation_state = PresentationState::new::<State>(&display_handle, 1);
        
        // Initialize additional protocol support
        let viewporter_state = ViewporterState::new::<State>(&display_handle);
        let pointer_gestures_state = PointerGesturesState::new::<State>(&display_handle);
        let relative_pointer_manager_state = RelativePointerManagerState::new::<State>(&display_handle);
        let text_input_manager_state = TextInputManagerState::new::<State>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<State>(&display_handle);
        let data_control_state = DataControlState::new::<State, _>(&display_handle, Some(&primary_selection_state), |_| true);
        let xdg_activation_state = XdgActivationState::new::<State>(&display_handle);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<State>(&display_handle);
        let cursor_shape_manager_state = CursorShapeManagerState::new::<State>(&display_handle);
        
        Self {
            display_handle: display_handle.clone(),
            loop_handle,
            loop_signal,
            should_stop: false,
            socket_name,
            backend: BackendData::Uninitialized,
            seat_state,
            seat,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            layer_shell_state,
            shm_state,
            data_device_state,
            dmabuf_state,
            dmabuf_global: None,
            output_manager_state,
            presentation_state,
            shell,
            outputs: Vec::new(),
            pending_windows: Vec::new(),
            popups: PopupManager::default(),
            cursor_state: Mutex::new(CursorStateInner::default()),
            keybindings: Keybindings::new(),
            session_active: false,
            needs_focus_refresh: false,
            viewporter_state,
            pointer_gestures_state,
            relative_pointer_manager_state,
            text_input_manager_state,
            primary_selection_state,
            data_control_state,
            xdg_activation_state,
            fractional_scale_manager_state,
            cursor_shape_manager_state,
        }
    }
    
    pub fn session_active(&mut self, active: bool) {
        self.session_active = active;
        if active {
            // resume operations
            if let BackendData::Kms(kms) = &mut self.backend {
                if let Err(err) = kms.libinput.resume() {
                    tracing::error!(?err, "Failed to resume libinput context");
                }
            }
        } else {
            // pause operations
            if let BackendData::Kms(kms) = &self.backend {
                kms.libinput.suspend();
            }
        }
    }
    
    /// Refresh focus to the topmost window in the focus stack
    /// This is called from the main event loop when needs_focus_refresh is set
    pub fn refresh_focus(&mut self) {
        use smithay::utils::IsAlive;
        
        // get current keyboard focus
        let keyboard = self.seat.get_keyboard().unwrap();
        let current_focus = keyboard.current_focus();
        
        // check if current focus is still valid
        if let Some(ref target) = current_focus {
            if target.alive() {
                // focus is still valid, nothing to do
                return;
            }
        }
        
        // current focus is invalid or none, restore from focus stack
        let window = self.shell.write().unwrap().refresh_focus();
        
        if let Some(window) = window {
            // restore keyboard focus to the window's surface
            let surface = window.toplevel().unwrap().wl_surface().clone();
            keyboard.set_focus(self, Some(surface), smithay::utils::SERIAL_COUNTER.next_serial());
            
            // also update pointer focus if needed
            if let Some(output) = self.outputs.first() {
                self.backend.schedule_render(output);
            }
            
            tracing::info!("Focus restored to window");
        } else {
            // no window to focus, clear keyboard focus
            keyboard.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial());
            tracing::info!("No window to restore focus to, cleared focus");
        }
    }
    
    pub fn process_input_event<B: smithay::backend::input::InputBackend>(&mut self, event: InputEvent<B>) 
    where
        <B as smithay::backend::input::InputBackend>::Device: 'static,
    {
        // delegate to our input handler
        self.process_input_event_impl(event);
    }
    
    /// Update primary output and fractional scale for all surfaces on the given output
    pub fn update_primary_output(&self, output: &Output, render_element_states: &RenderElementStates) {
        let shell = self.shell.read().unwrap();
        
        // Processor function that updates primary output and fractional scale
        let processor = |surface: &WlSurface, states: &smithay::wayland::compositor::SurfaceData| {
            let primary_scanout_output = update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
            
            // If the primary output changed, update the fractional scale
            if let Some(output) = primary_scanout_output {
                with_fractional_scale(states, |fraction_scale| {
                    fraction_scale.set_preferred_scale(output.current_scale().fractional_scale());
                });
            }
        };
        
        // Process all windows in the space
        for window in shell.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                with_surfaces_surface_tree(toplevel.wl_surface(), processor);
            }
        }
        
        // Process layer shell surfaces
        let layer_map = smithay::desktop::layer_map_for_output(output);
        for surface in layer_map.layers() {
            with_surfaces_surface_tree(surface.wl_surface(), processor);
        }
        
        // Process cursor surfaces  
        if let Some(pointer) = self.seat.get_pointer() {
            if let Some(surface) = pointer.current_focus() {
                with_surfaces_surface_tree(&surface, processor);
            }
        }
    }
    
    /// Handle device addition
    pub fn device_added(&mut self, dev: libc::dev_t, path: &std::path::Path, _dh: &DisplayHandle) -> anyhow::Result<()> {
        tracing::info!("Device added: {} ({})", path.display(), dev);
        
        let BackendData::Kms(kms) = &mut self.backend else {
            return Ok(());
        };
        
        // check if session is active
        if !kms.session.is_active() {
            return Ok(());
        }
        
        // check if this is actually a DRM device
        let Ok(drm_node) = DrmNode::from_dev_id(dev) else {
            tracing::debug!("Device {} is not a DRM device", path.display());
            return Ok(());
        };
        
        // don't add the same device twice
        if kms.drm_devices.contains_key(&drm_node) {
            tracing::debug!("Device {:?} already added", drm_node);
            return Ok(());
        }
        
        // create the device
        match Device::new(&mut kms.session, path, dev, &self.loop_handle, &mut kms.gpu_manager, kms.primary_node.clone()) {
            Ok(mut device) => {
                tracing::info!("Successfully initialized DRM device: {:?}", drm_node);
                
                // set primary GPU if not set
                if kms.primary_gpu.is_none() {
                    kms.primary_gpu = Some(drm_node.clone());
                    *kms.primary_node.write().unwrap() = Some(drm_node.clone());
                    tracing::info!("Setting primary GPU: {:?}", drm_node);
                }
                
                // update EGL and add to GPU manager if needed
                let should_create_dmabuf = if let Err(err) = device.update_egl(kms.primary_gpu.as_ref(), &mut kms.gpu_manager) {
                    tracing::warn!("Failed to initialize EGL for device {:?}: {}", drm_node, err);
                    false
                } else {
                    device.egl.is_some() && self.dmabuf_global.is_none()
                };
                
                // Create dmabuf global if needed (do this before scan_outputs to avoid borrow conflicts)
                if should_create_dmabuf {
                    // Extract needed info from device
                    let render_node = device.render_node.clone();
                    let formats = device.egl.as_ref().unwrap().display.dmabuf_texture_formats();
                    
                    // Create dmabuf feedback
                    if let Ok(feedback) = DmabufFeedbackBuilder::new(render_node.dev_id(), formats.clone()).build() {
                        // Create the global and store it
                        let global = self.dmabuf_state
                            .create_global_with_default_feedback::<State>(&self.display_handle, &feedback);
                        
                        self.dmabuf_global = Some(global);
                        
                        tracing::info!(
                            "Created dmabuf global for device {:?} with {} formats",
                            render_node,
                            formats.indexset().len()
                        );
                    } else {
                        tracing::warn!("Failed to create dmabuf feedback");
                    }
                }
                
                // scan for connected outputs
                match device.scan_outputs(&self.display_handle, &self.loop_handle, &mut kms.gpu_manager, self.shell.clone(), self.seat.clone()) {
                    Ok(outputs) => {
                        // add outputs to the shell's space
                        for output in &outputs {
                            self.shell.write().unwrap().add_output(output);
                        }
                        // add outputs to our state
                        self.outputs.extend(outputs.clone());
                        
                        // schedule initial render for each output
                        for output in outputs {
                            device.schedule_render(&output);
                        }
                    }
                    Err(err) => {
                        tracing::warn!("Failed to scan outputs for device {:?}: {}", drm_node, err);
                    }
                }
                
                kms.drm_devices.insert(drm_node, device);
                Ok(())
            }
            Err(err) => {
                tracing::warn!("Failed to initialize DRM device {}: {}", path.display(), err);
                Ok(()) // non-fatal, might not be a GPU we can use
            }
        }
    }
    
    /// Handle device change (stub for now)
    pub fn device_changed(&mut self, dev: libc::dev_t) -> anyhow::Result<()> {
        tracing::debug!("Device changed: {}", dev);
        // we'll handle this in a later phase
        Ok(())
    }
    
    /// Handle device removal
    pub fn device_removed(&mut self, dev: libc::dev_t, _dh: &DisplayHandle) -> anyhow::Result<()> {
        tracing::info!("Device removed: {}", dev);
        
        let BackendData::Kms(kms) = &mut self.backend else {
            return Ok(());
        };
        
        // find and remove the device
        if let Ok(drm_node) = DrmNode::from_dev_id(dev) {
            if let Some(mut device) = kms.drm_devices.shift_remove(&drm_node) {
                tracing::info!("Removing DRM device: {:?}", drm_node);
                
                // remove from GPU manager
                kms.gpu_manager.as_mut().remove_node(&drm_node);
                
                // remove event source from event loop
                if let Some(token) = device.event_token.take() {
                    self.loop_handle.remove(token);
                }
                
                // if this was the primary GPU, try to find another
                if kms.primary_gpu.as_ref() == Some(&drm_node) {
                    kms.primary_gpu = kms.drm_devices.keys().next().cloned();
                    if let Some(ref new_primary) = kms.primary_gpu {
                        tracing::info!("New primary GPU: {:?}", new_primary);
                    }
                }
            }
        }
        
        Ok(())
    }
    
}