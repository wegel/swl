// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::renderer::{
        element::{
            surface::WaylandSurfaceRenderElement,
            texture::TextureRenderElement,
            memory::MemoryRenderBufferRenderElement,
            Element, Id, Kind, RenderElement, UnderlyingStorage,
        },
        gles::{GlesError, GlesTexture},
        glow::{GlowRenderer, GlowFrame},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        ImportAll, ImportMem, Renderer,
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale},
};

use super::GlMultiRenderer;
use crate::backend::render::{cursor::CursorRenderElement, GlMultiError};

/// Trait for converting GlesError to renderer-specific errors
#[allow(dead_code)] // will be used for error conversion in rendering
pub trait FromGlesError {
    fn from_gles_error(err: GlesError) -> Self;
}

impl FromGlesError for GlesError {
    fn from_gles_error(err: GlesError) -> Self {
        err
    }
}

impl FromGlesError for GlMultiError {
    fn from_gles_error(err: GlesError) -> Self {
        // convert GlesError to MultiError
        // The Render variant expects the renderer error type
        smithay::backend::renderer::multigpu::Error::Render(err)
    }
}

/// Trait for renderers that can provide a GlowRenderer reference
#[allow(dead_code)] // will be used for multi-renderer support
pub trait AsGlowRenderer: Renderer {
    fn glow_renderer(&self) -> &GlowRenderer;
    fn glow_renderer_mut(&mut self) -> &mut GlowRenderer;
    fn glow_frame<'a, 'frame, 'buffer>(
        frame: &'a Self::Frame<'frame, 'buffer>,
    ) -> &'a GlowFrame<'frame, 'buffer>;
    fn glow_frame_mut<'a, 'frame, 'buffer>(
        frame: &'a mut Self::Frame<'frame, 'buffer>,
    ) -> &'a mut GlowFrame<'frame, 'buffer>;
}

impl AsGlowRenderer for GlowRenderer {
    fn glow_renderer(&self) -> &GlowRenderer {
        self
    }
    
    fn glow_renderer_mut(&mut self) -> &mut GlowRenderer {
        self
    }
    
    fn glow_frame<'a, 'frame, 'buffer>(
        frame: &'a Self::Frame<'frame, 'buffer>,
    ) -> &'a GlowFrame<'frame, 'buffer> {
        frame
    }
    
    fn glow_frame_mut<'a, 'frame, 'buffer>(
        frame: &'a mut Self::Frame<'frame, 'buffer>,
    ) -> &'a mut GlowFrame<'frame, 'buffer> {
        frame
    }
}

impl<'a> AsGlowRenderer for GlMultiRenderer<'a> {
    fn glow_renderer(&self) -> &GlowRenderer {
        self.as_ref()
    }
    
    fn glow_renderer_mut(&mut self) -> &mut GlowRenderer {
        self.as_mut()
    }
    
    fn glow_frame<'b, 'frame, 'buffer>(
        frame: &'b Self::Frame<'frame, 'buffer>,
    ) -> &'b GlowFrame<'frame, 'buffer> {
        frame.as_ref()
    }
    
    fn glow_frame_mut<'b, 'frame, 'buffer>(
        frame: &'b mut Self::Frame<'frame, 'buffer>,
    ) -> &'b mut GlowFrame<'frame, 'buffer> {
        frame.as_mut()
    }
}

/// A damage-only element for forcing damage in specific regions
pub struct DamageElement {
    id: Id,
    location: Point<i32, Physical>,
    geometry: Rectangle<i32, Physical>,
    commit: CommitCounter,
}

impl DamageElement {
    #[allow(dead_code)] // will be used for damage tracking
    pub fn new(location: Point<i32, Physical>, size: Rectangle<i32, Physical>) -> Self {
        Self {
            id: Id::new(),
            location,
            geometry: size,
            commit: CommitCounter::default(),
        }
    }
}

impl Element for DamageElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        Rectangle::from_size((0.0, 0.0).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn location(&self, _scale: Scale<f64>) -> Point<i32, Physical> {
        self.location
    }

    fn transform(&self) -> smithay::utils::Transform {
        smithay::utils::Transform::Normal
    }

    fn damage_since(&self, _scale: Scale<f64>, _commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        DamageSet::from_slice(&[self.geometry])
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::from_slice(&[])
    }
    
    fn alpha(&self) -> f32 {
        1.0
    }
    
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: Renderer> RenderElement<R> for DamageElement {
    fn draw<'frame>(
        &self,
        _frame: &mut R::Frame<'frame, '_>,
        _src: Rectangle<f64, BufferCoords>,
        _dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        // damage-only element doesn't actually draw anything
        Ok(())
    }
}

// DamageElement doesn't have underlying storage as it's draw-only

