// SPDX-License-Identifier: GPL-3.0-only

use crate::backend::kms::{KmsState, Device};
use smithay::{
    backend::{
        drm::DrmNode,
        input::InputEvent,
        session::Session,
    },
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{Display, DisplayHandle},
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
    session_active: bool,
}

// suppress warnings for now - we'll use these soon
#[allow(dead_code)]
impl State {
    pub fn socket_name(&self) -> &str {
        &self.socket_name
    }
}

impl State {
    pub fn new(
        display: &Display<State>,
        socket_name: String,
        loop_handle: LoopHandle<'static, State>,
        loop_signal: LoopSignal,
    ) -> Self {
        let display_handle = display.handle();
        
        Self {
            display_handle,
            loop_handle,
            loop_signal,
            should_stop: false,
            socket_name,
            backend: BackendData::Uninitialized,
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
    
    pub fn process_input_event(&mut self, event: InputEvent<impl smithay::backend::input::InputBackend>) {
        // we'll handle input processing in a later phase
        let _ = event;
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
        match Device::new(&mut kms.session, path, dev, &self.loop_handle) {
            Ok(mut device) => {
                tracing::info!("Successfully initialized DRM device: {:?}", drm_node);
                
                // set primary GPU if not set
                if kms.primary_gpu.is_none() {
                    kms.primary_gpu = Some(drm_node.clone());
                    tracing::info!("Setting primary GPU: {:?}", drm_node);
                }
                
                // update EGL and add to GPU manager if needed
                if let Err(err) = device.update_egl(kms.primary_gpu.as_ref(), kms.gpu_manager.as_mut()) {
                    tracing::warn!("Failed to initialize EGL for device {:?}: {}", drm_node, err);
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