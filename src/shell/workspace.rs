// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    desktop::Window,
    output::Output,
    utils::{IsAlive, Logical, Rectangle, Size},
};
use std::collections::{HashMap, HashSet};

use super::tiling::TilingLayout;

/// Tab bar height in pixels
pub const TAB_HEIGHT: i32 = 6;

/// Layout mode for a workspace
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// Traditional tiling with master/stack columns
    Tiling,
    /// Tabbed mode where only one window is visible at a time
    Tabbed,
}

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
    
    /// Cached window rectangles from last tiling arrangement
    pub window_rectangles: HashMap<Window, Rectangle<i32, Logical>>,
    
    /// Cached available area (non-exclusive zone) from last arrangement
    pub available_area: Rectangle<i32, Logical>,
    
    /// Current layout mode
    pub layout_mode: LayoutMode,
    
    /// Active tab index (for tabbed mode)
    pub active_tab_index: usize,
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
            window_rectangles: HashMap::new(),
            available_area: Rectangle::from_size(Size::from((1920, 1080))), // default size
            layout_mode: LayoutMode::Tiling,
            active_tab_index: 0,
        }
    }
    
    /// Add a window to this workspace
    pub fn add_window(&mut self, window: Window, floating: bool) {
        // Check if window already exists
        if self.windows.iter().any(|w| w == &window) {
            tracing::warn!("Window already exists in workspace {}, not adding duplicate", self.name);
            return;
        }
        
        self.windows.push(window.clone());
        if floating {
            self.floating_windows.insert(window);
        }
        // In tabbed mode, new tiled windows become the active tab
        if matches!(self.layout_mode, LayoutMode::Tabbed) && !floating {
            let tiled_count = self.tiled_windows().count();
            self.active_tab_index = tiled_count.saturating_sub(1);
        }
        self.needs_arrange = true;
    }
    
    /// Remove a window from this workspace
    pub fn remove_window(&mut self, window: &Window) -> bool {
        // Check if this was a tiled window and the active tab
        let was_tiled = !self.floating_windows.contains(window);
        let was_active = if was_tiled && matches!(self.layout_mode, LayoutMode::Tabbed) {
            self.tiled_windows().nth(self.active_tab_index).map(|w| w == window).unwrap_or(false)
        } else {
            false
        };
        
        // Remove from windows list
        let original_len = self.windows.len();
        self.windows.retain(|w| w != window);
        let was_present = self.windows.len() < original_len;
        
        // Remove from focus stack
        self.focus_stack.retain(|w| w.alive() && w != window);
        
        // Remove from floating set
        self.floating_windows.remove(window);
        
        // Remove from cached rectangles
        self.window_rectangles.remove(window);
        
        // Clear fullscreen if it was this window
        if self.fullscreen.as_ref() == Some(window) {
            self.fullscreen = None;
        }
        
        // Adjust active_tab_index if needed
        if was_active && matches!(self.layout_mode, LayoutMode::Tabbed) {
            let tiled_count = self.tiled_windows().count();
            if tiled_count > 0 {
                self.active_tab_index = self.active_tab_index.min(tiled_count - 1);
            } else {
                self.active_tab_index = 0;
            }
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
        self.window_rectangles.retain(|w, _| w.alive());
        
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
    
    /// Toggle between tiling and tabbed layout modes
    pub fn toggle_layout_mode(&mut self) {
        match self.layout_mode {
            LayoutMode::Tiling => {
                self.layout_mode = LayoutMode::Tabbed;
                self.active_tab_index = 0;
                // Find index of currently focused window if any
                if let Some(focused) = self.focus_stack.last() {
                    let idx = self.tiled_windows()
                        .enumerate()
                        .find(|(_, w)| *w == focused)
                        .map(|(idx, _)| idx);
                    if let Some(idx) = idx {
                        self.active_tab_index = idx;
                    }
                }
            }
            LayoutMode::Tabbed => {
                self.layout_mode = LayoutMode::Tiling;
                // active_tab_index is ignored in tiling mode
            }
        }
        self.needs_arrange = true;
    }
    
    /// Switch to the next tab in tabbed mode
    pub fn next_tab(&mut self) -> Option<Window> {
        if !matches!(self.layout_mode, LayoutMode::Tabbed) {
            return None;
        }
        
        let tiled: Vec<_> = self.tiled_windows().cloned().collect();
        if tiled.is_empty() {
            return None;
        }
        
        self.active_tab_index = (self.active_tab_index + 1) % tiled.len();
        self.needs_arrange = true;
        
        // Update focus stack to match the active tab
        if let Some(window) = tiled.get(self.active_tab_index) {
            self.append_focus(window);
        }
        
        tiled.get(self.active_tab_index).cloned()
    }
    
    /// Switch to the previous tab in tabbed mode
    pub fn prev_tab(&mut self) -> Option<Window> {
        if !matches!(self.layout_mode, LayoutMode::Tabbed) {
            return None;
        }
        
        let tiled: Vec<_> = self.tiled_windows().cloned().collect();
        if tiled.is_empty() {
            return None;
        }
        
        self.active_tab_index = if self.active_tab_index == 0 {
            tiled.len() - 1
        } else {
            self.active_tab_index - 1
        };
        self.needs_arrange = true;
        
        // Update focus stack to match the active tab
        if let Some(window) = tiled.get(self.active_tab_index) {
            self.append_focus(window);
        }
        
        tiled.get(self.active_tab_index).cloned()
    }
    
    /// Validate workspace consistency
    pub fn validate_consistency(&self) {
        // Check for dead windows
        let dead_count = self.windows.iter().filter(|w| !w.alive()).count();
        if dead_count > 0 {
            tracing::warn!("Workspace {} has {} dead windows", self.name, dead_count);
        }
        
        // Check floating windows are subset of all windows
        for floating in &self.floating_windows {
            if !self.windows.contains(floating) {
                tracing::error!("Workspace {} has floating window not in windows list", self.name);
            }
        }
        
        // Check focus stack is subset of windows
        for focused in &self.focus_stack {
            if !self.windows.contains(focused) {
                tracing::error!("Workspace {} has focus stack window not in windows list", self.name);
            }
        }
        
        // Check active tab index
        if matches!(self.layout_mode, LayoutMode::Tabbed) {
            let tiled_count = self.tiled_windows().count();
            if self.active_tab_index >= tiled_count && tiled_count > 0 {
                tracing::error!("Workspace {} has invalid active_tab_index {} for {} tiled windows", 
                    self.name, self.active_tab_index, tiled_count);
            }
        }
    }
}