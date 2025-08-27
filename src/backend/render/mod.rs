// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::{
        allocator::{dmabuf::{Dmabuf, AnyError, DmabufAllocator}, gbm::GbmAllocator, Allocator},
        drm::DrmNode,
        renderer::{
            glow::GlowRenderer,
            multigpu::{ApiDevice, GraphicsApi},
        },
    },
};
use std::{
    cell::Cell,
    collections::HashMap,
    os::unix::prelude::AsFd,
    sync::atomic::{AtomicBool, Ordering},
};

/// A simplified GraphicsApi for GBM/GLES rendering
pub struct GbmGlowBackend<A: AsFd + 'static> {
    devices: HashMap<DrmNode, (GbmAllocator<A>, Cell<Option<GlowRenderer>>)>,
    needs_enumeration: AtomicBool,
}

impl<A: AsFd + 'static> std::fmt::Debug for GbmGlowBackend<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GbmGlowBackend")
            .field("devices", &self.devices.keys().collect::<Vec<_>>())
            .field("needs_enumeration", &self.needs_enumeration)
            .finish()
    }
}

impl<A: AsFd + 'static> Default for GbmGlowBackend<A> {
    fn default() -> Self {
        Self {
            devices: HashMap::new(),
            needs_enumeration: AtomicBool::new(true),
        }
    }
}

impl<A: AsFd + Clone + Send + 'static> GbmGlowBackend<A> {
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
            needs_enumeration: AtomicBool::new(false),
        }
    }

    pub fn add_node(&mut self, node: DrmNode, gbm: GbmAllocator<A>, renderer: GlowRenderer) {
        if self.devices.contains_key(&node) {
            return;
        }

        self.devices.insert(node, (gbm, Cell::new(Some(renderer))));
        self.needs_enumeration.store(true, Ordering::SeqCst);
    }

    pub fn remove_node(&mut self, node: &DrmNode) {
        if self.devices.remove(node).is_some() {
            self.needs_enumeration.store(true, Ordering::SeqCst);
        }
    }
}

/// Error type for the GbmGlowBackend
#[derive(Debug, thiserror::Error)]
pub enum GbmGlowError {
    #[error("Failed to allocate buffer")]
    Allocation,
    #[error("Rendering error: {0}")]
    Render(#[from] smithay::backend::renderer::gles::GlesError),
}


impl<A: AsFd + Clone + 'static> GraphicsApi for GbmGlowBackend<A> {
    type Device = GbmGlowDevice;
    type Error = GbmGlowError;

    fn enumerate(&self, list: &mut Vec<Self::Device>) -> Result<(), Self::Error> {
        self.needs_enumeration.store(false, Ordering::SeqCst);

        // remove old devices
        list.retain(|device| {
            self.devices
                .keys()
                .any(|node| device.node.dev_id() == node.dev_id())
        });

        // add new devices
        for (node, (allocator, renderer)) in &self.devices {
            if list.iter().any(|d| d.node.dev_id() == node.dev_id()) {
                continue;
            }

            if let Some(renderer) = renderer.take() {
                // take ownership from the Cell
                list.push(GbmGlowDevice {
                    node: *node,
                    renderer,
                    allocator: Box::new(DmabufAllocator(allocator.clone())),
                });
            }
        }

        Ok(())
    }

    fn needs_enumeration(&self) -> bool {
        self.needs_enumeration.load(Ordering::Acquire)
    }

    fn identifier() -> &'static str {
        "gbm_glow"
    }
}

/// Device for the GbmGlowBackend
pub struct GbmGlowDevice {
    node: DrmNode,
    renderer: GlowRenderer,
    allocator: Box<dyn Allocator<Buffer = Dmabuf, Error = AnyError>>,
}

impl std::fmt::Debug for GbmGlowDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GbmGlowDevice")
            .field("node", &self.node)
            .field("renderer", &"GlowRenderer")
            .field("allocator", &"GbmAllocator")
            .finish()
    }
}

impl ApiDevice for GbmGlowDevice {
    type Renderer = GlowRenderer;

    fn renderer(&self) -> &Self::Renderer {
        &self.renderer
    }

    fn renderer_mut(&mut self) -> &mut Self::Renderer {
        &mut self.renderer
    }

    fn allocator(&mut self) -> &mut dyn Allocator<Buffer = Dmabuf, Error = AnyError> {
        &mut *self.allocator
    }

    fn node(&self) -> &DrmNode {
        &self.node
    }
}

/// Initialize shaders for the renderer
pub fn init_shaders(_renderer: &mut GlowRenderer) -> Result<(), anyhow::Error> {
    // smithay's GlowRenderer handles shader compilation internally
    // shaders are compiled on first use
    Ok(())
}

/// Clear color for empty frames
pub const CLEAR_COLOR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];