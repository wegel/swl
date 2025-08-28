// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmDevice},
            Fourcc,
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutputManager, LockedDrmOutputManager},
        },
        egl::{EGLContext, EGLDevice, EGLDisplay, context::ContextPriority},
        renderer::{
            glow::GlowRenderer,
            multigpu::GpuManager,
        },
        session::Session,
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{LoopHandle, RegistrationToken},
        drm::control::{connector, crtc, ModeTypeFlags},
        gbm::BufferObjectFlags as GbmBufferFlags,
        rustix::fs::OFlags,
        wayland_server::DisplayHandle,
    },
    utils::{DeviceFd, Point, Transform},
};
use std::{
    collections::HashMap,
    fmt,
    path::Path,
    sync::{Arc, RwLock},
};
use tracing::{debug, error, info, warn};

use crate::backend::render::{GlMultiRenderer, element::CosmicElement};

/// EGL context and display for rendering
#[derive(Debug)]
pub struct EGLInternals {
    pub display: EGLDisplay,
    pub device: EGLDevice,
    pub context: EGLContext,
}

/// Type alias for our locked DRM output manager
#[allow(dead_code)] // will be used for output management
pub type LockedGbmDrmOutputManager<'a> = LockedDrmOutputManager<
    'a,
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),  // simplified - no presentation feedback yet
    DrmDeviceFd,
>;

/// Type alias for our DRM output manager
pub type GbmDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),  // simplified - no presentation feedback yet
    DrmDeviceFd,
>;

/// A DRM device with rendering capabilities
pub struct Device {
    pub drm: GbmDrmOutputManager,  // now using DrmOutputManager
    pub drm_node: DrmNode,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub allocator: Option<GbmAllocator<DrmDeviceFd>>,
    pub renderer: Option<GlowRenderer>,
    pub egl: Option<EGLInternals>,
    pub render_node: DrmNode,
    pub supports_atomic: bool,
    pub event_token: Option<RegistrationToken>,
    pub primary_node: Arc<RwLock<Option<DrmNode>>>,
    
    // track outputs and surfaces
    pub outputs: HashMap<connector::Handle, Output>,
    pub surfaces: HashMap<crtc::Handle, connector::Handle>,  // maps CRTC to connector
    pub surface_manager: super::surface::SurfaceManager,
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
    /// Schedule a render for the given output
    pub fn schedule_render(&self, output: &Output) {
        // find surfaces displaying this output and schedule render
        for surface in self.surface_manager.surfaces_for_output(output) {
            surface.schedule_render();
        }
    }
    
