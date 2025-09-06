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
        allocator::{dmabuf::Dmabuf, Buffer},
        drm::{DrmDeviceFd, DrmNode},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::multigpu::GpuManager,
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{UdevBackend, UdevEvent},
    },
    output::Output,
    reexports::{
        calloop::{Dispatcher, EventLoop, LoopHandle},
        input::{self, Libinput},
        wayland_server::DisplayHandle,
    },
    wayland::dmabuf::DmabufGlobal,
};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
use tracing::{debug, error, info, trace, warn};

pub use self::device::Device;

/// KMS backend state
pub struct KmsState {
    pub session: LibSeatSession,
    pub libinput: Libinput,
    pub drm_devices: IndexMap<DrmNode, Device>,
    pub input_devices: HashMap<String, input::Device>,
    pub primary_gpu: Option<DrmNode>,
    pub primary_node: Arc<RwLock<Option<DrmNode>>>,
    pub gpu_manager: GpuManager<GbmGlowBackend<DrmDeviceFd>>,
}

impl KmsState {
    /// Schedule a render for the given output on all surfaces displaying it
    pub fn schedule_render(&mut self, output: &Output) {
        for device in self.drm_devices.values() {
            device.schedule_render(output);
        }
    }

    /// Import a dmabuf and verify it can be used
    pub fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf) -> Result<DrmNode> {
        // find device with EGL support to validate the dmabuf
        let mut last_err = anyhow::anyhow!("No device with EGL support found");

        for (node, device) in &self.drm_devices {
            if let Some(ref egl) = device.egl {
                // check if the format is supported
                if !egl
                    .display
                    .dmabuf_texture_formats()
                    .contains(&dmabuf.format())
                {
                    trace!(
                        "Skipping import of dmabuf on {:?}: unsupported format {:?}",
                        node,
                        dmabuf.format()
                    );
                    continue;
                }

                // try to create an EGL image to validate the dmabuf
                match egl.display.create_image_from_dmabuf(&dmabuf) {
                    Ok(image) => {
                        // successfully imported - destroy the test image
                        unsafe {
                            smithay::backend::egl::ffi::egl::DestroyImageKHR(
                                **egl.display.get_display_handle(),
                                image,
                            );
                        }
                        return Ok(device.render_node.clone());
                    }
                    Err(err) => {
                        debug!("Failed to import dmabuf on {:?}: {:?}", node, err);
                        last_err = anyhow::anyhow!("Failed to import dmabuf: {:?}", err);
                    }
                }
            }
        }

        Err(last_err)
    }
}

pub fn init_backend(
    dh: &DisplayHandle,
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
) -> Result<()> {
    info!("Initializing KMS backend");

    // establish session
    let (session, notifier) = LibSeatSession::new().context("Failed to acquire session")?;

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
    let primary_node = Arc::new(RwLock::new(None));

    // finish backend initialization
    state.backend = BackendData::Kms(KmsState {
        session,
        libinput: libinput_context,
        drm_devices: IndexMap::new(),
        input_devices: HashMap::new(),
        primary_gpu: None,
        primary_node,
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

fn init_libinput(session: &LibSeatSession, evlh: &LoopHandle<'static, State>) -> Result<Libinput> {
    let mut libinput_context =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());

    libinput_context
        .udev_assign_seat(&session.seat())
        .map_err(|_| anyhow::anyhow!("Failed to assign seat to libinput"))?;

    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());

    evlh.insert_source(libinput_backend, move |mut event, _, state| {
        match &mut event {
            InputEvent::DeviceAdded { device } => {
                info!("Input device added: {}", device.name());

                // configure touchpad tap-to-click
                if device.config_tap_finger_count() > 0 {
                    // this is a touchpad
                    info!("Configuring touchpad: {}", device.name());

                    // enable tap-to-click
                    if let Err(e) = device.config_tap_set_enabled(true) {
                        warn!("Failed to enable tap-to-click: {:?}", e);
                    }

                    // enable tap-and-drag
                    if let Err(e) = device.config_tap_set_drag_enabled(true) {
                        warn!("Failed to enable tap-drag: {:?}", e);
                    }

                    // enable drag lock (keep dragging when lifting finger briefly)
                    if let Err(e) = device.config_tap_set_drag_lock_enabled(true) {
                        warn!("Failed to enable tap-drag-lock: {:?}", e);
                    }

                    // disable touchpad while typing
                    if device.config_dwt_is_available() {
                        if let Err(e) = device.config_dwt_set_enabled(false) {
                            warn!("Failed to disable 'disable-while-typing': {:?}", e);
                        } else {
                            info!("Disabled 'disable-while-typing' for touchpad");
                        }
                    }
                }

                // track input devices
                if let BackendData::Kms(kms) = &mut state.backend {
                    kms.input_devices
                        .insert(device.name().into(), device.clone());
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
