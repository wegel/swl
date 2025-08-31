// SPDX-License-Identifier: GPL-3.0-only

mod timings;

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
            DrmDeviceFd, DrmNode, DrmEventMetadata, DrmEventTime, VrrSupport,
        },
        egl::EGLContext,
        renderer::{
            damage::{OutputDamageTracker, Error as RenderError},
            element::{
                texture::{TextureRenderBuffer, TextureRenderElement},
                Kind, RenderElementStates,
            },
            glow::GlowRenderer,
            gles::GlesTexture,
            multigpu::GpuManager,
            Bind, Renderer, Offscreen, Texture,
        },
    },
    desktop::utils::OutputPresentationFeedback,
    output::Output,
    reexports::{
        calloop::{
            channel::{channel, Channel, Event, Sender},
            timer::{Timer, TimeoutAction},
            LoopHandle, RegistrationToken,
        },
        drm::control::{connector, crtc},
    },
    utils::{Clock, Monotonic, Rectangle, Size, Transform},
    wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder},
};

use crate::{
    backend::render::{
        cursor,
        element::{AsGlowRenderer, CosmicElement},
        GlMultiRenderer,
    },
    shell::Shell,
};
use self::timings::Timings;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use tracing::{debug, error, info, trace, warn};

/// Type alias for our DRM output
/// Now properly configured with presentation feedback support
pub type GbmDrmOutput = DrmOutput<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,  // presentation feedback for frame timing
    DrmDeviceFd,
>;

/// Adaptive sync (VRR) configuration modes
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AdaptiveSync {
    /// Never use VRR
    Disabled,
    /// Use VRR for fullscreen content
    Enabled,
    /// Always use VRR (testing)
    Force,
}

impl Default for AdaptiveSync {
    fn default() -> Self {
        AdaptiveSync::Disabled
    }
}

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
    /// Mark structural changes (windows added/removed/moved)
    /// VBlank event occurred
    VBlank(Option<DrmEventMetadata>),
    /// Check if adaptive sync is available
    AdaptiveSyncAvailable(std::sync::mpsc::SyncSender<Result<Option<VrrSupport>>>),
    /// Set adaptive sync mode
    UseAdaptiveSync(AdaptiveSync),
    /// Render element states from a successful render
    RenderStates(RenderElementStates),
    /// End the thread
    End,
}

/// Commands sent from surface thread back to main thread
#[derive(Debug)]
pub enum SurfaceCommand {
    /// Render states from a successful render
    RenderStates(RenderElementStates),
}