    /// Scan for connected outputs and create them
    pub fn scan_outputs(&mut self, display_handle: &DisplayHandle, event_loop: &LoopHandle<'static, crate::state::State>, gpu_manager: &mut GpuManager<crate::backend::render::GbmGlowBackend<DrmDeviceFd>>, shell: Arc<std::sync::RwLock<crate::shell::Shell>>) -> Result<Vec<Output>> {
        use smithay::reexports::drm::control::Device as ControlDevice;
        
        // get display configuration (connector -> CRTC mapping)  
        // we need to access the underlying DrmDevice
        let display_config = super::drm_helpers::display_configuration(self.drm.device_mut(), self.supports_atomic)?;
        
        for (conn, maybe_crtc) in display_config {
            let conn_info = match self.drm.device().get_connector(conn, false) {
                Ok(info) => info,
                Err(err) => {
                    warn!(?err, ?conn, "Failed to get connector info");
                    continue;
                }
            };
            
            if conn_info.state() == connector::State::Connected {
                let Some(crtc) = maybe_crtc else {
                    warn!("No CRTC available for connector {:?}", conn);
                    continue;
                };
                
                match create_output_for_conn(self.drm.device_mut(), conn, display_handle) {
                    Ok(output) => {
                        if let Err(err) = populate_modes(self.drm.device_mut(), &output, conn) {
                            warn!(?err, ?conn, "Failed to populate modes");
                            continue;
                        }
                        
                        let output_name = output.name();
                        info!("Detected output: {} ({}x{} @ {}Hz) on CRTC {:?}", 
                            output_name,
                            output.current_mode().map(|m| m.size.w).unwrap_or(0),
                            output.current_mode().map(|m| m.size.h).unwrap_or(0),
                            output.current_mode().map(|m| m.refresh).unwrap_or(0),
                            crtc,
                        );
                        
                        // create surface for the output
                        if let Err(err) = self.surface_manager.create_surface(
                            output.clone(), 
                            crtc, 
                            conn, 
                            self.primary_node.clone(),
                            self.render_node,
                            event_loop,
                            shell.clone(),
                        ) {
                            warn!(?err, "Failed to create surface for output");
                            continue;
                        }
                        
                        // notify the new surface about the GPU node before resuming
                        // this ensures PostprocessState can be created
                        if let (Some(surface), Some(egl_context)) = (self.surface_manager.get(&crtc), self.egl.as_ref()) {
                            use smithay::backend::egl::{context::ContextPriority, EGLContext};
                            use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags};
                            
                            let shared_ctx = EGLContext::new_shared_with_priority(
                                &egl_context.display,
                                &egl_context.context,
                                ContextPriority::High,
                            )?;
                            let allocator = GbmAllocator::new(
                                self.gbm.clone(),
                                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                            );
                            surface.add_node(self.render_node, allocator, shared_ctx);
                        }
                        
                        // create DRM compositor for the output
                        // we need to get the DRM mode, not the output mode
                        let drm_mode = conn_info.modes()[0]; // use first available mode
                        
                        // get renderer from GPU manager
                        match gpu_manager.single_renderer(&self.render_node) {
                            Ok(mut renderer) => {
                                // create render elements
                                // for now, just empty elements - the surface thread will populate them
                                let elements = smithay::backend::drm::output::DrmOutputRenderElements::<
                                    GlMultiRenderer, 
                                    CosmicElement<GlMultiRenderer>
                                >::default();
                                
                                // get planes for this CRTC
                                let planes = self.drm.device().planes(&crtc)
                                    .ok();
                                
                                // create the compositor
                                match self.drm.lock().initialize_output(
                                    crtc,
                                    drm_mode,
                                    &[conn],
                                    &output,  // use output as OutputModeSource
                                    planes,
                                    &mut renderer,
                                    &elements,
                                ) {
                                    Ok(compositor) => {
                                        debug!("Created DRM compositor for output {}", output_name);
                                        // send compositor to surface thread to start rendering
                                        if let Some(surface) = self.surface_manager.get(&crtc) {
                                            surface.resume(compositor);
                                        }
                                    }
                                    Err(err) => {
                                        warn!(?err, "Failed to create DRM compositor for output");
                                        continue;
                                    }
                                }
                            }
                            Err(err) => {
                                warn!(?err, "Failed to get renderer for creating compositor");
                                continue;
                            }
                        }
                        
                        // store output and crtc mapping
                        self.outputs.insert(conn, output);
                        self.surfaces.insert(crtc, conn);
                    }
                    Err(err) => {
                        warn!(?err, ?conn, "Failed to create output");
                    }
                }
            }
        }
        
