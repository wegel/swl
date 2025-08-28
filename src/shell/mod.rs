// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::renderer::{
        element::AsRenderElements,
        ImportAll, ImportMem, Renderer,
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point, Scale},
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
}

impl Shell {
    pub fn new() -> Self {
        Self {
            space: Space::default(),
            windows: HashMap::new(),
            next_window_id: 1,
            focused_window: None,
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
        
        // add to our tracking
        self.windows.insert(id, window.clone());
        
        // map the window to the space
        let output_size = output.current_mode().unwrap().size;
        let window_size = window.geometry().size;
        
        // center the window on the output for now (no tiling yet)
        let x = (output_size.w - window_size.w) / 2;
        let y = (output_size.h - window_size.h) / 2;
        let location = Point::from((x, y));
        
        self.space.map_element(window.clone(), location, false);
        
        // set as focused if no window is focused
        if self.focused_window.is_none() {
            self.focused_window = Some(window);
        }
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
        self.space
            .elements()
            .find(|window| {
                let geometry = self.space.element_geometry(window).unwrap();
                geometry.to_f64().contains(point)
            })
            .cloned()
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
    
    /// Get render elements for all windows on the given output
    pub fn render_elements<R>(&self, output: &Output, renderer: &mut R) -> Vec<CosmicElement<R>>
    where
        R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + 'static,
    {
        let mut elements = Vec::new();
        let output_scale = Scale::from(output.current_scale().fractional_scale());
        
        // render all windows in the space
        for window in self.space.elements() {
            if let Some(location) = self.space.element_location(window) {
                // get surface render elements and wrap them in CosmicElement
                let surface_elements = window.render_elements(
                    renderer,
                    location.to_physical_precise_round(output_scale),
                    output_scale,
                    1.0, // alpha
                );
                
                // wrap each surface element in CosmicElement::Surface
                elements.extend(
                    surface_elements.into_iter()
                        .map(|elem| CosmicElement::Surface(elem))
                );
            }
        }
        
        elements
    }
}