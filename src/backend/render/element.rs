// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::renderer::{
        element::{
            surface::WaylandSurfaceRenderElement,
            texture::TextureRenderElement,
            solid::SolidColorRenderElement,
            utils::RelocateRenderElement,
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
        // the Render variant expects the renderer error type
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

// damage element doesn't have underlying storage as it's draw-only

/// Simplified render element enum for basic compositor functionality
/// This will grow as we add more features
#[allow(dead_code)] // will be used in Phase 2f/2g for rendering
pub enum SwlElement<R>
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
    /// Cursor element (wrapped with relocate for hotspot offset)
    Cursor(RelocateRenderElement<CursorRenderElement<R>>),
    /// Solid color element (for borders, backgrounds, etc)
    SolidColor(SolidColorRenderElement),
}

impl<R> Element for SwlElement<R>
where
    R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
    R::TextureId: 'static,
{
    fn id(&self) -> &Id {
        match self {
            SwlElement::Surface(elem) => elem.id(),
            SwlElement::Damage(elem) => elem.id(),
            SwlElement::Texture(elem) => elem.id(),
            SwlElement::Cursor(elem) => elem.id(),
            SwlElement::SolidColor(elem) => elem.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match self {
            SwlElement::Surface(elem) => elem.current_commit(),
            SwlElement::Damage(elem) => elem.current_commit(),
            SwlElement::Texture(elem) => elem.current_commit(),
            SwlElement::Cursor(elem) => elem.current_commit(),
            SwlElement::SolidColor(elem) => elem.current_commit(),
        }
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        match self {
            SwlElement::Surface(elem) => elem.src(),
            SwlElement::Damage(elem) => elem.src(),
            SwlElement::Texture(elem) => elem.src(),
            SwlElement::Cursor(elem) => elem.src(),
            SwlElement::SolidColor(elem) => elem.src(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            SwlElement::Surface(elem) => elem.geometry(scale),
            SwlElement::Damage(elem) => elem.geometry(scale),
            SwlElement::Texture(elem) => elem.geometry(scale),
            SwlElement::Cursor(elem) => elem.geometry(scale),
            SwlElement::SolidColor(elem) => elem.geometry(scale),
        }
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        match self {
            SwlElement::Surface(elem) => elem.location(scale),
            SwlElement::Damage(elem) => elem.location(scale),
            SwlElement::Texture(elem) => elem.location(scale),
            SwlElement::Cursor(elem) => elem.location(scale),
            SwlElement::SolidColor(elem) => elem.location(scale),
        }
    }

    fn transform(&self) -> smithay::utils::Transform {
        match self {
            SwlElement::Surface(elem) => elem.transform(),
            SwlElement::Damage(elem) => elem.transform(),
            SwlElement::Texture(elem) => elem.transform(),
            SwlElement::Cursor(elem) => elem.transform(),
            SwlElement::SolidColor(elem) => elem.transform(),
        }
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        match self {
            SwlElement::Surface(elem) => elem.damage_since(scale, commit),
            SwlElement::Damage(elem) => elem.damage_since(scale, commit),
            SwlElement::Texture(elem) => elem.damage_since(scale, commit),
            SwlElement::Cursor(elem) => elem.damage_since(scale, commit),
            SwlElement::SolidColor(elem) => elem.damage_since(scale, commit),
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        match self {
            SwlElement::Surface(elem) => elem.opaque_regions(scale),
            SwlElement::Damage(elem) => elem.opaque_regions(scale),
            SwlElement::Texture(elem) => elem.opaque_regions(scale),
            SwlElement::Cursor(elem) => elem.opaque_regions(scale),
            SwlElement::SolidColor(elem) => elem.opaque_regions(scale),
        }
    }

    fn alpha(&self) -> f32 {
        match self {
            SwlElement::Surface(elem) => elem.alpha(),
            SwlElement::Damage(elem) => elem.alpha(),
            SwlElement::Texture(elem) => elem.alpha(),
            SwlElement::Cursor(elem) => elem.alpha(),
            SwlElement::SolidColor(elem) => elem.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            SwlElement::Surface(elem) => elem.kind(),
            SwlElement::Damage(elem) => elem.kind(),
            SwlElement::Texture(elem) => elem.kind(),
            SwlElement::Cursor(elem) => elem.kind(),
            SwlElement::SolidColor(elem) => elem.kind(),
        }
    }
}

impl<R> RenderElement<R> for SwlElement<R>
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
            SwlElement::Surface(elem) => elem.draw(frame, src, dst, damage, opaque_regions),
            SwlElement::Damage(elem) => <DamageElement as RenderElement<R>>::draw(elem, frame, src, dst, damage, opaque_regions),
            SwlElement::Texture(elem) => <TextureRenderElement<GlesTexture> as RenderElement<GlowRenderer>>::draw(
                elem,
                R::glow_frame_mut(frame),
                src,
                dst,
                damage,
                opaque_regions,
            ).map_err(R::Error::from_gles_error),
            SwlElement::Cursor(elem) => elem.draw(frame, src, dst, damage, opaque_regions),
            SwlElement::SolidColor(elem) => <SolidColorRenderElement as RenderElement<GlowRenderer>>::draw(
                elem,
                R::glow_frame_mut(frame),
                src,
                dst,
                damage,
                opaque_regions,
            ).map_err(R::Error::from_gles_error),
        }
    }
    
    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        match self {
            SwlElement::Surface(elem) => elem.underlying_storage(renderer),
            SwlElement::Damage(_) => None,  // DamageElement has no underlying storage
            SwlElement::Texture(_) => None, // TextureRenderElement doesn't provide underlying storage for external renderers
            SwlElement::Cursor(elem) => elem.underlying_storage(renderer),
            SwlElement::SolidColor(_) => None, // SolidColorRenderElement has no underlying storage
        }
    }
}

// note: UnderlyingStorage trait implementation would be needed for advanced features
// for now, basic rendering is sufficient