/// Simplified render element enum for basic compositor functionality
/// This will grow as we add more features
#[allow(dead_code)] // will be used in Phase 2f/2g for rendering
pub enum CosmicElement<R>
where
    R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
    R::TextureId: 'static,
{
    /// A wayland surface (window content)
    Surface(WaylandSurfaceRenderElement<R>),
    /// Additional damage for forcing redraws
    Damage(DamageElement),
    /// Texture element for offscreen rendering composition
    Texture(TextureRenderElement<GlesTexture>),
    /// Cursor element
    Cursor(CursorRenderElement<R>),
}

impl<R> Element for CosmicElement<R>
where
    R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
    R::TextureId: 'static,
{
    fn id(&self) -> &Id {
        match self {
            CosmicElement::Surface(elem) => elem.id(),
            CosmicElement::Damage(elem) => elem.id(),
            CosmicElement::Texture(elem) => elem.id(),
            CosmicElement::Cursor(elem) => elem.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match self {
            CosmicElement::Surface(elem) => elem.current_commit(),
            CosmicElement::Damage(elem) => elem.current_commit(),
            CosmicElement::Texture(elem) => elem.current_commit(),
            CosmicElement::Cursor(elem) => elem.current_commit(),
        }
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        match self {
            CosmicElement::Surface(elem) => elem.src(),
            CosmicElement::Damage(elem) => elem.src(),
            CosmicElement::Texture(elem) => elem.src(),
            CosmicElement::Cursor(elem) => elem.src(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            CosmicElement::Surface(elem) => elem.geometry(scale),
            CosmicElement::Damage(elem) => elem.geometry(scale),
            CosmicElement::Texture(elem) => elem.geometry(scale),
            CosmicElement::Cursor(elem) => elem.geometry(scale),
        }
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        match self {
            CosmicElement::Surface(elem) => elem.location(scale),
            CosmicElement::Damage(elem) => elem.location(scale),
            CosmicElement::Texture(elem) => elem.location(scale),
            CosmicElement::Cursor(elem) => elem.location(scale),
        }
    }

    fn transform(&self) -> smithay::utils::Transform {
        match self {
            CosmicElement::Surface(elem) => elem.transform(),
            CosmicElement::Damage(elem) => elem.transform(),
            CosmicElement::Texture(elem) => elem.transform(),
            CosmicElement::Cursor(elem) => elem.transform(),
        }
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        match self {
            CosmicElement::Surface(elem) => elem.damage_since(scale, commit),
            CosmicElement::Damage(elem) => elem.damage_since(scale, commit),
            CosmicElement::Texture(elem) => elem.damage_since(scale, commit),
            CosmicElement::Cursor(elem) => elem.damage_since(scale, commit),
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        match self {
            CosmicElement::Surface(elem) => elem.opaque_regions(scale),
            CosmicElement::Damage(elem) => elem.opaque_regions(scale),
            CosmicElement::Texture(elem) => elem.opaque_regions(scale),
            CosmicElement::Cursor(elem) => elem.opaque_regions(scale),
        }
    }

    fn alpha(&self) -> f32 {
        match self {
            CosmicElement::Surface(elem) => elem.alpha(),
            CosmicElement::Damage(elem) => elem.alpha(),
            CosmicElement::Texture(elem) => elem.alpha(),
            CosmicElement::Cursor(elem) => elem.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            CosmicElement::Surface(elem) => elem.kind(),
            CosmicElement::Damage(elem) => elem.kind(),
            CosmicElement::Texture(elem) => elem.kind(),
            CosmicElement::Cursor(elem) => elem.kind(),
        }
    }
}

impl<R> RenderElement<R> for CosmicElement<R>
where
    R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
    R::TextureId: 'static,
    R::Error: FromGlesError,
{
    fn draw<'frame>(
        &self,
        frame: &mut R::Frame<'frame, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        match self {
            CosmicElement::Surface(elem) => elem.draw(frame, src, dst, damage, opaque_regions),
            CosmicElement::Damage(elem) => <DamageElement as RenderElement<R>>::draw(elem, frame, src, dst, damage, opaque_regions),
            CosmicElement::Texture(elem) => <TextureRenderElement<GlesTexture> as RenderElement<GlowRenderer>>::draw(
                elem,
                R::glow_frame_mut(frame),
                src,
                dst,
                damage,
                opaque_regions,
            ).map_err(R::Error::from_gles_error),
            CosmicElement::Cursor(elem) => elem.draw(frame, src, dst, damage, opaque_regions),
        }
    }
    
    fn underlying_storage(&self, renderer: &mut R) -> Option<smithay::backend::renderer::element::UnderlyingStorage<'_>> {
        match self {
            CosmicElement::Surface(elem) => elem.underlying_storage(renderer),
            CosmicElement::Damage(_) => None,  // DamageElement has no underlying storage
            CosmicElement::Texture(_) => None, // TextureRenderElement doesn't provide underlying storage for external renderers
            CosmicElement::Cursor(elem) => elem.underlying_storage(renderer),
        }
    }
}

// Note: UnderlyingStorage trait implementation would be needed for advanced features
// For now, basic rendering is sufficient