// SPDX-License-Identifier: GPL-3.0-only

use crate::backend::kms::KmsState;
use smithay::{
    backend::{
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
    
    /// Handle device addition (stub for now - will be filled in Phase 2c)
    pub fn device_added(&mut self, dev: libc::dev_t, path: &std::path::Path, _dh: &DisplayHandle) -> anyhow::Result<()> {
        tracing::info!("Device added: {} ({})", path.display(), dev);
        
        // check if session is active
        if let BackendData::Kms(kms) = &self.backend {
            if !kms.session.is_active() {
                return Ok(());
            }
        }
        
        // we'll actually handle the device in Phase 2c
        Ok(())
    }
    
    /// Handle device change (stub for now)
    pub fn device_changed(&mut self, dev: libc::dev_t) -> anyhow::Result<()> {
        tracing::debug!("Device changed: {}", dev);
        // we'll handle this in a later phase
        Ok(())
    }
    
    /// Handle device removal (stub for now)
    pub fn device_removed(&mut self, dev: libc::dev_t, _dh: &DisplayHandle) -> anyhow::Result<()> {
        tracing::info!("Device removed: {}", dev);
        // we'll handle this in a later phase  
        Ok(())
    }
}