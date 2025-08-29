// simplified cursor implementation adapted from cosmic-comp

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            ImportAll, ImportMem, Renderer,
        },
    },
    input::pointer::{CursorIcon, CursorImageAttributes, CursorImageStatus},
    reexports::wayland_server::protocol::wl_surface,
    render_elements,
    utils::{Buffer as BufferCoords, Logical, Physical, Point, Scale, Size, Transform},
    wayland::compositor::with_states,
};
use std::{collections::HashMap, io::Read, sync::Mutex};
use tracing::warn;
use xcursor::{
    parser::{parse_xcursor, Image},
    CursorTheme,
};

static FALLBACK_CURSOR_DATA: &[u8] = include_bytes!("../../../resources/cursor.rgba");

#[derive(Debug, Clone)]
pub struct Cursor {
    icons: Vec<Image>,
    size: u32,
}

impl Cursor {
    pub fn load(theme: &CursorTheme, shape: CursorIcon, size: u32) -> Cursor {
        let icons = load_icon(theme, shape)
            .map_err(|err| warn!(?err, "Unable to load xcursor, using fallback cursor"))
            .or_else(|_| load_icon(theme, CursorIcon::Default))
            .unwrap_or_else(|_| {
                vec![Image {
                    size: 32,
                    width: 64,
                    height: 64,
                    xhot: 1,
                    yhot: 1,
                    delay: 1,
                    pixels_rgba: Vec::from(FALLBACK_CURSOR_DATA),
                    pixels_argb: vec![], //unused
                }]
            });

        Cursor { icons, size }
    }

    pub fn get_image(&self, scale: u32, millis: u32) -> Image {
        let size = self.size * scale;
        frame(millis, size, &self.icons)
    }
}

fn nearest_images(size: u32, images: &[Image]) -> impl Iterator<Item = &Image> {
    // follow the nominal size of the cursor to choose the nearest
    let nearest_image = images
        .iter()
        .min_by_key(|image| u32::abs_diff(size, image.size))
        .unwrap();

    images.iter().filter(move |image| {
        image.width == nearest_image.width && image.height == nearest_image.height
    })
}

fn frame(mut millis: u32, size: u32, images: &[Image]) -> Image {
    let total = nearest_images(size, images).fold(0, |acc, image| acc + image.delay);

    if total == 0 {
        millis = 0;
    } else {
        millis %= total;
    }

    for img in nearest_images(size, images) {
        if millis <= img.delay {
            return img.clone();
        }
        millis -= img.delay;
    }

    unreachable!()
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("Theme has no default cursor")]
    NoDefaultCursor,
    #[error("Error opening xcursor file: {0}")]
    File(#[from] std::io::Error),
    #[error("Failed to parse XCursor file")]
    Parse,
}

fn load_icon(theme: &CursorTheme, shape: CursorIcon) -> Result<Vec<Image>, Error> {
    let icon_path = theme
        .load_icon(&shape.to_string())
        .ok_or(Error::NoDefaultCursor)?;
    let mut cursor_file = std::fs::File::open(&icon_path)?;
    let mut cursor_data = Vec::new();
    cursor_file.read_to_end(&mut cursor_data)?;
    parse_xcursor(&cursor_data).ok_or(Error::Parse)
}

render_elements! {
    pub CursorRenderElement<R> where R: ImportAll + ImportMem;
    Static=MemoryRenderBufferRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
}

pub fn draw_surface_cursor<R>(
    renderer: &mut R,
    surface: &wl_surface::WlSurface,
    location: Point<f64, Logical>,
    scale: impl Into<Scale<f64>>,
) -> Vec<(CursorRenderElement<R>, Point<i32, Physical>)>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let scale = scale.into();
    let h = with_states(&surface, |states| {
        states
            .data_map
            .get::<Mutex<CursorImageAttributes>>()
            .unwrap()
            .lock()
            .unwrap()
            .hotspot
            .to_physical_precise_round(scale)
    });

    render_elements_from_surface_tree(
        renderer,
        surface,
        location.to_physical(scale).to_i32_round(),
        scale,
        1.0,
        Kind::Cursor,
    )
    .into_iter()
    .map(|elem| (elem, h))
    .collect()
}

