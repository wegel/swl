// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmDevice},
            format::FormatSet,
        },
        drm::{
            exporter::gbm::GbmFramebufferExporter,
            output::DrmOutput,
            DrmDeviceFd, DrmNode,
        },
        egl::EGLContext,
        renderer::{
            glow::GlowRenderer,
            multigpu::GpuManager,
        },
    },
    output::Output,
    reexports::{
        calloop::{
            channel::{channel, Channel, Event, Sender},
            LoopHandle, RegistrationToken,
        },
        drm::control::{connector, crtc},
    },
    utils::{Clock, Monotonic},
    wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder},
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};
use tracing::{debug, error, info};

/// Type alias for our DRM output - following cosmic-comp's definition
/// Simplified version without presentation feedback for now
pub type GbmDrmOutput = DrmOutput<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),  // simplified - no presentation feedback yet (cosmic-comp has complex feedback)
    DrmDeviceFd,
>;

/// Commands sent to the surface render thread
#[derive(Debug)]
pub enum ThreadCommand {
    /// Resume rendering with the given compositor
    Resume {
        compositor: GbmDrmOutput,
    },
    /// Add a GPU node for rendering
    NodeAdded {
        node: DrmNode,
        gbm: GbmAllocator<DrmDeviceFd>,
        egl: EGLContext,
    },
    /// Remove a GPU node
    NodeRemoved {
        node: DrmNode,
    },
    /// Schedule a render frame
    ScheduleRender,
    /// VBlank event occurred
    VBlank,
    /// End the thread
    End,
}

/// Commands sent from surface thread back to main thread
#[derive(Debug)]
pub enum SurfaceCommand {
    // placeholder for now - we'll add commands as needed
}

/// Queue state for frame scheduling
#[derive(Debug)]
pub enum QueueState {
    /// No render queued
    Idle,
    /// A render is queued
    Queued(RegistrationToken),
    /// Waiting for VBlank
    WaitingForVBlank {
        redraw_needed: bool,
    },
}

impl Default for QueueState {
    fn default() -> Self {
        QueueState::Idle
    }
}

/// State for the surface render thread
struct SurfaceThreadState {
    // rendering
    api: GpuManager<crate::backend::render::GbmGlowBackend<DrmDeviceFd>>,
    primary_node: Arc<RwLock<Option<DrmNode>>>,
    target_node: DrmNode,
    active: Arc<AtomicBool>,
    compositor: Option<GbmDrmOutput>,
    
    // scheduling
    state: QueueState,
    thread_sender: Sender<SurfaceCommand>,
    
    // output info
    output: Output,
    
    // event loop
    loop_handle: LoopHandle<'static, Self>,
    clock: Clock<Monotonic>,
}

/// Dmabuf feedback for a surface
#[derive(Debug, Clone)]
pub struct SurfaceDmabufFeedback {
    pub render_feedback: DmabufFeedback,
    pub scanout_feedback: DmabufFeedback,
}

/// Surface with render thread
pub struct Surface {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub output: Output,
    pub primary_plane_formats: FormatSet,
    pub dmabuf_feedback: Option<SurfaceDmabufFeedback>,
    
    // threading support
    active: Arc<AtomicBool>,
    thread_command: Sender<ThreadCommand>,
    thread_token: RegistrationToken,
}

impl Surface {
    pub fn new(
        output: Output,
        crtc: crtc::Handle,
        connector: connector::Handle,
        primary_node: Arc<RwLock<Option<DrmNode>>>,
        target_node: DrmNode,
        event_loop: &LoopHandle<'static, crate::state::State>,
    ) -> Result<Self> {
        info!("Creating surface for output {} on CRTC {:?}", output.name(), crtc);
        
        // create channels for thread communication
        let (tx, rx) = channel::<ThreadCommand>();
        let (tx2, rx2) = channel::<SurfaceCommand>();
        let active = Arc::new(AtomicBool::new(false));
        
        let active_clone = active.clone();
        let output_clone = output.clone();
        
        // spawn the render thread
        std::thread::Builder::new()
            .name(format!("surface-{}", output.name()))
            .spawn(move || {
                if let Err(err) = surface_thread(
                    output_clone,
                    primary_node,
                    target_node,
                    active_clone,
                    tx2,
                    rx,
                ) {
                    error!("Surface thread crashed: {}", err);
                }
            })
            .context("Failed to spawn surface thread")?;
        
        // register channel to receive commands from surface thread
        let thread_token = event_loop
            .insert_source(rx2, move |command, _, _state| match command {
                Event::Msg(_cmd) => {
                    // handle surface commands from thread
                    // we'll add handling as needed
                }
                Event::Closed => {}
            })
            .map_err(|_| anyhow::anyhow!("Failed to establish channel to surface thread"))?;
        
        Ok(Self {
            connector,
            crtc,
            output,
            primary_plane_formats: FormatSet::default(),
            dmabuf_feedback: None,
            active,
            thread_command: tx,
            thread_token,
        })
    }
    
