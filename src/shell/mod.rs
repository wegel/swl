// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::renderer::{
        element::{AsRenderElements, RenderElementStates},
        ImportAll, ImportMem, Renderer,
    },
    desktop::{
        utils::{
            surface_presentation_feedback_flags_from_states,
            OutputPresentationFeedback,
        },
        Space, Window,
    },
    input::pointer::CursorImageStatus,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    output::Output,
    utils::{Logical, Point, Rectangle, Scale},
};
use std::collections::HashMap;

use crate::backend::render::element::{AsGlowRenderer, CosmicElement};

/// A simple shell for managing windows
pub struct Shell {
    /// The space containing all windows
    pub space: Space<Window>,
    
    /// Active windows indexed by their ID
    pub windows: HashMap<u32, Window>,
    
    /// Next window ID
    next_window_id: u32,
    
    /// The currently focused window
    pub focused_window: Option<Window>,
    
    /// Fullscreen window (if any)
    pub fullscreen_window: Option<Window>,
    
    /// Cursor position (relative to space origin)
    pub cursor_position: Point<f64, Logical>,
    
    /// Cursor image status
    pub cursor_status: CursorImageStatus,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            space: Space::default(),
            windows: HashMap::new(),
            next_window_id: 1,
            focused_window: None,
            fullscreen_window: None,
            // start cursor off-screen to avoid rendering on all outputs at startup
            cursor_position: Point::from((-1000.0, -1000.0)),
            cursor_status: CursorImageStatus::default_named(),
        }
    }
    
    /// Add an output to the shell's space
    pub fn add_output(&mut self, output: &Output) {
        // map the output at origin (we don't support multi-monitor positioning yet)
        self.space.map_output(output, Point::from((0, 0)));
        tracing::info!("Added output {} to shell space", output.name());
    }
    
    /// Add a new window to the shell
    pub fn add_window(&mut self, window: Window, output: &Output) {
        let id = self.next_window_id;
        self.next_window_id += 1;
        
        tracing::info!("Adding window {} to shell", id);
        
        // add to our tracking
        self.windows.insert(id, window.clone());
        
        // map the window to the space
        let output_mode = output.current_mode().expect("Output should have a mode");
        let output_size = output_mode.size;
        let window_geometry = window.geometry();
        let window_size = window_geometry.size;
        
        tracing::info!("Output mode: {:?}, Output size: {:?}", output_mode, output_size);
        tracing::info!("Window geometry: {:?}, Window size: {:?}", window_geometry, window_size);
        
        // center the window on the output for now (no tiling yet)
        // if window has no size yet (0x0), use a default position
        let location = if window_size.w > 0 && window_size.h > 0 {
            let x = (output_size.w - window_size.w) / 2;
            let y = (output_size.h - window_size.h) / 2;
            Point::from((x, y))
        } else {
            // window has no size yet, position at top-left and it will be repositioned later
            tracing::warn!("Window has 0x0 size, using default position");
            Point::from((0, 0))
        };
        
        tracing::info!("Mapping window {} to space at location {:?}", id, location);
        self.space.map_element(window.clone(), location, false);
        
        // set as focused if no window is focused
        if self.focused_window.is_none() {
            self.focused_window = Some(window.clone());
            tracing::debug!("Set window {} as focused", id);
        }
        
        tracing::info!("Window {} added successfully. Total windows: {}", id, self.windows.len());
    }
    
    /// Remove a window from the shell
    pub fn remove_window(&mut self, window: &Window) {
        // find and remove from our tracking
        let mut id_to_remove = None;
        for (id, w) in &self.windows {
            if w == window {
                id_to_remove = Some(*id);
                break;
            }
        }
        
        if let Some(id) = id_to_remove {
            self.windows.remove(&id);
        }
        
        // unmap from space
        self.space.unmap_elem(window);
        
        // update focus if this was the focused window
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = self.windows.values().next().cloned();
        }
    }
    
    /// Get the window under the given point
    pub fn window_under(&self, point: Point<f64, Logical>) -> Option<Window> {
        use tracing::debug;
        
        for window in self.space.elements() {
            // get the window's position in space
            let location = self.space.element_location(window).unwrap_or_default();
            // get the window's bounding box (includes decorations)
            let bbox = window.bbox();
            // translate bbox to global coordinates
            let global_bbox = Rectangle::from_loc_and_size(
                location + bbox.loc,
                bbox.size,
            );
            
            debug!("Checking window bbox at {:?} against point {:?}", global_bbox, point);
            if global_bbox.to_f64().contains(point) {
                debug!("Point is within window bounds!");
                return Some(window.clone());
            }
        }
        debug!("No window found at point {:?}", point);
        None
    }
    
    /// Find the surface under a point (including decorations)
    pub fn surface_under(&self, point: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        use smithay::desktop::WindowSurfaceType;
        use tracing::trace;
        
        trace!("Looking for surface under point: {:?}", point);
        
        // find window containing the point
        for window in self.space.elements() {
            // get the window's position in space  
            let location = self.space.element_location(window).unwrap_or_default();
            // get the window's bounding box (includes decorations)
            let bbox = window.bbox();
            // translate bbox to global coordinates
            let global_bbox = Rectangle::from_loc_and_size(
                location + bbox.loc,
                bbox.size,
            );
            
            trace!("Window bbox: {:?}", global_bbox);
            if global_bbox.to_f64().contains(point) {
                // convert point to window-relative coordinates
                // window.surface_under expects coordinates relative to the window's origin (0,0)
                let window_relative = point - location.to_f64();
                trace!("Window-relative point: {:?}", window_relative);
                
                // check for surface under this point (including decorations)
                if let Some((surface, loc)) = window.surface_under(
                    window_relative,
                    WindowSurfaceType::ALL,
                ) {
                    // convert back to global coordinates (and to f64)
                    let global_loc = (loc + location).to_f64();
                    trace!("Found surface at global location: {:?}", global_loc);
                    return Some((surface, global_loc));
                } else {
                    trace!("No surface found in window at relative point");
                }
            }
        }
        trace!("No window contains the point");
        None
    }
    
    /// Get the current fullscreen window (if any)
    pub fn get_fullscreen(&self) -> Option<&Window> {
        self.fullscreen_window.as_ref()
    }
    
    /// Set a window as fullscreen
    pub fn set_fullscreen(&mut self, window: Window, fullscreen: bool) {
        if fullscreen {
            self.fullscreen_window = Some(window);
        } else if self.fullscreen_window.as_ref() == Some(&window) {
            self.fullscreen_window = None;
        }
    }
    
    /// Refresh the space (needed for damage tracking)
    pub fn refresh(&mut self) {
        self.space.refresh();
    }
    
    /// Find which output a surface is visible on
    pub fn visible_output_for_surface(&self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> Option<&Output> {
        // find the window containing this surface
        tracing::debug!("Looking for output for surface");
        for window in self.space.elements() {
            if window.toplevel().unwrap().wl_surface() == surface {
                // find which output this window is on
                for output in self.space.outputs() {
                    let output_geometry = self.space.output_geometry(output).unwrap();
                    if let Some(window_location) = self.space.element_location(window) {
                        // check if window intersects with output
                        let window_geometry = smithay::utils::Rectangle::from_extremities(
                            window_location,
                            window_location + window.geometry().size,
                        );
                        if output_geometry.overlaps(window_geometry) {
                            return Some(output);
                        }
                    }
                }
            }
        }
        None
    }
    
    /// Check if there are any ongoing animations
    pub fn animations_going(&self) -> bool {
        // we don't have compositor-side animations yet (window movement, fading, etc)
        // client animations are handled through proper frame callbacks in the backend
        false
    }
    
    /// Collect presentation feedback for all surfaces on the given output
    pub fn take_presentation_feedback(
        &self,
        output: &Output,
        render_element_states: &RenderElementStates,
    ) -> OutputPresentationFeedback {
        let mut output_presentation_feedback = OutputPresentationFeedback::new(output);
        
        // collect feedback from all windows on this output
        for window in self.space.elements() {
            // check if window is on this output
            if let Some(window_location) = self.space.element_location(window) {
                let output_geometry = self.space.output_geometry(output).unwrap();
                let window_geometry = smithay::utils::Rectangle::from_extremities(
                    window_location,
                    window_location + window.geometry().size,
                );
                
                if output_geometry.overlaps(window_geometry) {
                    // collect feedback for this window's surface tree
                    window.take_presentation_feedback(
                        &mut output_presentation_feedback,
                        |_surface, _states| {
                            // For now, always return the current output since we're single-GPU
                            // TODO: properly track primary scanout output when we add multi-GPU support
                            Some(output.clone())
                        },
                        |surface, _| {
                            surface_presentation_feedback_flags_from_states(
                                surface,
                                render_element_states,
                            )
                        },
                    );
                }
            }
        }
        
        // TODO: handle layer shell surfaces when we add them
        // TODO: handle override redirect windows when we add them
        
        output_presentation_feedback
    }
    
    /// Get the output at the given position
    pub fn output_at(&self, position: Point<f64, Logical>) -> Option<Output> {
        self.space.outputs().find(|output| {
            let geometry = self.space.output_geometry(output).unwrap();
            geometry.to_f64().contains(position)
        }).cloned()
    }
    
    /// Get render elements for all windows on the given output
    pub fn render_elements<R>(&self, output: &Output, renderer: &mut R) -> Vec<CosmicElement<R>>
    where
        R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + 'static,
    {
        let mut elements = Vec::new();
        let output_scale = Scale::from(output.current_scale().fractional_scale());
        
        let window_count = self.space.elements().count();
        tracing::debug!("Rendering {} windows in space", window_count);
        
        // render all windows in the space
        for window in self.space.elements() {
            if let Some(location) = self.space.element_location(window) {
                tracing::debug!("Window location: {:?}, geometry: {:?}", location, window.geometry());
                
                // get surface render elements and wrap them in CosmicElement
                let surface_elements = window.render_elements(
                    renderer,
                    location.to_physical_precise_round(output_scale),
                    output_scale,
                    1.0, // alpha
                );
                
                tracing::debug!("Window produced {} render elements", surface_elements.len());
                
                // wrap each surface element in CosmicElement::Surface
                elements.extend(
                    surface_elements.into_iter()
                        .map(|elem| CosmicElement::Surface(elem))
                );
            }
        }
        
        tracing::debug!("Total render elements: {}", elements.len());
        elements
    }
}