pub type CursorState = Mutex<CursorStateInner>;

pub struct CursorStateInner {
    pub current_cursor: Option<CursorIcon>,
    
    cursor_theme: CursorTheme,
    cursor_size: u32,
    
    cursors: HashMap<CursorIcon, Cursor>,
    pub current_image: Option<Image>,
    image_cache: Vec<(Image, MemoryRenderBuffer)>,
}

impl CursorStateInner {
    pub fn get_named_cursor(&mut self, shape: CursorIcon) -> &Cursor {
        self.cursors
            .entry(shape)
            .or_insert_with(|| Cursor::load(&self.cursor_theme, shape, self.cursor_size))
    }

    pub fn size(&self) -> u32 {
        self.cursor_size
    }
}

pub fn load_cursor_env() -> (String, u32) {
    let name = std::env::var("XCURSOR_THEME")
        .ok()
        .unwrap_or_else(|| "default".into());
    let size = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    (name, size)
}

pub fn load_cursor_theme() -> (CursorTheme, u32) {
    let (name, size) = load_cursor_env();
    (CursorTheme::load(&name), size)
}

impl Default for CursorStateInner {
    fn default() -> CursorStateInner {
        let (theme, size) = load_cursor_theme();
        CursorStateInner {
            current_cursor: Some(CursorIcon::Default),
            
            cursor_size: size,
            cursor_theme: theme,
            
            cursors: HashMap::new(),
            current_image: None,
            image_cache: Vec::new(),
        }
    }
}

// simplified draw_cursor that returns the cursor element for software rendering
pub fn draw_cursor<R>(
    renderer: &mut R,
    cursor_state: &mut CursorStateInner,
    cursor_status: &CursorImageStatus,
    location: Point<f64, Logical>,
    scale: Scale<f64>,
    time_millis: u32,
) -> Vec<(CursorRenderElement<R>, Point<i32, Physical>)>
where
    R: Renderer + ImportMem + ImportAll,
    R::TextureId: Send + Clone + 'static,
{
    let named_cursor = cursor_state.current_cursor.or(match cursor_status {
        CursorImageStatus::Named(named_cursor) => Some(*named_cursor),
        _ => None,
    });
    
    if let Some(current_cursor) = named_cursor {
        let integer_scale = scale.x.max(scale.y).ceil() as u32;
        let frame = cursor_state
            .get_named_cursor(current_cursor)
            .get_image(integer_scale, time_millis);
        let actual_scale = (frame.size / cursor_state.size()).max(1);

        let pointer_images = &mut cursor_state.image_cache;
        let maybe_image =
            pointer_images
                .iter()
                .find_map(|(image, texture)| if image == &frame { Some(texture) } else { None });
        let pointer_image = match maybe_image {
            Some(image) => image,
            None => {
                let buffer = MemoryRenderBuffer::from_slice(
                    &frame.pixels_rgba,
                    Fourcc::Argb8888,
                    (frame.width as i32, frame.height as i32),
                    actual_scale as i32,
                    Transform::Normal,
                    None,
                );
                pointer_images.push((frame.clone(), buffer));
                pointer_images.last().map(|(_, i)| i).unwrap()
            }
        };

        let hotspot = Point::<i32, BufferCoords>::from((frame.xhot as i32, frame.yhot as i32))
            .to_logical(
                actual_scale as i32,
                Transform::Normal,
                &Size::from((frame.width as i32, frame.height as i32)),
            );
        cursor_state.current_image = Some(frame);

        vec![(
            CursorRenderElement::Static(
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location.to_physical(scale),
                    &pointer_image,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .expect("Failed to import cursor bitmap"),
            ),
            hotspot.to_physical_precise_round(scale),
        )]
    } else if let CursorImageStatus::Surface(wl_surface) = cursor_status {
        draw_surface_cursor(renderer, wl_surface, location, scale)
    } else {
        Vec::new()
    }
}