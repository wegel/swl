// SPDX-License-Identifier: GPL-3.0-only

use indexmap::IndexMap;
use smithay::utils::{Logical, Physical, Rectangle, Point, Size};
use smithay::output::Output;
use std::collections::HashMap;

use super::workspace::WorkspaceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VirtualOutputId(pub u32);

#[derive(Debug, Clone)]
pub struct VirtualRegion {
    pub physical_output: Output,
    pub logical_rect: Rectangle<i32, Logical>,
}

#[derive(Debug)]
pub struct VirtualOutput {
    pub id: VirtualOutputId,
    pub config: IndexMap<String, Rectangle<i32, Physical>>, // output_name -> rect
    pub regions: Vec<VirtualRegion>,
    pub logical_geometry: Rectangle<i32, Logical>,
    pub active_workspace: Option<WorkspaceId>,  // TODO: Make private once Shell APIs are updated
}

impl VirtualOutput {
    /// Get the currently active workspace ID (read-only access)
    pub fn active_workspace(&self) -> Option<WorkspaceId> {
        self.active_workspace
    }
    
    /// Set the active workspace (package-private, only callable by Shell)
    pub(super) fn set_active_workspace(&mut self, workspace_id: Option<WorkspaceId>) {
        self.active_workspace = workspace_id;
    }
    
    ///
    /// Create a virtual output from a single physical region (split mode)
    pub fn from_split(
        id: VirtualOutputId,
        physical_output: Output,
        physical_rect: Rectangle<i32, Physical>,
    ) -> Self {
        let scale = physical_output.current_scale().fractional_scale();
        
        // convert physical rectangle to logical coordinates
        let logical_x = (physical_rect.loc.x as f64 / scale) as i32;
        let logical_y = (physical_rect.loc.y as f64 / scale) as i32;
        let logical_w = (physical_rect.size.w as f64 / scale) as i32;
        let logical_h = (physical_rect.size.h as f64 / scale) as i32;
        
        
        let logical_rect = Rectangle::new(
            Point::new(logical_x, logical_y),
            Size::new(logical_w, logical_h),
        );
        
        let region = VirtualRegion {
            physical_output: physical_output.clone(),
            logical_rect,
        };
        
        let mut config = IndexMap::new();
        config.insert(physical_output.name(), physical_rect);
        
        Self {
            id,
            config,
            regions: vec![region],
            logical_geometry: logical_rect,
            active_workspace: None,
        }
    }
    
    /// Create a virtual output from multiple regions (merge mode)
    #[allow(dead_code)]
    pub fn from_merge(
        id: VirtualOutputId,
        regions_config: Vec<(Output, Rectangle<i32, Physical>)>,
    ) -> Self {
        let mut config = IndexMap::new();
        let mut regions = Vec::new();
        let mut logical_bounds_min: Point<i32, Logical> = Point::new(i32::MAX, i32::MAX);
        let mut logical_bounds_max: Point<i32, Logical> = Point::new(i32::MIN, i32::MIN);
        
        for (output, physical_rect) in regions_config {
            let scale = output.current_scale().fractional_scale();
            
            // convert physical rectangle to logical coordinates
            let logical_rect = Rectangle::new(
                Point::new(
                    (physical_rect.loc.x as f64 / scale) as i32,
                    (physical_rect.loc.y as f64 / scale) as i32,
                ),
                Size::new(
                    (physical_rect.size.w as f64 / scale) as i32,
                    (physical_rect.size.h as f64 / scale) as i32,
                ),
            );
            
            // track overall logical bounds
            logical_bounds_min.x = logical_bounds_min.x.min(logical_rect.loc.x);
            logical_bounds_min.y = logical_bounds_min.y.min(logical_rect.loc.y);
            logical_bounds_max.x = logical_bounds_max.x.max(logical_rect.loc.x + logical_rect.size.w);
            logical_bounds_max.y = logical_bounds_max.y.max(logical_rect.loc.y + logical_rect.size.h);
            
            config.insert(output.name(), physical_rect);
            regions.push(VirtualRegion {
                physical_output: output,
                logical_rect,
            });
        }
        
        // create the overall logical geometry
        let logical_geometry = if logical_bounds_min.x != i32::MAX {
            Rectangle::new(
                logical_bounds_min,
                Size::new(
                    logical_bounds_max.x - logical_bounds_min.x,
                    logical_bounds_max.y - logical_bounds_min.y,
                ),
            )
        } else {
            // fallback for empty regions
            Rectangle::from_size(Size::from((1920, 1080)))
        };
        
        Self {
            id,
            config,
            regions,
            logical_geometry,
            active_workspace: None,
        }
    }
    
