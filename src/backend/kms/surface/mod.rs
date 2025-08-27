// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use smithay::{
    backend::{
        allocator::{
            gbm::GbmAllocator,
            format::FormatSet,
        },
        drm::{
            exporter::gbm::GbmFramebufferExporter,
            output::DrmOutput,
            DrmDeviceFd, DrmNode,
        },
        renderer::{
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

/// Surface with render thread
pub struct Surface {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub output: Output,
    pub primary_plane_formats: FormatSet,
    
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
}