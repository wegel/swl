// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    backend::kms::{KmsState, Device},
    backend::render::cursor::{CursorState, CursorStateInner},
    shell::Shell,
};
use std::sync::{Arc, Mutex, RwLock};
use smithay::{
    backend::{
        drm::DrmNode,
        input::InputEvent,
        session::Session,
    },
    desktop::Window,
    input::{Seat, SeatState},
    output::Output,
    wayland::{
        compositor::CompositorState,
        output::OutputManagerState,
        presentation::PresentationState,
        selection::data_device::DataDeviceState,
        shell::xdg::{XdgShellState, ToplevelSurface},
        shm::ShmState,
    },
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{DisplayHandle},
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
    pub shm_state: ShmState,
    pub data_device_state: DataDeviceState,
    pub output_manager_state: OutputManagerState,
    pub presentation_state: PresentationState,
    pub shell: Arc<RwLock<Shell>>,
    pub outputs: Vec<Output>,
    pub pending_windows: Vec<(ToplevelSurface, Window)>,
    pub cursor_state: CursorState,
    session_active: bool,
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
        let shm_state = ShmState::new::<State>(&display_handle, vec![]);
        let data_device_state = DataDeviceState::new::<State>(&display_handle);
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
        
        // create the shell
        let shell = Arc::new(RwLock::new(Shell::new()));
        
        // create presentation state
        // using CLOCK_MONOTONIC (id = 1) as the clock
        let presentation_state = PresentationState::new::<State>(&display_handle, 1);
        
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
            shm_state,
            data_device_state,
            output_manager_state,
            presentation_state,
            shell,
            outputs: Vec::new(),
            pending_windows: Vec::new(),
            cursor_state: Mutex::new(CursorStateInner::default()),
            session_active: false,
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
    
    pub fn process_input_event<B: smithay::backend::input::InputBackend>(&mut self, event: InputEvent<B>) 
    where
        <B as smithay::backend::input::InputBackend>::Device: 'static,
    {
        // delegate to our input handler
        self.process_input_event_impl(event);
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
                if let Err(err) = device.update_egl(kms.primary_gpu.as_ref(), &mut kms.gpu_manager) {
                    tracing::warn!("Failed to initialize EGL for device {:?}: {}", drm_node, err);
                }
                
                // scan for connected outputs
                match device.scan_outputs(&self.display_handle, &self.loop_handle, &mut kms.gpu_manager, self.shell.clone()) {
                    Ok(outputs) => {
                        // add outputs to the shell's space
                        for output in &outputs {
                            self.shell.write().unwrap().add_output(output);
                        }
                        // add outputs to our state
                        self.outputs.extend(outputs);
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