    /// Update geometry when physical outputs change
    #[allow(dead_code)]
    pub fn update_geometry(&mut self) {
        self.regions.clear();
        let _logical_bounds_min: Point<i32, Logical> = Point::new(i32::MAX, i32::MAX);
        let _logical_bounds_max: Point<i32, Logical> = Point::new(i32::MIN, i32::MIN);
        
        for (output_name, _physical_rect) in &self.config {
            // find the physical output by name (we'll need access to outputs for this)
            // for now, we'll leave this as a stub and handle it in the manager
            tracing::warn!("update_geometry needs physical output lookup for {}", output_name);
        }
        
        // note: update logical_geometry is handled in update_all method in the manager
    }
}

pub struct VirtualOutputManager {
    next_id: u32,
    pub virtual_outputs: IndexMap<VirtualOutputId, VirtualOutput>,
    physical_mapping: HashMap<String, Vec<VirtualOutputId>>,
}

impl VirtualOutputManager {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            virtual_outputs: IndexMap::new(),
            physical_mapping: HashMap::new(),
        }
    }
    
    /// Create a default 1:1 virtual output for a new physical output
    pub fn create_default(&mut self, output: &Output) -> VirtualOutputId {
        let id = VirtualOutputId(self.next_id);
        self.next_id += 1;
        
        
        let mode = output.current_mode().unwrap();
        let physical_rect = Rectangle::new(
            Point::new(0, 0),
            Size::new(mode.size.w, mode.size.h),
        );
        
        let virtual_output = VirtualOutput::from_split(
            id,
            output.clone(),
            physical_rect,
        );
        
        
        self.physical_mapping
            .entry(output.name())
            .or_insert_with(Vec::new)
            .push(id);
        
        self.virtual_outputs.insert(id, virtual_output);
        id
    }
    
    /// Update all virtual outputs when physical outputs change
    pub fn update_all(&mut self, physical_outputs: &[Output]) {
        
        // create a lookup map for outputs by name
        let outputs_by_name: HashMap<String, &Output> = physical_outputs
            .iter()
            .map(|o| (o.name(), o))
            .collect();
        
        
        
        // update each virtual output's regions
        for (vout_id, virtual_output) in self.virtual_outputs.iter_mut() {
            virtual_output.regions.clear();
            let mut logical_bounds_min: Point<i32, Logical> = Point::new(i32::MAX, i32::MAX);
            let mut logical_bounds_max: Point<i32, Logical> = Point::new(i32::MIN, i32::MIN);
            
            for (output_name, physical_rect) in &virtual_output.config {
                if let Some(&output) = outputs_by_name.get(output_name) {
                    let scale = output.current_scale().fractional_scale();
                    
                    // convert physical rectangle to logical coordinates
                    let pre_transform_logical = Size::new(
                        (physical_rect.size.w as f64 / scale) as i32,
                        (physical_rect.size.h as f64 / scale) as i32,
                    );
                    
                    
                    // virtual output coordinates are specified post-rotation, pre-scaling
                    // so we only apply scaling, not transform
                    let logical_size = pre_transform_logical;
                    
                    let logical_rect = Rectangle::new(
                        Point::new(
                            (physical_rect.loc.x as f64 / scale) as i32,
                            (physical_rect.loc.y as f64 / scale) as i32,
                        ),
                        logical_size,
                    );
                    
                    // track overall logical bounds
                    logical_bounds_min.x = logical_bounds_min.x.min(logical_rect.loc.x);
                    logical_bounds_min.y = logical_bounds_min.y.min(logical_rect.loc.y);
                    logical_bounds_max.x = logical_bounds_max.x.max(logical_rect.loc.x + logical_rect.size.w);
                    logical_bounds_max.y = logical_bounds_max.y.max(logical_rect.loc.y + logical_rect.size.h);
                    
                    virtual_output.regions.push(VirtualRegion {
                        physical_output: output.clone(),
                        logical_rect,
                    });
                }
            }
            
            // update logical geometry
            if logical_bounds_min.x != i32::MAX {
                let new_geometry = Rectangle::new(
                    logical_bounds_min,
                    Size::new(
                        logical_bounds_max.x - logical_bounds_min.x,
                        logical_bounds_max.y - logical_bounds_min.y,
                    ),
                );
                
                
                virtual_output.logical_geometry = new_geometry;
            } else {
                tracing::warn!("Virtual output {} has invalid bounds, skipping geometry update", vout_id.0);
            }
        }
        
        // rebuild physical mapping
        self.physical_mapping.clear();
        for (id, virtual_output) in &self.virtual_outputs {
            for region in &virtual_output.regions {
                self.physical_mapping
                    .entry(region.physical_output.name())
                    .or_insert_with(Vec::new)
                    .push(*id);
            }
        }
    }
    
    /// Get virtual outputs that overlap with a physical output
    pub fn virtual_outputs_for_physical(&self, output: &Output) -> Vec<&VirtualOutput> {
        self.physical_mapping
            .get(&output.name())
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.virtual_outputs.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }
    
    /// Get a virtual output by ID
    pub fn get(&self, id: VirtualOutputId) -> Option<&VirtualOutput> {
        self.virtual_outputs.get(&id)
    }
    
    /// Get a mutable virtual output by ID
    pub fn get_mut(&mut self, id: VirtualOutputId) -> Option<&mut VirtualOutput> {
        self.virtual_outputs.get_mut(&id)
    }
    
    /// Get all virtual outputs
    pub fn all(&self) -> impl Iterator<Item = &VirtualOutput> {
        self.virtual_outputs.values()
    }
    
    /// Load configuration from environment variable
    pub fn load_config(&mut self, physical_outputs: &[Output]) {
        // example: SWL_VIRTUAL_OUTPUTS="DP-1:0,0,1920x1080;DP-1:1920,0,1920x1080"
        // this would split DP-1 into two 1920x1080 virtual outputs
        
        if let Ok(config) = std::env::var("SWL_VIRTUAL_OUTPUTS") {
            tracing::info!("Loading virtual output config: {}", config);
            
            // clear existing virtual outputs
            self.virtual_outputs.clear();
            self.physical_mapping.clear();
            self.next_id = 1;
            
            // create a lookup map for outputs by name
            let outputs_by_name: HashMap<String, &Output> = physical_outputs
                .iter()
                .map(|o| (o.name(), o))
                .collect();
            
            
            let specs: Vec<&str> = config.split(';').collect();
            
            for spec in specs.iter() {
                
                // parse "output_name:x,y,widthxheight" format
                let parts: Vec<&str> = spec.split(':').collect();
                
                if parts.len() == 2 {
                    let output_name = parts[0];
                    let rect_spec = parts[1];
                    
                    // parse rectangle
                    if let Some(rect) = self.parse_rectangle_spec(rect_spec) {
                        
                        if let Some(&output) = outputs_by_name.get(output_name) {
                            let id = VirtualOutputId(self.next_id);
                            self.next_id += 1;
                            
                            
                            let virtual_output = VirtualOutput::from_split(
                                id,
                                output.clone(),
                                rect,
                            );
                            
                            
                            // add to mapping
                            self.physical_mapping
                                .entry(output.name())
                                .or_insert_with(Vec::new)
                                .push(id);
                            
                            self.virtual_outputs.insert(id, virtual_output);
                            
                            tracing::info!("Created virtual output {} for {}:{:?}", 
                                id.0, output_name, rect);
                        } else {
                            tracing::warn!("Physical output {} not found for virtual output config", output_name);
                        }
                    } else {
                        tracing::warn!("Failed to parse rectangle spec: {}", rect_spec);
                    }
                } else {
                    tracing::warn!("Invalid virtual output spec: {}", spec);
                }
            }
            
        }
    }
    
    /// Parse rectangle specification in format "x,y,widthxheight"
    fn parse_rectangle_spec(&self, spec: &str) -> Option<Rectangle<i32, Physical>> {
        
        let comma_parts: Vec<&str> = spec.split(',').collect();
        
        if comma_parts.len() == 3 {
            // format: x,y,widthxheight
            let x_str = comma_parts[0];
            let y_str = comma_parts[1];
            let size_spec = comma_parts[2];
            
            
            let x = x_str.parse::<i32>().ok()?;
            let y = y_str.parse::<i32>().ok()?;
            
            
            let size_parts: Vec<&str> = size_spec.split('x').collect();
            
            if size_parts.len() == 2 {
                let w_str = size_parts[0];
                let h_str = size_parts[1];
                
                
                let w = w_str.parse::<i32>().ok()?;
                let h = h_str.parse::<i32>().ok()?;
                
                
                let rect = Rectangle::new(
                    Point::new(x, y),
                    Size::new(w, h),
                );
                
                return Some(rect);
            } else {
                tracing::warn!("Invalid size specification '{}', expected 'widthxheight'", size_spec);
            }
        } else {
            tracing::warn!("Invalid rectangle specification '{}', expected 'x,y,widthxheight'", spec);
        }
        None
    }
}