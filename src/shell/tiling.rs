// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    desktop::Window,
    utils::{Logical, Point, Rectangle, Size},
};
use tracing::debug;

// import border width from shell module
use crate::shell::BORDER_WIDTH;
use crate::utils::coordinates::VirtualOutputRelativeRect;

/// Tiling layout implementation inspired by dwm/dwl
#[derive(Debug)]
pub struct TilingLayout {
    /// Width ratio for master area (0.1 to 0.9)
    master_factor: f32,
    
    /// Number of windows in master area
    n_master: usize,
    
    /// Available area for tiling (excluding exclusive zones)
    available_area: VirtualOutputRelativeRect,
}

impl TilingLayout {
    /// Create a new tiling layout with default settings
    pub fn new(available_area: impl Into<VirtualOutputRelativeRect>) -> Self {
        // check environment variables for configuration
        let master_factor = std::env::var("SWL_MASTER_FACTOR")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .map(|f| f.clamp(0.1, 0.9))
            .unwrap_or(0.5);
            
        let n_master = std::env::var("SWL_N_MASTER")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        
        let available_area_rect = available_area.into();
        
        debug!("TilingLayout initialized: master_factor={}, n_master={}, available_area={:?}", 
               master_factor, n_master, available_area_rect);
        
        Self {
            master_factor,
            n_master,
            available_area: available_area_rect,
        }
    }
    
    /// Calculate positions for all windows according to the tiling layout
    /// Returns vec of (Window, Rectangle) for positioning
    pub fn tile(&self, windows: &[Window]) -> Vec<(Window, Rectangle<i32, Logical>)> {
        if windows.is_empty() {
            return Vec::new();
        }
        
        let n = windows.len();
        let mut positions = Vec::with_capacity(n);
        
        // use the available area's position and size
        let area_x = self.available_area.location().as_point().x;
        let area_y = self.available_area.location().as_point().y;
        let area_width = self.available_area.size().w;
        let area_height = self.available_area.size().h;
        
        // calculate space available for windows (excluding all borders)
        let (master_window_width, stack_window_width) = if n > self.n_master {
            // we have 2 columns, so need 3 borders: left, middle, right
            let total_window_space = area_width - 3 * BORDER_WIDTH;
            
            // master gets its portion, rounded up (gets remainder pixel)
            let master_w = ((total_window_space as f32 * self.master_factor).ceil() as i32).max(1);
            let stack_w = (total_window_space - master_w).max(1);
            
            (master_w, stack_w)
        } else {
            // single column, just 2 borders: left and right
            let window_w = area_width - 2 * BORDER_WIDTH;
            (window_w, 0)
        };
        
        // tile master windows (left side)
        let master_count = n.min(self.n_master);
        
        // calculate vertical space for master windows
        let total_height_space = area_height - (master_count + 1) as i32 * BORDER_WIDTH;
        
        for i in 0..master_count {
            // calculate window position
            let x = area_x + BORDER_WIDTH;
            
            // calculate height for this window - first window gets remainder pixels
            let base_height = total_height_space / master_count as i32;
            let remainder = total_height_space % master_count as i32;
            let h = if i == 0 {
                base_height + remainder
            } else {
                base_height
            };
            
            // calculate Y position
            let y = if i == 0 {
                area_y + BORDER_WIDTH
            } else {
                // sum heights of previous windows plus borders
                let mut y_pos = area_y + BORDER_WIDTH;
                for j in 0..i {
                    let prev_h = if j == 0 {
                        base_height + remainder
                    } else {
                        base_height
                    };
                    y_pos += prev_h + BORDER_WIDTH;
                }
                y_pos
            };
            
            let w = master_window_width;
            
            // create virtual-output-relative rectangle for this window
            let rect = Rectangle::new(
                Point::from((x, y)),  // relative to virtual output origin
                Size::from((w.max(1), h.max(1))), // ensure minimum size
            );
            positions.push((windows[i].clone(), rect));
        }
        
        // tile stack windows (right side)
        if n > self.n_master {
            let stack_count = n - self.n_master;
            
            // calculate vertical space for stack windows
            let total_height_space = area_height - (stack_count + 1) as i32 * BORDER_WIDTH;
            
            for i in 0..stack_count {
                let stack_i = i + self.n_master;
                
                // stack X position: master windows + left border + master width + middle border
                let x = area_x + BORDER_WIDTH + master_window_width + BORDER_WIDTH;
                
                // calculate height for this window - first window gets remainder pixels
                let base_height = total_height_space / stack_count as i32;
                let remainder = total_height_space % stack_count as i32;
                let h = if i == 0 {
                    base_height + remainder
                } else {
                    base_height
                };
                
                // calculate Y position
                let y = if i == 0 {
                    area_y + BORDER_WIDTH
                } else {
                    // sum heights of previous windows plus borders
                    let mut y_pos = area_y + BORDER_WIDTH;
                    for j in 0..i {
                        let prev_h = if j == 0 {
                            base_height + remainder
                        } else {
                            base_height
                        };
                        y_pos += prev_h + BORDER_WIDTH;
                    }
                    y_pos
                };
                
                let w = stack_window_width;
                
                // create virtual-output-relative rectangle for this window
                let rect = Rectangle::new(
                    Point::from((x, y)),  // relative to virtual output origin
                    Size::from((w.max(1), h.max(1))), // ensure minimum size
                );
                positions.push((windows[stack_i].clone(), rect));
            }
        }
        
        debug!("Tiled {} windows (master={}, stack={}) in area {:?}", 
               n, master_count, n.saturating_sub(self.n_master), self.available_area);
        positions
    }
    
    /// Adjust the master area width factor
    pub fn set_master_factor(&mut self, delta: f32) {
        self.master_factor = (self.master_factor + delta).clamp(0.1, 0.9);
        debug!("Master factor adjusted to {}", self.master_factor);
    }
    
    /// Adjust the number of master windows
    pub fn inc_n_master(&mut self, delta: i32) {
        if delta > 0 {
            self.n_master = self.n_master.saturating_add(delta as usize);
        } else {
            self.n_master = self.n_master.saturating_sub((-delta) as usize);
        }
        debug!("Master count adjusted to {}", self.n_master);
    }
    
    /// Update the available area (for when output or exclusive zones change)
    pub fn set_available_area(&mut self, area: impl Into<VirtualOutputRelativeRect>) {
        self.available_area = area.into();
        debug!("Available area updated to {:?}", self.available_area);
    }
    
    /// Get current master factor
    #[allow(dead_code)] // will be used for status display
    pub fn master_factor(&self) -> f32 {
        self.master_factor
    }
    
    /// Get current master count
    #[allow(dead_code)] // will be used for status display
    pub fn n_master(&self) -> usize {
        self.n_master
    }
}