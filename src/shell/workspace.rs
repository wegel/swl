// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    desktop::Window,
    output::Output,
    utils::{IsAlive, Logical, Rectangle, Size},
};
use std::collections::HashSet;

use super::tiling::TilingLayout;

/// A workspace containing windows
#[derive(Debug)]
pub struct Workspace {
    /// User-visible name (default "1", "2", etc)
    #[allow(dead_code)] // Will be used for workspace switching/display
    pub name: String,
    
    /// Currently displayed on this output (None = hidden)
    pub output: Option<Output>,
    
    /// Windows in this workspace
    pub windows: Vec<Window>,
    
    /// Fullscreen window (if any)
    pub fullscreen: Option<Window>,
    
    /// Per-workspace focus history
    pub focus_stack: Vec<Window>,
    
    /// Per-workspace tiling state
    pub tiling: TilingLayout,
    
    /// Windows that are floating (exempt from tiling)
    pub floating_windows: HashSet<Window>,
    
    /// Flag indicating windows need re-arrangement
    pub needs_arrange: bool,
}

impl Workspace {
    /// Create a new workspace with the given name
    pub fn new(name: String) -> Self {
        Self {
            name,
            output: None,
            windows: Vec::new(),
            fullscreen: None,
            focus_stack: Vec::new(),
            tiling: TilingLayout::new(Rectangle::from_size(Size::from((1920, 1080)))), // default size
            floating_windows: HashSet::new(),
            needs_arrange: false,
        }
    }
    
    /// Add a window to this workspace
    pub fn add_window(&mut self, window: Window, floating: bool) {
        self.windows.push(window.clone());
        if floating {
            self.floating_windows.insert(window);
        }
        self.needs_arrange = true;
    }
    
    /// Remove a window from this workspace
    pub fn remove_window(&mut self, window: &Window) -> bool {
        // Remove from windows list
        let original_len = self.windows.len();
        self.windows.retain(|w| w != window);
        let was_present = self.windows.len() < original_len;
        
        // Remove from focus stack
        self.focus_stack.retain(|w| w.alive() && w != window);
        
        // Remove from floating set
        self.floating_windows.remove(window);
        
        // Clear fullscreen if it was this window
        if self.fullscreen.as_ref() == Some(window) {
            self.fullscreen = None;
        }
        
        if was_present {
            self.needs_arrange = true;
        }
        
        was_present
    }
    
    /// Get tiled windows (non-floating, non-fullscreen)
    pub fn tiled_windows(&self) -> impl Iterator<Item = &Window> {
        self.windows.iter()
            .filter(|w| !self.floating_windows.contains(w))
            .filter(|w| self.fullscreen.is_none() || self.fullscreen.as_ref() == Some(w))
    }
    
    /// Clean up dead windows
    pub fn refresh(&mut self) {
        self.windows.retain(|w| w.alive());
        self.focus_stack.retain(|w| w.alive());
        self.floating_windows.retain(|w| w.alive());
        
        if let Some(fullscreen) = &self.fullscreen {
            if !fullscreen.alive() {
                self.fullscreen = None;
            }
        }
    }
    
    /// Append window to focus stack, removing any existing occurrence
    pub fn append_focus(&mut self, window: &Window) {
        self.focus_stack.retain(|w| w != window);
        self.focus_stack.push(window.clone());
    }
    
    /// Update the output area for tiling
    pub fn update_output_geometry(&mut self, size: Rectangle<i32, Logical>) {
        self.tiling.set_available_area(size);
        self.needs_arrange = true;
    }
}