        let outputs: Vec<Output> = self.outputs.values().cloned().collect();
        info!("Found {} connected output(s)", outputs.len());
        Ok(outputs)
    }
    /// Update EGL context and add to GPU manager when device is in use
    pub fn update_egl(
        &mut self,
        primary_node: Option<&DrmNode>,
        gpu_manager: &mut GpuManager<crate::backend::render::GbmGlowBackend<DrmDeviceFd>>,
    ) -> Result<bool> {
        // for now, consider all devices in use if they exist
        // in the future we'd check if this device has outputs
        let in_use = primary_node.is_none() || primary_node == Some(&self.drm_node);
        
        debug!("update_egl: primary_node={:?}, self.drm_node={:?}, in_use={}", 
               primary_node, self.drm_node, in_use);
        
        if in_use {
            if self.egl.is_none() {
                let egl = init_egl(&self.gbm)?;
                
                // create shared context for renderer
                let shared_context = EGLContext::new_shared_with_priority(
                    &egl.display,
                    &egl.context,
                    ContextPriority::High,
                )?;
                
                let renderer = unsafe { GlowRenderer::new(shared_context) }?;
                
                // create allocator
                let allocator = GbmAllocator::new(
                    self.gbm.clone(),
                    GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                );
                
                self.allocator = Some(allocator.clone());
                self.egl = Some(egl);
                
                // add to GPU manager's API
                info!("Adding render node {:?} to GPU manager", self.render_node);
                gpu_manager.as_mut().add_node(self.render_node, allocator, renderer);
                self.renderer = None;  // renderer is moved to the GPU manager
                
                // notify surfaces about the new GPU node
                if let Some(egl_context) = self.egl.as_ref() {
                    self.surface_manager.update_surface_nodes(
                        self.render_node,
                        &self.gbm,
                        egl_context,
                        true,  // add node
                    )?;
                }
            }
            Ok(true)
        } else {
            if self.egl.is_some() {
                // notify surfaces about the removed GPU node first
                // (before we drop the egl context)
                if let Some(egl_context) = self.egl.as_ref() {
                    let _ = self.surface_manager.update_surface_nodes(
                        self.render_node,
                        &self.gbm,
                        egl_context,
                        false,  // remove node
                    );
                }
                
                self.egl = None;
                self.allocator = None;
                self.renderer = None;
                gpu_manager.as_mut().remove_node(&self.render_node);
            }
            Ok(false)
        }
    }
    
    /// Create a new DRM device from a file descriptor
    pub fn new(
        session: &mut impl Session,
        path: &Path,
        dev: libc::dev_t,
        event_loop: &LoopHandle<'static, crate::state::State>,
        _gpu_manager: &mut GpuManager<crate::backend::render::GbmGlowBackend<DrmDeviceFd>>,
        primary_node: Arc<RwLock<Option<DrmNode>>>,
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
        let (drm_device, notifier) = DrmDevice::new(fd.clone(), false)
            .with_context(|| format!("Failed to initialize drm device for: {}", path.display()))?;
        
        let drm_node = DrmNode::from_dev_id(dev)?;
        let supports_atomic = drm_device.is_atomic();
        
        info!(
            "DRM device initialized: {:?}, atomic modesetting: {}",
            drm_node,
            supports_atomic
        );
        
        // initialize GBM for buffer allocation
        let gbm = GbmDevice::new(fd)
            .with_context(|| format!("Failed to initialize GBM device for {}", path.display()))?;
        
        // try to initialize EGL temporarily to get render formats
        let (render_node, render_formats) = match init_egl(&gbm) {
            Ok(egl) => {
                let render_node = egl
                    .device
                    .try_get_render_node()
                    .ok()
                    .and_then(std::convert::identity)
                    .unwrap_or(drm_node);
                
                // get render formats from the EGL context
                let formats = egl.context.dmabuf_texture_formats().clone();
                
                info!("EGL initialized, render node: {:?}", render_node);
                // drop the EGL context for now, we'll recreate it later if needed
                (render_node, formats)
            }
            Err(err) => {
                warn!("Failed to initialize EGL: {}", err);
                (drm_node, Default::default())
            }
        };
        
        // create allocator for the DrmOutputManager
        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        
        // create framebuffer exporter
        let fb_exporter = GbmFramebufferExporter::new(
            gbm.clone(),
            render_node.into(),
        );
        
        // create DrmOutputManager
        let drm = DrmOutputManager::new(
            drm_device,
            allocator.clone(),
            fb_exporter,
            Some(gbm.clone()),
            // supported color formats
            [
                Fourcc::Abgr8888,
                Fourcc::Argb8888,
                Fourcc::Xbgr8888,
                Fourcc::Xrgb8888,
            ],
            render_formats,
        );
        
        // register DRM event handler
        let token = event_loop
            .insert_source(notifier, move |event, _metadata, _state| {
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
            allocator: Some(allocator),
            renderer: None,   // will be created when device is used
            egl: None,        // will be created when device is used
            render_node,
            supports_atomic,
            event_token: Some(token),
            primary_node,
            outputs: HashMap::new(),
            surfaces: HashMap::new(),
            surface_manager: super::surface::SurfaceManager::new(),
        })
    }
}

