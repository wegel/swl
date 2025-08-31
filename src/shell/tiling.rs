// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle},
};
use tracing::debug;

/// Tiling layout implementation inspired by dwm/dwl
#[derive(Debug)]
pub struct TilingLayout {
    /// Width ratio for master area (0.1 to 0.9)
    master_factor: f32,
    
    /// Number of windows in master area
    n_master: usize,
    
    /// Available area for tiling (excluding exclusive zones)
    available_area: Rectangle<i32, Logical>,
}

impl TilingLayout {
    /// Create a new tiling layout with default settings
    pub fn new(available_area: Rectangle<i32, Logical>) -> Self {
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
        
        debug!("TilingLayout initialized: master_factor={}, n_master={}, available_area={:?}", 
               master_factor, n_master, available_area);
        
        Self {
            master_factor,
            n_master,
            available_area,
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
        let area_x = self.available_area.loc.x;
        let area_y = self.available_area.loc.y;
        let area_width = self.available_area.size.w;
        let area_height = self.available_area.size.h;
        
        // calculate master area width
        let master_width = if n > self.n_master {
            (area_width as f32 * self.master_factor) as i32
        } else {
            area_width
        };
        
        // tile master windows (left side)
        let mut master_y = 0;
        let master_count = n.min(self.n_master);
        
        for i in 0..master_count {
            let height = (area_height - master_y) / (master_count - i) as i32;
            let rect = Rectangle::new(
                (area_x, area_y + master_y).into(),
                (master_width, height).into(),
            );
            positions.push((windows[i].clone(), rect));
            master_y += height;
        }
        
        // tile stack windows (right side)
        if n > self.n_master {
            let stack_width = area_width - master_width;
            let stack_count = n - self.n_master;
            let mut stack_y = 0;
            
            for i in self.n_master..n {
                let height = (area_height - stack_y) / (stack_count - (i - self.n_master)) as i32;
                let rect = Rectangle::new(
                    (area_x + master_width, area_y + stack_y).into(),
                    (stack_width, height).into(),
                );
                positions.push((windows[i].clone(), rect));
                stack_y += height;
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
    pub fn set_available_area(&mut self, area: Rectangle<i32, Logical>) {
        self.available_area = area;
        debug!("Available area updated to {:?}", area);
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