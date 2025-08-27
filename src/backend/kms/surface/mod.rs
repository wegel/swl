// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmDevice},
            format::FormatSet,
            Fourcc,
        },
        drm::{
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::DrmOutput,
            DrmDeviceFd, DrmNode,
        },
        egl::EGLContext,
        renderer::{
            damage::{OutputDamageTracker, Error as RenderError},
            element::{
                solid::SolidColorRenderElement, 
                texture::{TextureRenderBuffer, TextureRenderElement},
                Kind,
            },
            glow::GlowRenderer,
            gles::GlesTexture,
            multigpu::GpuManager,
            Bind, Renderer, Offscreen, Texture,
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
    utils::{Clock, Monotonic, Rectangle, Physical, Transform},
    wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder},
};

use crate::backend::render::{
    element::{AsGlowRenderer, CosmicElement},
    GlMultiRenderer,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};
use tracing::{debug, error, info, trace};

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
#[allow(dead_code)] // variants will be used when we connect the render loop
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

/// Simplified PostprocessState for offscreen rendering
/// Based on cosmic-comp's approach but simplified for our needs
struct PostprocessState {
    texture: TextureRenderBuffer<GlesTexture>,
    damage_tracker: OutputDamageTracker,
}

impl PostprocessState {
    /// Create a new PostprocessState with offscreen texture
    fn new_with_renderer<R>(
        renderer: &mut R,
        format: Fourcc,
        output: &Output,
    ) -> Result<Self>
    where
        R: AsGlowRenderer + Offscreen<GlesTexture>,
    {
        let mode = output.current_mode()
            .ok_or_else(|| anyhow::anyhow!("Output has no mode"))?;
        
        let size = mode.size;
        let scale = output.current_scale().integer_scale();
        let transform = output.current_transform();
        let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
        let opaque_regions = vec![Rectangle::from_size(buffer_size)];
        
        // create offscreen texture
        let texture = Offscreen::<GlesTexture>::create_buffer(
            renderer,
            format,
            buffer_size,
        ).map_err(|e| anyhow::anyhow!("Failed to create buffer: {:?}", e))?;
        
        // create texture render buffer
        let texture_buffer = TextureRenderBuffer::from_texture(
            renderer.glow_renderer(),
            texture,
            scale,
            transform,
            Some(opaque_regions),
        );
        
        // create damage tracker (without output transform to match texture)
        let damage_tracker = OutputDamageTracker::new(
            size,
            output.current_scale().fractional_scale(),
            Transform::Normal,  // no transform for offscreen buffer
        );
        
        Ok(PostprocessState {
            texture: texture_buffer,
            damage_tracker,
        })
    }
}

/// Queue state for frame scheduling
#[derive(Debug)]
#[allow(dead_code)] // Queued variant will be used for frame timing
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
#[allow(dead_code)] // fields will be used in later phases
struct SurfaceThreadState {
    // rendering
    api: GpuManager<crate::backend::render::GbmGlowBackend<DrmDeviceFd>>,
    primary_node: Arc<RwLock<Option<DrmNode>>>,
    target_node: DrmNode,
    active: Arc<AtomicBool>,
    compositor: Option<GbmDrmOutput>,
    
    // offscreen rendering and damage tracking
    postprocess: Option<PostprocessState>,
    last_frame_damage: Option<Vec<Rectangle<i32, Physical>>>,
    
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
#[allow(dead_code)] // will be used for dmabuf optimization
pub struct SurfaceDmabufFeedback {
    pub render_feedback: DmabufFeedback,
    pub scanout_feedback: DmabufFeedback,
}

/// Surface with render thread
#[allow(dead_code)] // fields will be used as we implement more features
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
    #[allow(dead_code)] // will be used when we connect the render loop
    pub fn schedule_render(&self) {
        debug!("Render scheduled for output {}", self.output.name());
        let _ = self.thread_command.send(ThreadCommand::ScheduleRender);
    }
    
    /// Resume the surface with a compositor
    #[allow(dead_code)] // will be used when we connect the render loop
    pub fn resume(&self, compositor: GbmDrmOutput) {
        info!("Resuming surface for output {}", self.output.name());
        self.active.store(true, Ordering::SeqCst);
        let _ = self.thread_command.send(ThreadCommand::Resume { compositor });
    }
    
    /// Handle VBlank event
    #[allow(dead_code)] // will be used for vblank handling
    pub fn on_vblank(&self) {
        let _ = self.thread_command.send(ThreadCommand::VBlank);
    }
    
    /// Check if the surface is active
    #[allow(dead_code)] // will be used for state queries
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
    #[allow(dead_code)] // will be used for dmabuf optimization
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
        postprocess: None,
        last_frame_damage: None,
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
        
