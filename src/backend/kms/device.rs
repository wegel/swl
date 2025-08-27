// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use smithay::{
    backend::{
        allocator::gbm::GbmDevice,
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode},
        egl::{EGLContext, EGLDevice, EGLDisplay, context::ContextPriority},
        session::Session,
    },
    reexports::{
        calloop::{LoopHandle, RegistrationToken},
        drm::control::{connector, crtc},
        rustix::fs::OFlags,
    },
    utils::DeviceFd,
};
use std::{
    collections::HashMap,
    fmt,
    path::Path,
};
use tracing::{debug, error, info, warn};

/// EGL context and display for rendering
#[derive(Debug)]
pub struct EGLInternals {
    pub display: EGLDisplay,
    pub device: EGLDevice,
    pub context: EGLContext,
}

/// A DRM device with rendering capabilities
pub struct Device {
    pub drm: DrmDevice,
    pub drm_node: DrmNode,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub egl: Option<EGLInternals>,
    pub render_node: DrmNode,
    pub supports_atomic: bool,
    pub event_token: Option<RegistrationToken>,
    
    // track outputs and surfaces (will be filled in later phases)
    pub outputs: HashMap<connector::Handle, ()>,  // placeholder
    pub surfaces: HashMap<crtc::Handle, ()>,      // placeholder
}

impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("drm_node", &self.drm_node)
            .field("render_node", &self.render_node)
            .field("supports_atomic", &self.supports_atomic)
            .field("outputs", &self.outputs.len())
            .field("surfaces", &self.surfaces.len())
            .finish()
    }
}

/// Initialize EGL context for a GBM device
pub fn init_egl(gbm: &GbmDevice<DrmDeviceFd>) -> Result<EGLInternals> {
    let display = unsafe { EGLDisplay::new(gbm.clone()) }
        .context("Failed to create EGLDisplay for device")?;
    
    let device = EGLDevice::device_for_display(&display)
        .context("Unable to find matching egl device")?;
    
    let context = EGLContext::new_with_priority(&display, ContextPriority::High)
        .context("Failed to create EGLContext for device")?;
    
    Ok(EGLInternals {
        display,
        device,
        context,
    })
}

impl Device {
    /// Create a new DRM device from a file descriptor
    pub fn new(
        session: &mut impl Session,
        path: &Path,
        dev: libc::dev_t,
        event_loop: &LoopHandle<'static, crate::state::State>,
    ) -> Result<Self> {
        info!("Initializing DRM device: {}", path.display());
        
        // open the device file
        let fd = session
            .open(
                path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|e| anyhow::anyhow!("Failed to open device {}: {:?}", path.display(), e))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        
        // initialize DRM device
        let (drm, notifier) = DrmDevice::new(fd.clone(), false)
            .with_context(|| format!("Failed to initialize drm device for: {}", path.display()))?;
        
        let drm_node = DrmNode::from_dev_id(dev)?;
        let supports_atomic = drm.is_atomic();
        
        info!(
            "DRM device initialized: {:?}, atomic modesetting: {}",
            drm_node,
            supports_atomic
        );
        
        // initialize GBM for buffer allocation
        let gbm = GbmDevice::new(fd)
            .with_context(|| format!("Failed to initialize GBM device for {}", path.display()))?;
        
        // try to initialize EGL for rendering
        let (egl, render_node) = match init_egl(&gbm) {
            Ok(egl) => {
                let render_node = egl
                    .device
                    .try_get_render_node()
                    .ok()
                    .and_then(std::convert::identity)
                    .unwrap_or(drm_node);
                
                info!("EGL initialized, render node: {:?}", render_node);
                (Some(egl), render_node)
            }
            Err(err) => {
                warn!("Failed to initialize EGL: {}", err);
                (None, drm_node)
            }
        };
        
        // register DRM event handler
        let token = event_loop
            .insert_source(notifier, move |event, _metadata, state| {
                match event {
                    DrmEvent::VBlank(crtc) => {
                        debug!("VBlank event for CRTC {:?}", crtc);
                        // we'll handle vblank events when we have surfaces
                    }
                    DrmEvent::Error(err) => {
                        error!(?err, "DRM device error");
                    }
                }
            })
            .context("Failed to add drm device to event loop")?;
        
        Ok(Device {
            drm,
            drm_node,
            gbm,
            egl,
            render_node,
            supports_atomic,
            event_token: Some(token),
            outputs: HashMap::new(),
            surfaces: HashMap::new(),
        })
    }
}