    /// Schedule a render for this surface
    pub fn schedule_render(&self) {
        debug!("Render scheduled for output {}", self.output.name());
        let _ = self.thread_command.send(ThreadCommand::ScheduleRender);
    }
    
    /// Resume the surface with a compositor
    pub fn resume(&self, compositor: GbmDrmOutput) {
        info!("Resuming surface for output {}", self.output.name());
        self.active.store(true, Ordering::SeqCst);
        let _ = self.thread_command.send(ThreadCommand::Resume { compositor });
    }
    
    /// Handle VBlank event
    pub fn on_vblank(&self) {
        let _ = self.thread_command.send(ThreadCommand::VBlank);
    }
    
    /// Check if the surface is active
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    
    /// Add a GPU node to the surface thread
    pub fn add_node(&self, node: DrmNode, gbm: GbmAllocator<DrmDeviceFd>, egl: EGLContext) {
        info!("Adding GPU node {:?} to surface {}", node, self.output.name());
        let _ = self.thread_command.send(ThreadCommand::NodeAdded { node, gbm, egl });
    }
    
    /// Remove a GPU node from the surface thread
    pub fn remove_node(&self, node: DrmNode) {
        info!("Removing GPU node {:?} from surface {}", node, self.output.name());
        let _ = self.thread_command.send(ThreadCommand::NodeRemoved { node });
    }
    
    /// Update dmabuf feedback based on current formats
    pub fn update_dmabuf_feedback(&mut self, render_node: DrmNode, render_formats: FormatSet) {
        // simplified dmabuf feedback - just basic render and scanout tranches
        // cosmic-comp has more sophisticated logic for multi-gpu scenarios
        
        let builder = DmabufFeedbackBuilder::new(render_node.dev_id(), render_formats.clone());
        
        // build basic render feedback
        let render_feedback = builder.clone().build().unwrap();
        
        // build scanout feedback with primary plane formats if available
        let scanout_feedback = if !self.primary_plane_formats.iter().next().is_none() {
            builder
                .add_preference_tranche(
                    render_node.dev_id(),
                    None,  // no specific flags for now
                    self.primary_plane_formats.clone(),
                )
                .build()
                .unwrap()
        } else {
            render_feedback.clone()
        };
        
        self.dmabuf_feedback = Some(SurfaceDmabufFeedback {
            render_feedback,
            scanout_feedback,
        });
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        let _ = self.thread_command.send(ThreadCommand::End);
        // thread_token will be removed by the event loop
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
        primary_node: Arc<RwLock<Option<DrmNode>>>,
        target_node: DrmNode,
        event_loop: &LoopHandle<'static, crate::state::State>,
    ) -> Result<()> {
        let surface = Surface::new(output, crtc, connector, primary_node, target_node, event_loop)?;
        self.surfaces.insert(crtc, surface);
        debug!("Surface created for CRTC {:?}", crtc);
        Ok(())
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
    
    /// Update GPU nodes for all surfaces
    pub fn update_surface_nodes(
        &mut self,
        node: DrmNode,
        gbm: &GbmDevice<DrmDeviceFd>,
        egl: &crate::backend::kms::device::EGLInternals,
        add: bool,
    ) -> Result<()> {
        use smithay::backend::egl::{context::ContextPriority, EGLContext};
        use smithay::backend::allocator::{gbm::{GbmAllocator, GbmBufferFlags}};
        
        for surface in self.surfaces.values_mut() {
            if add {
                // create a new shared context for this surface
                let shared_ctx = EGLContext::new_shared_with_priority(
                    &egl.display,
                    &egl.context,
                    ContextPriority::High,
                )?;
                let allocator = GbmAllocator::new(
                    gbm.clone(),
                    GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                );
                surface.add_node(node.clone(), allocator, shared_ctx);
            } else {
                surface.remove_node(node.clone());
            }
        }
        Ok(())
    }
}

/// Surface render thread
fn surface_thread(
    output: Output,
    primary_node: Arc<RwLock<Option<DrmNode>>>,
    target_node: DrmNode,
    active: Arc<AtomicBool>,
    thread_sender: Sender<SurfaceCommand>,
    thread_receiver: Channel<ThreadCommand>,
) -> Result<()> {
    let name = output.name();
    info!("Starting surface thread for {}", name);
    
    // create event loop for this thread
    let mut event_loop = smithay::reexports::calloop::EventLoop::try_new()
        .context("Failed to create surface thread event loop")?;
    
    // initialize GPU manager for this thread
    let api = GpuManager::new(crate::backend::render::GbmGlowBackend::new())
        .context("Failed to initialize rendering api")?;
    
    // get stop signal for the event loop
    let signal = event_loop.get_signal();
    
    let mut state = SurfaceThreadState {
        api,
        primary_node,
        target_node,
        active,
        compositor: None,
        state: QueueState::Idle,
        thread_sender,
        output,
        loop_handle: event_loop.handle(),
        clock: Clock::new(),
    };
    
    // register command handler
    event_loop
        .handle()
        .insert_source(thread_receiver, move |command, _, _state| match command {
            Event::Msg(ThreadCommand::Resume { compositor }) => {
                _state.resume(compositor);
            }
            Event::Msg(ThreadCommand::NodeAdded { node, gbm, egl }) => {
                if let Err(err) = _state.node_added(node, gbm, egl) {
                    tracing::warn!(?err, ?node, "Failed to add node to surface thread");
                }
            }
            Event::Msg(ThreadCommand::NodeRemoved { node }) => {
                _state.node_removed(node);
            }
            Event::Msg(ThreadCommand::ScheduleRender) => {
                _state.queue_redraw();
            }
            Event::Msg(ThreadCommand::VBlank) => {
                _state.on_vblank();
            }
            Event::Msg(ThreadCommand::End) => {
                signal.stop();
            }
            Event::Closed => {
                signal.stop();
            }
        })
        .map_err(|e| anyhow::anyhow!("Failed to insert command source: {}", e))?;
    
    // run the event loop
    event_loop.run(None, &mut state, |_| {})?;
    
    info!("Surface thread for {} ending", name);
    Ok(())
}

impl SurfaceThreadState {
    fn resume(&mut self, compositor: GbmDrmOutput) {
        debug!("Resuming surface {}", self.output.name());
        self.compositor = Some(compositor);
        self.queue_redraw();
    }
    