/// Create an output for a DRM connector
fn create_output_for_conn(drm: &mut DrmDevice, conn: connector::Handle, display_handle: &DisplayHandle) -> Result<Output> {
    use smithay::reexports::drm::control::Device as ControlDevice;
    
    let conn_info = drm
        .get_connector(conn, false)
        .with_context(|| "Failed to query connector info")?;
    let interface = super::drm_helpers::interface_name(drm, conn)?;
    let edid_info = super::drm_helpers::edid_info(drm, conn)
        .inspect_err(|err| warn!(?err, "failed to get EDID for {}", interface))
        .ok();
    let (phys_w, phys_h) = conn_info.size().unwrap_or((0, 0));

    let output = Output::new(
        interface,
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: match conn_info.subpixel() {
                connector::SubPixel::HorizontalRgb => Subpixel::HorizontalRgb,
                connector::SubPixel::HorizontalBgr => Subpixel::HorizontalBgr,
                connector::SubPixel::VerticalRgb => Subpixel::VerticalRgb,
                connector::SubPixel::VerticalBgr => Subpixel::VerticalBgr,
                connector::SubPixel::None => Subpixel::None,
                _ => Subpixel::Unknown,
            },
            make: edid_info
                .as_ref()
                .and_then(|info| info.make())
                .unwrap_or_else(|| String::from("Unknown")),
            model: edid_info
                .as_ref()
                .and_then(|info| info.model())
                .unwrap_or_else(|| String::from("Unknown")),
            serial_number: edid_info
                .as_ref()
                .and_then(|info| info.serial())
                .unwrap_or_else(|| String::from("Unknown")),
        },
    );
    
    // Create the global to advertise this output to Wayland clients
    let _global = output.create_global::<crate::state::State>(display_handle);
    tracing::info!("Created wl_output global for {}", output.name());
    
    Ok(output)
}

/// Populate available modes for an output
fn populate_modes(
    drm: &mut DrmDevice,
    output: &Output,
    conn: connector::Handle,
) -> Result<()> {
    use smithay::reexports::drm::control::Device as ControlDevice;
    
    let conn_info = drm.get_connector(conn, false)?;
    let Some(mode) = conn_info
        .modes()
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .copied()
        .or(conn_info.modes().get(0).copied())
    else {
        anyhow::bail!("No mode found");
    };

    let refresh_rate = super::drm_helpers::calculate_refresh_rate(mode);
    let output_mode = OutputMode {
        size: (mode.size().0 as i32, mode.size().1 as i32).into(),
        refresh: refresh_rate as i32,
    };

    // Add all available modes
    let mut modes = Vec::new();
    for mode in conn_info.modes() {
        let refresh_rate = super::drm_helpers::calculate_refresh_rate(*mode);
        let mode = OutputMode {
            size: (mode.size().0 as i32, mode.size().1 as i32).into(),
            refresh: refresh_rate as i32,
        };
        modes.push(mode.clone());
        output.add_mode(mode);
    }
    
    // Remove any modes that no longer exist
    for mode in output
        .modes()
        .into_iter()
        .filter(|mode| !modes.contains(&mode))
    {
        output.delete_mode(mode);
    }
    output.set_preferred(output_mode);

    // Set initial configuration
    let scale = 1.0; // simplified - cosmic-comp has complex scale calculation
    let transform = Transform::Normal; // simplified - cosmic-comp reads panel orientation
    output.change_current_state(
        Some(output_mode),
        Some(transform),
        Some(Scale::Fractional(scale)),
        Some(Point::from((0, 0))), // simplified - cosmic-comp calculates position
    );

    Ok(())
}