        // create PostprocessState if not already done
        if self.postprocess.is_none() && self.output.current_mode().is_some() {
            let format = compositor.format();
            
            // get renderer for creating postprocess state
            match self.api.single_renderer(&self.target_node) {
                Ok(mut renderer) => {
                    match PostprocessState::new_with_renderer(&mut renderer, format, &self.output) {
                        Ok(state) => {
                            self.postprocess = Some(state);
                            debug!("Created PostprocessState for {}", self.output.name());
                        }
                        Err(e) => {
                            error!("Failed to create PostprocessState: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to get renderer for PostprocessState: {:?}", e);
                }
            }
        }
        
        self.compositor = Some(compositor);
        self.queue_redraw();
    }
    
    /// Select the appropriate render node for the output
    /// simplified version - just uses primary or target node
    #[allow(dead_code)] // used in redraw method
    fn render_node_for_output(&self) -> DrmNode {
        // if we have a primary node set, use it; otherwise use target
        self.primary_node
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or(self.target_node)
    }
    
    
    fn queue_redraw(&mut self) {
        // Phase 2jd: Hook up redraw() to be called
        // Phase 2k will add proper timing, for now just call directly
        if self.compositor.is_some() {
            match self.state {
                QueueState::Idle => {
                    debug!("Queueing redraw for {}", self.output.name());
                    // call redraw immediately for now
                    // Phase 2k will add proper timer scheduling
                    if let Err(err) = self.redraw() {
                        error!("Failed to redraw: {}", err);
                    }
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
        // Phase 2jd: VBlank handler calls queue_redraw if needed
        match &self.state {
            QueueState::WaitingForVBlank { redraw_needed } => {
                if *redraw_needed {
                    // another redraw was requested while waiting
                    self.state = QueueState::Idle;
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
    
    /// Perform a redraw with damage tracking using PostprocessState
    fn redraw(&mut self) -> Result<()> {
        // check we have a compositor first
        if self.compositor.is_none() {
            return Ok(());
        }
        
        // check we have postprocess state
        if self.postprocess.is_none() {
            error!("No postprocess state for output {}", self.output.name());
            return Ok(());
        }
        
        // for now, just render a clear color
        // Phase 2m will add actual element rendering
        let elements: Vec<SolidColorRenderElement> = Vec::new();
        
        // get format and render node before mutable borrows
        let format = self.compositor.as_ref().unwrap().format();
        let render_node = self.render_node_for_output();
        
        // get appropriate renderer
        let mut renderer = if render_node != self.target_node {
            // multi-gpu case
            self.api.renderer(&render_node, &self.target_node, format)
                .map_err(|e| anyhow::anyhow!("Failed to get multi-gpu renderer: {}", e))?
        } else {
            // single-gpu case
            self.api.single_renderer(&self.target_node)
                .map_err(|e| anyhow::anyhow!("Failed to get single-gpu renderer: {}", e))?
        };
        
        // Phase 2jb: Render to offscreen texture using PostprocessState
        // This follows cosmic-comp's approach exactly
        // Use the already obtained renderer for texture operations  
        let postprocess = self.postprocess.as_mut().unwrap();
        let transform = self.output.current_transform();
        
        let _damage = postprocess.texture.render()
            .draw(|texture| {
                // bind the texture as our render target
                let mut fb = renderer.bind(texture)
                    .map_err(|e| anyhow::anyhow!("Failed to bind texture: {:?}", e))?;
                
                // buffer age tells us how many frames ago this buffer was last used
                // for offscreen textures, we use age 1 (always redraw everything for now)
                let age = 1; // Phase 2je will track this properly
                
                // use OutputDamageTracker to render with damage tracking
                let res = match postprocess.damage_tracker.render_output(
                    &mut renderer,
                    &mut fb,
                    age,
                    &elements,
                    crate::backend::render::CLEAR_COLOR,
                ) {
                    Ok(res) => res,
                    Err(RenderError::Rendering(err)) => return Err(anyhow::anyhow!("Render error: {:?}", err)),
                    Err(RenderError::OutputNoMode(_)) => unreachable!("Output has mode"),
                };
                
                // wait for rendering to complete
                renderer.wait(&res.sync)
                    .map_err(|e| anyhow::anyhow!("Failed to wait for sync: {:?}", e))?;
                
                // unbind the texture
                std::mem::drop(fb);
                
                // return damage regions
                let area = texture.size().to_logical(1, transform);
                
                Ok(res.damage
                    .cloned()
                    .map(|v| {
                        v.into_iter()
                            .map(|r| r.to_logical(1).to_buffer(1, transform, &area))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default())
            })
            .context("Failed to draw to offscreen render target")?;
        
        // Phase 2jc: Composite the offscreen texture to the display
        // Create a texture element from our offscreen buffer
        // This is a simplified version of cosmic-comp's postprocess_elements()
        let texture_element = TextureRenderElement::from_texture_render_buffer(
            (0.0, 0.0),  // location at origin
            &postprocess.texture,
            None,  // no alpha
            None,  // no src crop
            None,  // no size override
            Kind::Unspecified,
        );
        
        // wrap in CosmicElement for proper rendering
        let elements: Vec<CosmicElement<GlMultiRenderer>> = vec![
            CosmicElement::Texture(texture_element)
        ];
        
        // use the multi-gpu renderer to present the composited texture
        let frame_result = self.compositor.as_mut().unwrap().render_frame(
            &mut renderer,
            &elements,
            [0.0, 0.0, 0.0, 0.0],  // black background (already rendered in texture)
            FrameFlags::empty(),
        ).map_err(|e| anyhow::anyhow!("Frame render failed: {:?}", e))?;
        
        // queue the frame for presentation if there's content
        if !frame_result.is_empty {
            // store damage for next frame (currently empty)
            self.last_frame_damage = Some(Vec::new());  // Phase 2je will track properly
            
            // queue for presentation (Phase 2k will add proper timing)
            self.compositor.as_mut().unwrap().queue_frame(())
                .map_err(|e| anyhow::anyhow!("Failed to queue frame: {:?}", e))?;
            
            // update state to wait for vblank
            self.state = QueueState::WaitingForVBlank {
                redraw_needed: false,
            };
            
            trace!("Frame queued for output {}, damage regions: {}", 
                self.output.name(), 
                self.last_frame_damage.as_ref().map(|d| d.len()).unwrap_or(0)
            );
        } else {
            trace!("Empty frame for output {}, skipping", self.output.name());
        }
        
        Ok(())
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