// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::{
        allocator::gbm::GbmAllocator,
        drm::{
            exporter::gbm::GbmFramebufferExporter,
            output::DrmOutput,
            DrmDeviceFd,
        },
    },
    output::Output,
    reexports::drm::control::{connector, crtc},
};
use std::collections::HashMap;
use tracing::{debug, info};

/// Type alias for our DRM output - following cosmic-comp's definition
/// Simplified version without presentation feedback for now
#[allow(dead_code)] // will be used in Phase 2f3 for actual rendering
pub type GbmDrmOutput = DrmOutput<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),  // simplified - no presentation feedback yet (cosmic-comp has complex feedback)
    DrmDeviceFd,
>;

/// Placeholder surface structure - cosmic-comp has complex threading
/// We'll expand this in Phase 2f3
#[derive(Debug)]
#[allow(dead_code)] // fields will be used in Phase 2f3+
pub struct Surface {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub output: Output,
    // cosmic-comp has render threads, dmabuf feedback, etc - we'll add later
}

impl Surface {
    pub fn new(
        output: Output,
        crtc: crtc::Handle,
        connector: connector::Handle,
    ) -> Self {
        info!("Creating surface for output {} on CRTC {:?}", output.name(), crtc);
        
        Self {
            connector,
            crtc,
            output,
        }
    }
}

/// Manages surfaces for outputs - simplified version of cosmic-comp's approach
pub struct SurfaceManager {
    surfaces: HashMap<crtc::Handle, Surface>,
}

impl SurfaceManager {
    pub fn new() -> Self {
        Self {
            surfaces: HashMap::new(),
        }
    }
    
    /// Create a surface for an output
    pub fn create_surface(
        &mut self,
        output: Output,
        crtc: crtc::Handle,
        connector: connector::Handle,
    ) {
        let surface = Surface::new(output, crtc, connector);
        self.surfaces.insert(crtc, surface);
        debug!("Surface created for CRTC {:?}", crtc);
    }
    
    #[allow(dead_code)] // will be used in Phase 2f3+ for surface operations
    pub fn get(&self, crtc: &crtc::Handle) -> Option<&Surface> {
        self.surfaces.get(crtc)
    }
    
    #[allow(dead_code)] // will be used in Phase 2f3+ for surface operations
    pub fn get_mut(&mut self, crtc: &crtc::Handle) -> Option<&mut Surface> {
        self.surfaces.get_mut(crtc)
    }
    
    #[allow(dead_code)] // will be used for output hotplug
    pub fn remove(&mut self, crtc: &crtc::Handle) -> Option<Surface> {
        self.surfaces.remove(crtc)
    }
}