/// Simplified PostprocessState for offscreen rendering
/// Based on cosmic-comp's approach but simplified for our needs
struct PostprocessState {
    texture: TextureRenderBuffer<GlesTexture>,
    damage_tracker: OutputDamageTracker,
    // Phase 5d: Multi-buffer support for proper damage tracking
    // TODO: Replace single texture with array of 2-3 textures
    // buffer_index: usize,
    // textures: Vec<TextureRenderBuffer<GlesTexture>>,
    // buffer_ages: Vec<usize>,
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

/// Plane assignment for hardware composition
// will be used in Phase 4i: Hardware Plane Optimization
#[allow(dead_code)]
#[derive(Debug)]
pub struct PlaneAssignment {
    pub element_index: usize,
    pub plane_type: PlaneType,
}

// will be used in Phase 4i: Hardware Plane Optimization
#[allow(dead_code)]
#[derive(Debug)]
pub enum PlaneType {
    Primary,
    Overlay,
    Cursor,
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
    /// We did not submit anything to KMS and made a timer to fire at the estimated VBlank
    WaitingForEstimatedVBlank(RegistrationToken),
    /// A redraw is queued on top of the above
    WaitingForEstimatedVBlankAndQueued {
        estimated_vblank: RegistrationToken,
        queued_render: RegistrationToken,
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
    last_frame_damage: Option<Vec<Rectangle<i32, smithay::utils::Buffer>>>,
    frame_count: u32,  // track frame count for buffer age
    
    // scheduling
    state: QueueState,
    thread_sender: Sender<SurfaceCommand>,
    timings: Timings,
    
    // adaptive sync
    vrr_mode: AdaptiveSync,
    
    // output info
    output: Output,
    
    // shell reference for element collection
    shell: Arc<RwLock<Shell>>,
    
    // seat for cursor state access
    seat: smithay::input::Seat<crate::State>,
    
    // event loop
    loop_handle: LoopHandle<'static, Self>,
    clock: Clock<Monotonic>,
    
    // frame callback sequence number to prevent empty-damage commit busy loops
    frame_callback_seq: usize,
    
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
        shell: Arc<RwLock<Shell>>,
        seat: smithay::input::Seat<crate::State>,
    ) -> Result<Self> {
        info!("Creating surface for output {} on CRTC {:?}", output.name(), crtc);
        
        // create channels for thread communication
        let (tx, rx) = channel::<ThreadCommand>();
        let (tx2, rx2) = channel::<SurfaceCommand>();
        let active = Arc::new(AtomicBool::new(false));
        
        let active_clone = active.clone();
        let output_clone = output.clone();
        let shell_clone = shell.clone();
        
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
                    shell_clone,
                    seat,
                ) {
                    error!("Surface thread crashed: {}", err);
                }
            })
            .context("Failed to spawn surface thread")?;
        
        // register channel to receive commands from surface thread
        let output_for_handler = output.clone();
        let thread_token = event_loop
            .insert_source(rx2, move |command, _, state| match command {
                Event::Msg(cmd) => {
                    match cmd {
                        SurfaceCommand::RenderStates(render_states) => {
                            // update primary output and fractional scale for all surfaces
                            state.update_primary_output(&output_for_handler, &render_states);
                        }
                    }
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
    #[allow(dead_code)] // will be used for vblank handling
    pub fn on_vblank(&self, metadata: Option<DrmEventMetadata>) {
        let _ = self.thread_command.send(ThreadCommand::VBlank(metadata));
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
    
    /// Check if adaptive sync (VRR) is supported on this output
    // currently unused but may be exposed to clients in the future
    #[allow(dead_code)]
    pub fn adaptive_sync_support(&self) -> Result<Option<VrrSupport>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let _ = self
            .thread_command
            .send(ThreadCommand::AdaptiveSyncAvailable(tx));
        rx.recv().context("Surface thread died")?
    }
    
    /// Set adaptive sync mode for this surface
    // currently unused but may be exposed for runtime VRR mode changes
    #[allow(dead_code)]
    pub fn use_adaptive_sync(&mut self, vrr: AdaptiveSync) {
        let _ = self
            .thread_command
            .send(ThreadCommand::UseAdaptiveSync(vrr));
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
        shell: Arc<RwLock<Shell>>,
        seat: smithay::input::Seat<crate::State>,
    ) -> Result<()> {
        let surface = Surface::new(output, crtc, connector, primary_node, target_node, event_loop, shell, seat)?;
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
    
    /// Get all surfaces displaying the given output
    pub fn surfaces_for_output(&self, output: &Output) -> impl Iterator<Item = &Surface> {
        self.surfaces.values()
            .filter(move |s| &s.output == output)
    }
    
    /// Forward VBlank event to the appropriate surface
    pub fn on_vblank(&self, crtc: crtc::Handle, metadata: Option<DrmEventMetadata>) {
        if let Some(surface) = self.surfaces.get(&crtc) {
            surface.on_vblank(metadata);
        }
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
    shell: Arc<RwLock<Shell>>,
    seat: smithay::input::Seat<crate::State>,
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
    
    let clock = Clock::new();
    
    // initialize frame timings (will be properly set in resume())
    // use None initially since we don't have the real DRM mode yet
    let timings = Timings::new(None, None, false, target_node.clone());
    
    let mut state = SurfaceThreadState {
        api,
        primary_node,
        target_node,
        active,
        compositor: None,
        postprocess: None,
        last_frame_damage: None,
        frame_count: 0,
        state: QueueState::Idle,
        thread_sender,
        timings,
        // allow overriding VRR mode via environment variable for testing
        vrr_mode: {
            let mode = std::env::var("SWL_VRR_MODE")
                .ok()
                .and_then(|mode| match mode.as_str() {
                    "force" | "Force" | "FORCE" => Some(AdaptiveSync::Force),
                    "enabled" | "Enabled" | "ENABLED" => Some(AdaptiveSync::Enabled),
                    "disabled" | "Disabled" | "DISABLED" => Some(AdaptiveSync::Disabled),
                    _ => {
                        warn!("Invalid SWL_VRR_MODE value: {}", mode);
                        None
                    }
                })
                .unwrap_or(AdaptiveSync::Enabled); // default to Enabled (opportunistic VRR)
            debug!("VRR mode for {}: {:?}", output.name(), mode);
            mode
        },
        output,
        shell,
        seat,
        frame_callback_seq: 0,
        loop_handle: event_loop.handle(),
        clock,
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
            Event::Msg(ThreadCommand::VBlank(metadata)) => {
                _state.on_vblank(metadata);
            }
            Event::Msg(ThreadCommand::AdaptiveSyncAvailable(result)) => {
                if let Some(compositor) = _state.compositor.as_mut() {
                    let _ = result.send(
                        compositor
                            .with_compositor(|c| {
                                c.vrr_supported(c.pending_connectors().into_iter().next().unwrap())
                            })
                            .map(Some)
                            .map_err(|e| anyhow::anyhow!("Failed to check VRR support: {}", e))
                    );
                } else {
                    let _ = result.send(Err(anyhow::anyhow!("Set vrr with inactive surface")));
                }
            }
            Event::Msg(ThreadCommand::UseAdaptiveSync(vrr)) => {
                _state.vrr_mode = vrr;
            }
            Event::Msg(ThreadCommand::RenderStates(_)) => {
                // RenderStates are handled in the main thread, not the surface thread
                // This shouldn't happen, but we'll just ignore it if it does
                warn!("Received RenderStates in surface thread - this should be handled in main thread");
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
        
        // update refresh interval from actual DRM mode
        let mode = compositor.with_compositor(|c| c.surface().pending_mode());
        // calculate_refresh_rate returns millihertz, so divide by 1000 to get Hz
        let interval = Duration::from_secs_f64(1000.0 / crate::backend::kms::drm_helpers::calculate_refresh_rate(mode) as f64);
        self.timings.set_refresh_interval(Some(interval));
        
        // set minimum refresh interval (30Hz minimum like cosmic-comp)
        const _SAFETY_MARGIN: u32 = 2; // magic two frames margin from kwin (unused for now)
        let min_min_refresh_interval = Duration::from_secs_f64(1.0 / 30.0); // 30Hz
        self.timings.set_min_refresh_interval(Some(min_min_refresh_interval));
        
        // Phase 4h: Check VRR support on this output
        let vrr_support = compositor.with_compositor(|c| {
            c.vrr_supported(c.pending_connectors().into_iter().next().unwrap())
        }).ok();
        
        // store VRR support in output user data
        if let Some(support) = vrr_support {
            debug!("VRR support for {}: {:?}", self.output.name(), support);
            // TODO: Store in output user_data when we add OutputExt trait
            // self.output.set_adaptive_sync_support(Some(support));
        } else {
            debug!("VRR not supported on {}", self.output.name());
        }
        
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
        debug!("Surface {} calling queue_redraw for initial render", self.output.name());
        self.queue_redraw();
        debug!("Surface {} resume complete", self.output.name());
    }
    
    /// Select the appropriate render node for the output
    /// simplified version - just uses primary or target node
    #[allow(dead_code)] // used in redraw method
    fn render_node_for_output(&self) -> DrmNode {
        // for single-GPU case, always use the target node (render node)
        // the primary node is for DRM operations, not rendering
        self.target_node
    }
    
    /// Phase 5a: Check if we can use direct rendering (bypass offscreen)
    fn can_use_direct_render(&self) -> bool {
        // Phase 5a: Enable direct rendering when conditions are met
        // Direct rendering is possible when:
        // 1. No screen filters active (we don't have any yet)
        // 2. No output mirroring (we don't support mirroring yet)  
        // 3. No transform/scaling mismatch (not implemented)
        // 4. Simple rendering scenario
        
        // enable direct rendering for Phase 5a
        // this will give us proper buffer age from the DRM swapchain
        true
    }
    
    /// Phase 5c: Check if elements can use hardware planes
    // will be used in Phase 4i: Hardware Plane Optimization
    #[allow(dead_code)]
    fn assign_planes(&self, _elements: &[CosmicElement<GlMultiRenderer>]) -> Vec<PlaneAssignment> {
        // Phase 5c: Hardware plane support
        // TODO: Query available planes and assign elements to them
        // For now, everything goes to primary plane (rendered)
        vec![]
    }
    
    /// Phase 5e: Check if we can do direct scanout (fullscreen bypass)
    // will be used in Phase 4i: Hardware Plane Optimization
    #[allow(dead_code)]
    fn can_direct_scanout(&self, _elements: &[CosmicElement<GlMultiRenderer>]) -> bool {
        // Phase 5e: Direct scanout for fullscreen content
        // TODO: Check if single fullscreen element with compatible buffer
        false
    }
    
    /// Phase 4h: Check and enable VRR if supported
    // will be used for dynamic VRR updates
    #[allow(dead_code)]
    fn update_vrr(&mut self, enable: bool) {
        if let Some(compositor) = self.compositor.as_mut() {
            // try to enable/disable VRR
            if let Err(e) = compositor.with_compositor(|c| c.use_vrr(enable)) {
                debug!("VRR update failed: {:?}", e);
            } else {
                debug!("VRR {} for output {}", 
                    if enable { "enabled" } else { "disabled" },
                    self.output.name());
            }
        }
    }
    
    fn queue_redraw(&mut self) {
        self.queue_redraw_force(false);
    }
    
    fn queue_redraw_force(&mut self, force: bool) {
        let Some(_compositor) = self.compositor.as_mut() else {
            debug!("No compositor for {}, skipping queue_redraw", self.output.name());
            return;
        };
        
        debug!("queue_redraw_force called for {} (force={})", self.output.name(), force);
        
        if let QueueState::WaitingForVBlank { .. } = &self.state {
            // we're waiting for VBlank, request a redraw afterwards.
            // this is the only time we should set redraw_needed to true
            self.state = QueueState::WaitingForVBlank {
                redraw_needed: true,
            };
            debug!("Setting redraw_needed=true for {} (waiting for VBlank)", self.output.name());
            return;
        }
        
        if !force {
            match &self.state {
                QueueState::Idle | QueueState::WaitingForEstimatedVBlank(_) => {
                    debug!("{}: State allows scheduling (Idle or WaitingForEstimatedVBlank)", self.output.name());
                }
                
                // a redraw is already queued.
                QueueState::Queued(_) | QueueState::WaitingForEstimatedVBlankAndQueued { .. } => {
                    debug!("{}: Skipping - redraw already queued", self.output.name());
                    return;
                }
                _ => {
                    debug!("{}: Unknown state, continuing", self.output.name());
                },
            };
        }
        
        let estimated_presentation = self.timings.next_presentation_time(&self.clock);
        let render_start = self.timings.next_render_time(&self.clock);
        
        let timer = if render_start.is_zero() {
            debug!("{}: Running late for frame, using immediate timer", self.output.name());
            Timer::immediate()
        } else {
            debug!("{}: Scheduling render in {:?}", self.output.name(), render_start);
            Timer::from_duration(render_start)
        };
        
        let token = self
            .loop_handle
            .insert_source(timer, move |_time, _, state| {
                debug!("Timer fired for {}, starting render", state.output.name());
                state.timings.start_render(&state.clock);
                if let Err(err) = state.redraw(estimated_presentation) {
                    let name = state.output.name();
                    warn!(?name, "Failed to submit rendering: {:?}", err);
                    state.queue_redraw_force(true);
                } else {
                    debug!("Render completed successfully for {}", state.output.name());
                }
                TimeoutAction::Drop
            })
            .expect("Failed to schedule render");
        
        match &self.state {
            QueueState::Idle => {
                self.state = QueueState::Queued(token);
            }
            QueueState::WaitingForEstimatedVBlank(estimated_vblank) => {
                self.state = QueueState::WaitingForEstimatedVBlankAndQueued {
                    estimated_vblank: estimated_vblank.clone(),
                    queued_render: token,
                };
            }
            QueueState::Queued(old_token) if force => {
                self.loop_handle.remove(*old_token);
                self.state = QueueState::Queued(token);
            }
            QueueState::WaitingForEstimatedVBlankAndQueued {
                estimated_vblank,
                queued_render,
            } if force => {
                self.loop_handle.remove(*queued_render);
                self.state = QueueState::WaitingForEstimatedVBlankAndQueued {
                    estimated_vblank: estimated_vblank.clone(),
                    queued_render: token,
                };
            }
            _ => {},
        }
    }
    
    fn on_vblank(&mut self, metadata: Option<DrmEventMetadata>) {
        let Some(compositor) = self.compositor.as_mut() else {
            return;
        };
        if matches!(self.state, QueueState::Idle) {
            // can happen right after resume
            return;
        }
        
        let now = self.clock.now();
        let presentation_time = match metadata.as_ref().map(|data| &data.time) {
            Some(DrmEventTime::Monotonic(tp)) => Some(tp.clone()),
            _ => None,
        };
        
        // mark last frame completed and send presentation feedback
        if let Ok(Some(feedback)) = compositor.frame_submitted() {
            let clock = if let Some(tp) = presentation_time {
                tp.into()  // convert Duration to Time<Monotonic>
            } else {
                now
            };
            
            // send presentation feedback to clients if available
            if let Some(mut feedback) = feedback {
                // get refresh interval from output mode
                use smithay::wayland::presentation::Refresh;
                let refresh = self.output.current_mode()
                    .map(|mode| {
                        let duration = Duration::from_secs_f64(1.0 / mode.refresh as f64 * 1000.0);
                        Refresh::Fixed(duration)
                    })
                    .unwrap_or(Refresh::Fixed(Duration::from_millis(16)));
                
                // get sequence number from metadata
                // note: Often 0 if DRM driver doesn't provide frame counter
                let sequence = metadata.as_ref()
                    .map(|m| {
                        debug!("VBlank metadata: sequence={}, time={:?}", m.sequence, m.time);
                        m.sequence
                    })
                    .unwrap_or_else(|| {
                        debug!("No VBlank metadata available");
                        0
                    }) as u64;
                
                // presentation flags - vsync, hardware completion
                use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
                let flags = wp_presentation_feedback::Kind::Vsync | 
                           wp_presentation_feedback::Kind::HwCompletion;
                
                feedback.presented(clock, refresh, sequence, flags);
            }
            
            self.timings.presented(clock);
        }
        
        // extract redraw_needed from current state and transition to Idle
        let redraw_needed = match std::mem::replace(&mut self.state, QueueState::Idle) {
            QueueState::WaitingForVBlank { redraw_needed } => redraw_needed,
            QueueState::WaitingForEstimatedVBlank(token) => {
                self.loop_handle.remove(token);
                false
            }
            QueueState::WaitingForEstimatedVBlankAndQueued { estimated_vblank, queued_render } => {
                self.loop_handle.remove(estimated_vblank);
                self.state = QueueState::Queued(queued_render);
                return;
            }
            _ => false,
        };
        
        self.frame_count = self.frame_count.saturating_add(1);
        
        // check if we need to continue rendering
        // only redraw if explicitly needed or if there are ongoing animations
        let needs_render = {
            let shell = self.shell.read().unwrap();
            redraw_needed || shell.animations_going()
        };
        
        if needs_render {
            self.queue_redraw();
        }
        
        // note: frame callbacks are already sent in redraw() when we successfully queue_frame
        // or in on_estimated_vblank() when we don't render
    }
    
    /// Send frame callbacks to all windows on this output
    /// This allows clients to continue their animations (like cursor blinking)
    fn send_frame_callbacks(&mut self) {
        use smithay::desktop::utils::send_frames_surface_tree;
        
        let clock = self.clock.now();
        let output = &self.output;
        
        // increment sequence to prevent empty-damage commit busy loops
        self.frame_callback_seq = self.frame_callback_seq.wrapping_add(1);
        
        // send frame callbacks to all windows on this output
        let shell = self.shell.read().unwrap();
        for window in shell.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                send_frames_surface_tree(
                    toplevel.wl_surface(),
                    output,
                    clock,
                    None,
                    |_, _| Some(output.clone()),  // always send for now
                );
            }
        }
        drop(shell);  // release the read lock
        
        // send frame callbacks to layer surfaces on this output
        let layer_map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in layer_map.layers() {
            send_frames_surface_tree(
                layer_surface.wl_surface(),
                output,
                clock,
                None,
                |_, _| Some(output.clone()),  // always send for now
            );
        }
    }
    
    /// Perform a redraw with damage tracking using PostprocessState
    fn redraw(&mut self, _estimated_presentation: Duration) -> Result<()> {
        debug!("Starting redraw for {}", self.output.name());
        
        // check we have a compositor first
        if self.compositor.is_none() {
            debug!("No compositor for {}, skipping redraw", self.output.name());
            return Ok(());
        }
        
        // check we have postprocess state (only if not using direct render)
        // Phase 5a: Direct Rendering Path - decide between direct and offscreen rendering
        let use_direct_render = self.can_use_direct_render();
        
        if !use_direct_render && self.postprocess.is_none() {
            error!("No postprocess state for output {}", self.output.name());
            return Ok(());
        }
        
        // Phase 4b: Collect render elements from the shell
        // get appropriate renderer before borrowing shell
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
        
        // check if windows need to be re-arranged before rendering
        {
            let shell = self.shell.write().unwrap();
            // Check if active workspace needs arrangement
            if let Some(workspace) = shell.active_workspace(&self.output) {
                if workspace.needs_arrange {
                    debug!("Windows need arrangement before render");
                    drop(shell);
                    let mut shell = self.shell.write().unwrap();
                    shell.arrange_windows_on_output(&self.output);
                }
            }
        }
        
        // collect elements from shell
        let mut elements = {
            let shell = self.shell.read().unwrap();
            shell.render_elements(&self.output, &mut renderer)
        };
        
        // add cursor elements (following cosmic-comp's approach)
        // Phase 4f: Add cursor rendering - software cursor for now
        // TODO: Hardware cursor via DRM planes will be added later in this phase
        
        // get cursor info from shell (which is updated by input handler)
        let (cursor_position, cursor_status) = {
            let shell = self.shell.read().unwrap();
            (shell.cursor_position, shell.cursor_status.clone())
        };
        
        // check if cursor is on this output
        let output_loc = self.output.current_location();
        let output_size = self.output.current_mode()
            .map(|m| Size::from((m.size.w as i32, m.size.h as i32)))
            .unwrap_or_default();
        let output_rect = Rectangle::new(output_loc, output_size);
        
        // for now, only render cursor on the output that contains the cursor hotspot
        // this avoids duplicate cursors when outputs overlap at the same position
        // TODO: once we have proper multi-monitor positioning, check for cursor rect overlap instead
        let cursor_elements = if output_rect.contains(cursor_position.to_i32_round()) {
            // get cursor state from seat user data
            let cursor_state = self.seat.user_data()
                .get::<crate::backend::render::cursor::CursorState>()
                .unwrap();
            let mut cursor_state_ref = cursor_state.lock().unwrap();
            
            // get current time for animated cursors
            let now = self.clock.now();
            
            // draw cursor (relative to this output)
            let relative_pos = cursor_position - output_loc.to_f64();
            debug!("Rendering cursor on {} at relative position {:?}", self.output.name(), relative_pos);
            
            cursor::draw_cursor(
                &mut renderer,
                &mut *cursor_state_ref,
                &cursor_status,
                relative_pos,
                self.output.current_scale().fractional_scale().into(),
                now.as_millis() as u32,
            )
        } else {
            // cursor is not on this output
            Vec::new()
        };
        
        debug!("Adding {} cursor elements to render for {}", cursor_elements.len(), self.output.name());
        
        // add cursor elements to the element list (at the beginning to avoid opaque region culling)
        // cursor should always be visible regardless of what's beneath it
        for (elem, hotspot) in cursor_elements.into_iter().rev() {
            // log cursor hotspot and kind for debugging
            use smithay::backend::renderer::element::Element;
            let elem_kind = elem.kind();
            debug!("Cursor element hotspot: {:?}, kind: {:?}", hotspot, elem_kind);
            
            // wrap cursor element in CosmicElement
            let cosmic_elem = CosmicElement::Cursor(elem);
            debug!("CosmicElement cursor kind: {:?}", cosmic_elem.kind());
            elements.insert(0, cosmic_elem);  // insert at beginning
        }
        
        // log element kinds for debugging z-order
        let element_kinds: Vec<_> = elements.iter().map(|e| match e {
            CosmicElement::Surface(_) => "Surface",
            CosmicElement::Damage(_) => "Damage", 
            CosmicElement::Texture(_) => "Texture",
            CosmicElement::Cursor(_) => "Cursor",
        }).collect();
        debug!("Element order for {}: {:?}", self.output.name(), element_kinds);
        
        // mark element gathering done
        self.timings.elements_done(&self.clock);
        
        // Phase 4h: Determine if VRR should be active
        let has_fullscreen = {
            let shell = self.shell.read().unwrap();
            shell.get_fullscreen(&self.output).is_some()
        };
        
        let vrr = match self.vrr_mode {
            AdaptiveSync::Force => true,
            AdaptiveSync::Enabled => has_fullscreen,
            AdaptiveSync::Disabled => false,
        };
        
        // set VRR on compositor before rendering
        if let Some(compositor) = self.compositor.as_mut() {
            if let Err(err) = compositor.with_compositor(|c| c.use_vrr(vrr)) {
                warn!("Unable to set VRR: {}", err);
            }
        }
        
        // update timings for VRR
        self.timings.set_vrr(vrr);
        
        // Phase 5a: Choose between direct and offscreen rendering
        if use_direct_render {
            // Phase 5a: Direct rendering path - render directly to DRM framebuffer
            debug!("[DIRECT] Starting render for {} (VRR={})", self.output.name(), vrr);
            
            // render directly to the DRM compositor's framebuffer
            // this gives us proper buffer age from the swapchain
            let frame_result = self.compositor.as_mut().unwrap().render_frame(
                &mut renderer,
                &elements,
                crate::backend::render::CLEAR_COLOR,  // grey background
                FrameFlags::DEFAULT,  // includes cursor plane scanout
            ).map_err(|e| anyhow::anyhow!("Failed to render frame: {:?}", e))?;
            
            debug!("[DIRECT] Render result for {}: is_empty={}, cursor_element={:?}, overlay_elements={}", 
                   self.output.name(), 
                   frame_result.is_empty,
                   frame_result.cursor_element.is_some(),
                   frame_result.overlay_elements.len());
            
            // mark submission time
            self.timings.submitted_for_presentation(&self.clock);
            
            // extract render states before any other operations
            let render_states = frame_result.states;
            
            // collect presentation feedback if frame is not empty
            let feedback = if !frame_result.is_empty {
                Some(self.shell.read().unwrap().take_presentation_feedback(
                    &self.output,
                    &render_states,
                ))
            } else {
                None
            };
            
            // always try to queue the frame, even if empty
            // the compositor will return EmptyFrame error if there's no damage
            match self.compositor.as_mut().unwrap().queue_frame(feedback) {
                Ok(()) => {
                    // successfully queued, we'll get a real VBlank
                    self.state = QueueState::WaitingForVBlank {
                        redraw_needed: false,
                    };
                    
                    // for direct rendering, we don't have damage tracking yet
                    // Phase 5b will add proper damage tracking with swapchain
                    self.last_frame_damage = None;
                    
                    // send frame callbacks now since we queued a frame
                    self.frame_callback_seq = self.frame_callback_seq.wrapping_add(1);
                    self.send_frame_callbacks();
                    
                    // send render states back to main thread for fractional scale updates
                    if let Err(e) = self.thread_sender.send(SurfaceCommand::RenderStates(render_states)) {
                        warn!("Failed to send render states to main thread: {:?}", e);
                    }
                    
                    trace!("Direct frame queued for output {}", self.output.name());
                }
                Err(smithay::backend::drm::compositor::FrameError::EmptyFrame) => {
                    // empty frame - use estimated VBlank to maintain frame callbacks
                    debug!("[DIRECT] Empty frame for output {}, using estimated VBlank", self.output.name());
                    
                    // calculate estimated presentation time
                    let estimated_presentation = self.timings.next_presentation_time(&self.clock);
                    
                    // queue estimated vblank timer to maintain frame timing
                    self.queue_estimated_vblank(
                        estimated_presentation,
                        false, // don't force redraw
                    );
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to queue frame: {:?}", e));
                }
            }
            
            return Ok(());
        }
        
        // offscreen rendering path - render to texture first for post-processing
        debug!("[OFFSCREEN] Starting render for {}", self.output.name());
        
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
                
                // mark drawing done
                self.timings.draw_done(&self.clock);
                
                // Phase 2je: Return and accumulate damage regions
                let area = texture.size().to_logical(1, transform);
                
                let damage = res.damage
                    .cloned()
                    .map(|v| {
                        v.into_iter()
                            .map(|r| r.to_logical(1).to_buffer(1, transform, &area))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                    
                // Store damage for next frame
                self.last_frame_damage = Some(damage.clone());
                self.frame_count += 1;
                
                Ok(damage)
            })
            .context("Failed to draw to offscreen render target")?;
            
            // NOTE: We can't skip on empty damage yet because we use age 1
            // which forces full redraw. This will be fixed when we implement
            // proper buffer age tracking in Phase 2je
            
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
        let postprocess_elements: Vec<CosmicElement<GlMultiRenderer>> = vec![
            CosmicElement::Texture(texture_element)
        ];
        
        // use the multi-gpu renderer to present the composited texture
        let frame_result = self.compositor.as_mut().unwrap().render_frame(
            &mut renderer,
            &postprocess_elements,
            [0.0, 0.0, 0.0, 0.0],  // black background (already rendered in texture)
            FrameFlags::DEFAULT,  // includes cursor plane scanout
        ).map_err(|e| anyhow::anyhow!("Frame render failed: {:?}", e))?;
        
        debug!("[OFFSCREEN] Render result for {}: is_empty={}", self.output.name(), frame_result.is_empty);
        
        // mark submission time
        self.timings.submitted_for_presentation(&self.clock);
        
        // extract render states before any other operations
        let render_states = frame_result.states;
        
        // collect presentation feedback if frame is not empty
        let feedback = if !frame_result.is_empty {
            Some(self.shell.read().unwrap().take_presentation_feedback(
                &self.output,
                &render_states,
            ))
        } else {
            None
        };
        
        // always try to queue the frame, even if empty
        // the compositor will return EmptyFrame error if there's no damage
        match self.compositor.as_mut().unwrap().queue_frame(feedback) {
            Ok(()) => {
                // successfully queued, we'll get a real VBlank
                self.state = QueueState::WaitingForVBlank {
                    redraw_needed: false,
                };
                
                // send frame callbacks now since we queued a frame
                self.frame_callback_seq = self.frame_callback_seq.wrapping_add(1);
                self.send_frame_callbacks();
                
                // send render states back to main thread for fractional scale updates
                if let Err(e) = self.thread_sender.send(SurfaceCommand::RenderStates(render_states)) {
                    warn!("Failed to send render states to main thread: {:?}", e);
                }
                
                trace!("Frame queued for output {}, damage regions: {}", 
                    self.output.name(), 
                    self.last_frame_damage.as_ref().map(|d| d.len()).unwrap_or(0)
                );
            }
            Err(smithay::backend::drm::compositor::FrameError::EmptyFrame) => {
                // empty frame - use estimated VBlank to maintain frame callbacks
                debug!("[OFFSCREEN] Empty frame for output {}, using estimated VBlank", self.output.name());
                
                // calculate estimated presentation time
                let estimated_presentation = self.timings.next_presentation_time(&self.clock);
                
                // queue estimated vblank timer to maintain frame timing
                self.queue_estimated_vblank(
                    estimated_presentation,
                    false, // don't force redraw
                );
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to queue frame: {:?}", e));
            }
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
    
    /// Queue an estimated VBlank timer when we didn't submit to KMS
    /// This maintains frame callback timing without actual rendering
    fn queue_estimated_vblank(&mut self, target_presentation_time: Duration, force: bool) {
        match std::mem::take(&mut self.state) {
            QueueState::Idle => unreachable!("queue_estimated_vblank called in Idle state"),
            QueueState::Queued(_) => (), // render was queued while we were working
            QueueState::WaitingForVBlank { .. } => unreachable!("queue_estimated_vblank called while waiting for VBlank"),
            QueueState::WaitingForEstimatedVBlank(token)
            | QueueState::WaitingForEstimatedVBlankAndQueued {
                estimated_vblank: token,
                ..
            } => {
                // already have an estimated vblank timer, keep it
                self.state = QueueState::WaitingForEstimatedVBlank(token);
                return;
            }
        }

        let now = self.clock.now();
        let mut duration = target_presentation_time.saturating_sub(now.into());

        // no use setting a zero timer, since we'll send frame callbacks anyway right after
        // this can happen for example with unknown presentation time from DRM
        if duration.is_zero() {
            duration += self.timings.refresh_interval();
        }

        debug!("Queueing estimated vblank timer to fire in {:?} for {}", duration, self.output.name());

        let timer = Timer::from_duration(duration);
        let token = self
            .loop_handle
            .insert_source(timer, move |_, _, data| {
                data.on_estimated_vblank(force);
                TimeoutAction::Drop
            })
            .unwrap();
        self.state = QueueState::WaitingForEstimatedVBlank(token);
    }
    
    /// Handle estimated VBlank timer firing
    /// Sends frame callbacks and optionally triggers redraw
    fn on_estimated_vblank(&mut self, force: bool) {
        let old_state = std::mem::replace(&mut self.state, QueueState::Idle);
        match old_state {
            QueueState::Idle => {
                warn!("on_estimated_vblank called in Idle state");
                return;
            }
            QueueState::Queued(token) => {
                // a real render was queued while timer was pending, ignore timer
                self.state = QueueState::Queued(token);
                return;
            }
            QueueState::WaitingForVBlank { redraw_needed } => {
                // we got a real frame queued while timer was pending, ignore timer
                self.state = QueueState::WaitingForVBlank { redraw_needed };
                return;
            }
            QueueState::WaitingForEstimatedVBlank(_) => (),
            // the timer fired just in front of a redraw
            QueueState::WaitingForEstimatedVBlankAndQueued { queued_render, .. } => {
                self.state = QueueState::Queued(queued_render);
                return;
            }
        }

        self.frame_callback_seq = self.frame_callback_seq.wrapping_add(1);

        // check if we need to trigger a redraw
        let should_redraw = {
            let shell = self.shell.read().unwrap();
            force || shell.animations_going()
        };
        
        if should_redraw {
            self.queue_redraw();
        }
        
        self.send_frame_callbacks();
    }
}