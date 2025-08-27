// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point},
};
use std::collections::HashMap;

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
}