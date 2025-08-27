// SPDX-License-Identifier: GPL-3.0-only

mod device;
mod drm_helpers;
pub mod surface;

use crate::{
    backend::render::GbmGlowBackend,
    state::{BackendData, State},
};
use anyhow::{Context, Result};
use indexmap::IndexMap;
use smithay::{
    backend::{
        drm::{DrmDeviceFd, DrmNode},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::multigpu::GpuManager,
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{UdevBackend, UdevEvent},
    },
    reexports::{
        calloop::{Dispatcher, EventLoop, LoopHandle},
        input::{self, Libinput},
        wayland_server::DisplayHandle,
    },
};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

pub use self::device::Device;

/// KMS backend state
pub struct KmsState {
    pub session: LibSeatSession,
    pub libinput: Libinput,
    pub drm_devices: IndexMap<DrmNode, Device>,
    pub input_devices: HashMap<String, input::Device>,
    pub primary_gpu: Option<DrmNode>,
    pub gpu_manager: GpuManager<GbmGlowBackend<DrmDeviceFd>>,
}

pub fn init_backend(
    dh: &DisplayHandle,
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
) -> Result<()> {
    info!("Initializing KMS backend");
    
    // establish session
    let (session, notifier) = LibSeatSession::new()
        .context("Failed to acquire session")?;
    
    info!("Session acquired on seat: {}", session.seat());
    
    // setup input
    let libinput_context = init_libinput(&session, &event_loop.handle())
        .context("Failed to initialize libinput backend")?;
    
    // watch for gpu events
    let udev_dispatcher = init_udev(session.seat(), &event_loop.handle())
        .context("Failed to initialize udev connection")?;
    
    // handle session events
    event_loop
        .handle()
        .insert_source(notifier, move |event, &mut (), state| match event {
            SessionEvent::ActivateSession => {
                info!("Session activated");
                state.session_active(true);
            }
            SessionEvent::PauseSession => {
                info!("Session paused");
                state.session_active(false);
            }
        })
        .map_err(|err| err.error)
        .context("Failed to initialize session event source")?;
    
    // initialize GPU manager
    let gpu_manager = GpuManager::new(GbmGlowBackend::new())?;
    
    // finish backend initialization
    state.backend = BackendData::Kms(KmsState {
        session,
        libinput: libinput_context,
        drm_devices: IndexMap::new(),
        input_devices: HashMap::new(),
        primary_gpu: None,
        gpu_manager,
    });
    
    // manually add already present gpus
    for (dev, path) in udev_dispatcher.as_source_ref().device_list() {
        if let Err(err) = state.device_added(dev, path.into(), dh) {
            warn!("Failed to add device {}: {:?}", path.display(), err);
        }
    }
    
    Ok(())
}

fn init_libinput(
    session: &LibSeatSession,
    evlh: &LoopHandle<'static, State>,
) -> Result<Libinput> {
    let mut libinput_context = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
        session.clone().into()
    );
    
    libinput_context
        .udev_assign_seat(&session.seat())
        .map_err(|_| anyhow::anyhow!("Failed to assign seat to libinput"))?;
    
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());
    
    evlh.insert_source(libinput_backend, move |mut event, _, state| {
        match &mut event {
            InputEvent::DeviceAdded { device } => {
                info!("Input device added: {}", device.name());
                // track input devices
                if let BackendData::Kms(kms) = &mut state.backend {
                    kms.input_devices.insert(device.name().into(), device.clone());
                }
            }
            InputEvent::DeviceRemoved { device } => {
                info!("Input device removed: {}", device.name());
                if let BackendData::Kms(kms) = &mut state.backend {
                    kms.input_devices.remove(device.name());
                }
            }
            _ => {
                // we'll handle actual input in a later phase
            }
        }
        
        state.process_input_event(event);
    })
    .map_err(|err| err.error)
    .context("Failed to initialize libinput event source")?;
    
    Ok(libinput_context)
}

fn init_udev(
    seat: String,
    evlh: &LoopHandle<'static, State>,
) -> Result<Dispatcher<'static, UdevBackend, State>> {
    let udev_backend = UdevBackend::new(&seat)?;
    
    let dispatcher = Dispatcher::new(udev_backend, move |event, _, state: &mut State| {
        let dh = state.display_handle.clone();
        match match event {
            UdevEvent::Added {
                device_id,
                ref path,
            } => state
                .device_added(device_id, path, &dh)
                .with_context(|| format!("Failed to add drm device: {}", device_id)),
            UdevEvent::Changed { device_id } => state
                .device_changed(device_id)
                .with_context(|| format!("Failed to update drm device: {}", device_id)),
            UdevEvent::Removed { device_id } => state
                .device_removed(device_id, &dh)
                .with_context(|| format!("Failed to remove drm device: {}", device_id)),
        } {
            Ok(()) => {
                debug!("Successfully handled udev event.");
            }
            Err(err) => {
                error!(?err, "Error while handling udev event.")
            }
        }
    });
    
    evlh.register_dispatcher(dispatcher.clone())
        .context("Failed to register udev event source")?;
    
    Ok(dispatcher)
}