    /// Select the appropriate render node for the output
    /// simplified version - just uses primary or target node
    fn render_node_for_output(&self) -> DrmNode {
        // if we have a primary node set, use it; otherwise use target
        self.primary_node
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or(self.target_node)
    }
    
    /// Get renderer for the selected node
    fn get_renderer(&mut self) -> Option<smithay::backend::renderer::multigpu::MultiRenderer<'_, '_, crate::backend::render::GbmGlowBackend<DrmDeviceFd>, crate::backend::render::GbmGlowBackend<DrmDeviceFd>>> {
        let render_node = self.render_node_for_output();
        
        // get compositor format if available
        let format = self.compositor.as_ref().map(|c| c.format());
        
        if render_node != self.target_node && format.is_some() {
            // multi-gpu case: need to render on one GPU and display on another
            self.api.renderer(&render_node, &self.target_node, format.unwrap())
                .map_err(|e| {
                    error!("Failed to get multi-gpu renderer: {}", e);
                    e
                })
                .ok()
        } else {
            // single-gpu case: render and display on same GPU
            self.api.single_renderer(&self.target_node)
                .map_err(|e| {
                    error!("Failed to get single-gpu renderer: {}", e);
                    e
                })
                .ok()
        }
    }
    
    fn queue_redraw(&mut self) {
        // simplified version - just mark as needing redraw
        // we'll implement actual timing in Phase 2k
        if self.compositor.is_some() {
            match self.state {
                QueueState::Idle => {
                    debug!("Queueing redraw for {}", self.output.name());
                    // in Phase 2k, we'll schedule a timer here
                    // for now, just transition to waiting state
                    self.state = QueueState::WaitingForVBlank {
                        redraw_needed: false,
                    };
                }
                QueueState::WaitingForVBlank { .. } => {
                    self.state = QueueState::WaitingForVBlank {
                        redraw_needed: true,
                    };
                }
                _ => {}
            }
        }
    }
    
    fn on_vblank(&mut self) {
        match &self.state {
            QueueState::WaitingForVBlank { redraw_needed } => {
                if *redraw_needed {
                    self.queue_redraw();
                } else {
                    self.state = QueueState::Idle;
                }
            }
            _ => {
                self.state = QueueState::Idle;
            }
        }
    }
    
    fn node_added(
        &mut self,
        node: DrmNode,
        gbm: GbmAllocator<DrmDeviceFd>,
        egl: EGLContext,
    ) -> Result<()> {
        // create glow renderer from EGL context
        let renderer = unsafe { GlowRenderer::new(egl) }
            .context("Failed to create glow renderer")?;
        
        // add the node to the GPU manager
        self.api.as_mut().add_node(node, gbm, renderer);
        
        Ok(())
    }
    
    fn node_removed(&mut self, node: DrmNode) {
        self.api.as_mut().remove_node(&node);
    }
}