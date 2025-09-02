// SPDX-License-Identifier: GPL-3.0-only

pub mod tiling;
pub mod workspace;

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, RenderElementStates,
            solid::{SolidColorRenderElement, SolidColorBuffer},
        },
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
    utils::{IsAlive, Logical, Point, Rectangle, Scale, Size},
};
use std::collections::HashMap;

use crate::backend::render::element::{AsGlowRenderer, CosmicElement};
use self::workspace::Workspace;

// Window border configuration
pub const BORDER_WIDTH: i32 = 1;
const FOCUSED_BORDER_COLOR: [f32; 4] = [0.0, 0.5, 1.0, 1.0]; // bright blue
const UNFOCUSED_BORDER_COLOR: [f32; 4] = [0.0, 0.2, 0.5, 1.0]; // darker blue

/// Determine if a window should float by default
fn should_float_impl(window: &Window) -> bool {
    // Check if window is a dialog
    if let Some(toplevel) = window.toplevel() {
        if let Some(_parent) = toplevel.parent() {
            // Window has a parent, likely a dialog
            return true;
        }
    }
    
    // Could add more checks here based on window size, app_id, etc.
    false
}

/// A simple shell for managing windows
pub struct Shell {
    /// The space containing all windows
    pub space: Space<Window>,
    
    /// All workspaces indexed by name
    pub workspaces: HashMap<String, Workspace>,
    
    /// Active workspace on each output
    pub active_workspaces: HashMap<Output, String>,
    
    /// The currently focused window (global)
    pub focused_window: Option<Window>,
    
    /// Cursor position (relative to space origin)
    pub cursor_position: Point<f64, Logical>,
    
    /// Cursor image status
    pub cursor_status: CursorImageStatus,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            space: Space::default(),
            workspaces: HashMap::new(),
            active_workspaces: HashMap::new(),
            focused_window: None,
            // start cursor off-screen to avoid rendering on all outputs at startup
            cursor_position: Point::from((-1000.0, -1000.0)),
            cursor_status: CursorImageStatus::default_named(),
        }
    }
    
    /// Add an output to the shell's space
    pub fn add_output(&mut self, output: &Output) {
        // map the output at origin (we don't support multi-monitor positioning yet)
        self.space.map_output(output, Point::from((0, 0)));
        
        // Initialize with workspace "1" if no workspace is active on this output
        if !self.active_workspaces.contains_key(output) {
            self.switch_to_workspace(output, "1".to_string());
        }
        
        tracing::info!("Added output {} to shell space", output.name());
    }
    
    /// Add a new window to the shell
    pub fn add_window(&mut self, window: Window, output: &Output) {
        // Get or create active workspace on this output
        let workspace_name = self.active_workspaces.get(output).cloned()
            .unwrap_or_else(|| {
                // No active workspace on this output yet, create workspace "1"
                self.switch_to_workspace(output, "1".to_string());
                "1".to_string()
            });
        
        tracing::debug!("Adding window to workspace {} on output {}", workspace_name, output.name());
        
        // Add to workspace
        if let Some(workspace) = self.workspaces.get_mut(&workspace_name) {
            // Determine if window should be floating
            let floating = should_float_impl(&window);
            
            workspace.add_window(window.clone(), floating);
            
            let windows_count = workspace.windows.len();
            tracing::debug!("Window added successfully to workspace {}. Total windows in workspace: {}", 
                workspace_name, windows_count);
        }
        
        // Map the window to the space
        self.space.map_element(window.clone(), Point::from((0, 0)), false);
        
        // Set as focused
        self.set_focus(window);
    }
    
    /// Move a window to a specific workspace
    pub fn move_window_to_workspace(&mut self, window: Window, workspace_name: String, output: &Output) {
        // First, remove window from all workspaces
        self.remove_window(&window);
        
        // Get or create the target workspace
        let workspace = self.get_or_create_workspace(workspace_name.clone());
        
        // Determine if window should be floating
        let floating = should_float_impl(&window);
        
        // Add window to the specific workspace
        workspace.add_window(window.clone(), floating);
        
        // If this workspace is currently active on the output, map the window
        if self.active_workspaces.get(output) == Some(&workspace_name) {
            self.space.map_element(window.clone(), Point::from((0, 0)), false);
        }
        
        // Set as focused
        self.set_focus(window);
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
            let global_bbox = Rectangle::new(
                location + bbox.loc,
                bbox.size,
            );
            
            //debug!("Checking window bbox at {:?} against point {:?}", global_bbox, point);
            if global_bbox.to_f64().contains(point) {
                //debug!("Point is within window bounds!");
                return Some(window.clone());
            }
        }
        debug!("No window found at point {:?}", point);
        None
    }
    
    /// Find the surface under a point (including decorations)
    pub fn surface_under(&self, point: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        use smithay::desktop::WindowSurfaceType;
        use smithay::wayland::shell::wlr_layer::Layer;
        use tracing::trace;
        
        //trace!("Looking for surface under point: {:?}", point);
        
        // Find which output contains the point
        let output = self.space.outputs().find(|o| {
            self.space.output_geometry(o)
                .map(|geo| geo.to_f64().contains(point))
                .unwrap_or(false)
        })?;
        
        let output_geo = self.space.output_geometry(output).unwrap();
        let layer_map = smithay::desktop::layer_map_for_output(output);
        let relative_point = point - output_geo.loc.to_f64();
        
        // Check layer surfaces in order (front to back)
        // 1. Overlay layer (always on top)
        if let Some(layer) = layer_map.layer_under(Layer::Overlay, relative_point) {
            if let Some(layer_geo) = layer_map.layer_geometry(layer) {
                let layer_relative = relative_point - layer_geo.loc.to_f64();
                if let Some((surface, surf_loc)) = layer.surface_under(layer_relative, WindowSurfaceType::ALL) {
                    let global_loc = surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
                    trace!("Found overlay layer surface at {:?}", global_loc);
                    return Some((surface, global_loc));
                }
            }
        }
        
        // 2. Top layer (above windows)
        if let Some(layer) = layer_map.layer_under(Layer::Top, relative_point) {
            if let Some(layer_geo) = layer_map.layer_geometry(layer) {
                let layer_relative = relative_point - layer_geo.loc.to_f64();
                if let Some((surface, surf_loc)) = layer.surface_under(layer_relative, WindowSurfaceType::ALL) {
                    let global_loc = surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
                    trace!("Found top layer surface at {:?}", global_loc);
                    return Some((surface, global_loc));
                }
            }
        }
        
        // 3. Windows
        for window in self.space.elements() {
            // get the window's position in space  
            let location = self.space.element_location(window).unwrap_or_default();
            // get the window's bounding box (includes decorations)
            let bbox = window.bbox();
            // translate bbox to global coordinates
            let global_bbox = Rectangle::new(
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
        
        // 4. Bottom layer (below windows)
        if let Some(layer) = layer_map.layer_under(Layer::Bottom, relative_point) {
            if let Some(layer_geo) = layer_map.layer_geometry(layer) {
                let layer_relative = relative_point - layer_geo.loc.to_f64();
                if let Some((surface, surf_loc)) = layer.surface_under(layer_relative, WindowSurfaceType::ALL) {
                    let global_loc = surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
                    trace!("Found bottom layer surface at {:?}", global_loc);
                    return Some((surface, global_loc));
                }
            }
        }
        
        // 5. Background layer (bottommost)
        if let Some(layer) = layer_map.layer_under(Layer::Background, relative_point) {
            if let Some(layer_geo) = layer_map.layer_geometry(layer) {
                let layer_relative = relative_point - layer_geo.loc.to_f64();
                if let Some((surface, surf_loc)) = layer.surface_under(layer_relative, WindowSurfaceType::ALL) {
                    let global_loc = surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
                    trace!("Found background layer surface at {:?}", global_loc);
                    return Some((surface, global_loc));
                }
            }
        }
        
        trace!("No surface found under point");
        None
    }
    
    /// Get the current fullscreen window (if any) for the given output
    pub fn get_fullscreen(&self, output: &Output) -> Option<&Window> {
        self.active_workspace(output)
            .and_then(|ws| ws.fullscreen.as_ref())
    }
    
    /// Set a window as fullscreen
    pub fn set_fullscreen(&mut self, window: Window, fullscreen: bool, output: &Output) {
        if let Some(workspace) = self.active_workspace_mut(output) {
            if fullscreen {
                workspace.fullscreen = Some(window);
            } else if workspace.fullscreen.as_ref() == Some(&window) {
                workspace.fullscreen = None;
            }
            workspace.needs_arrange = true;
        }
        
        // Arrange windows after fullscreen change
        self.arrange_windows_on_output(output);
    }
    
    
    /// Refresh the space (needed for damage tracking)
    pub fn refresh(&mut self) {
        self.space.refresh();
    }
    
    /// Find which output a surface is visible on
    pub fn visible_output_for_surface(&self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> Option<&Output> {
        // find the window containing this surface
        // tracing::debug!("Looking for output for surface");
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
    
    /// Get render elements for all windows and layer surfaces on the given output
    pub fn render_elements<R>(&self, output: &Output, renderer: &mut R) -> Vec<CosmicElement<R>>
    where
        R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + 'static,
    {
        let mut elements = Vec::new();
        let output_scale = Scale::from(output.current_scale().fractional_scale());
        
        use smithay::wayland::shell::wlr_layer::Layer;
        
        // Check if we have a focused fullscreen window on this output
        let has_focused_fullscreen = self.active_workspace(output)
            .and_then(|ws| ws.fullscreen.as_ref())
            .map(|fullscreen_window| {
                // Check if the fullscreen window is the currently focused window
                self.focused_window.as_ref() == Some(fullscreen_window)
            })
            .unwrap_or(false);
        
        // render layer surfaces in correct order
        let layer_map = smithay::desktop::layer_map_for_output(output);
        let layers: Vec<_> = layer_map.layers().cloned().collect();
        
        // elements should be in front-to-back order for smithay's damage tracker
        // (first element is topmost, last element is bottommost)
        
        // 1. Overlay layers always render (topmost)
        for layer_surface in &layers {
            let layer = layer_surface.layer();
            if layer == Layer::Overlay {
                if let Some(geometry) = layer_map.layer_geometry(layer_surface) {
                    let surface_elements = layer_surface.render_elements(
                        renderer,
                        geometry.loc.to_physical_precise_round(output_scale),
                        output_scale,
                        1.0, // alpha
                    );
                    
                    elements.extend(
                        surface_elements.into_iter()
                            .map(|elem| CosmicElement::Surface(elem))
                    );
                }
            }
        }
        
        // 2. Top layers - skip if there's a focused fullscreen window
        if !has_focused_fullscreen {
            for layer_surface in &layers {
                let layer = layer_surface.layer();
                if layer == Layer::Top {
                    if let Some(geometry) = layer_map.layer_geometry(layer_surface) {
                        let surface_elements = layer_surface.render_elements(
                            renderer,
                            geometry.loc.to_physical_precise_round(output_scale),
                            output_scale,
                            1.0, // alpha
                        );
                        
                        elements.extend(
                            surface_elements.into_iter()
                                .map(|elem| CosmicElement::Surface(elem))
                        );
                    }
                }
            }
        }
        
        // 2. Windows (in the middle) - only from active workspace
        if let Some(workspace) = self.active_workspace(output) {
            
            // When there's a focused fullscreen window, only render that window
            if has_focused_fullscreen {
                if let Some(fullscreen_window) = &workspace.fullscreen {
                    if let Some(location) = self.space.element_location(fullscreen_window) {
                        // Render only the fullscreen window
                        let surface_elements = fullscreen_window.render_elements(
                            renderer,
                            location.to_physical_precise_round(output_scale),
                            output_scale,
                            1.0, // alpha
                        );
                        
                        elements.extend(
                            surface_elements.into_iter()
                                .map(|elem| CosmicElement::Surface(elem))
                        );
                    }
                }
            } else {
                // Normal rendering for all windows when not in focused fullscreen
                // First, collect all windows to render
                let mut window_elements = Vec::new();
                let mut focused_window_rect = None;
                
                for window in &workspace.windows {
                    if let Some(location) = self.space.element_location(window) {
                        // get surface render elements and wrap them in CosmicElement
                        let surface_elements = window.render_elements(
                            renderer,
                            location.to_physical_precise_round(output_scale),
                            output_scale,
                            1.0, // alpha
                        );
                        
                        // wrap each surface element in CosmicElement::Surface
                        window_elements.extend(
                            surface_elements.into_iter()
                                .map(|elem| CosmicElement::Surface(elem))
                        );
                        
                        // Track focused window rectangle for border rendering
                        if self.focused_window.as_ref() == Some(window) && !workspace.floating_windows.contains(window) {
                            if let Some(rect) = workspace.window_rectangles.get(window) {
                                if rect.size.w > 0 && rect.size.h > 0 {
                                    // Use the intended rectangle location, not the actual window location
                                    focused_window_rect = Some((rect.loc, rect.size));
                                }
                            }
                        }
                    }
                }
                
                // Add window elements first (they will render behind borders in front-to-back order)
                elements.extend(window_elements);
                
                // Render tab bar if in tabbed mode
                if matches!(workspace.layout_mode, workspace::LayoutMode::Tabbed) {
                let tiled: Vec<_> = workspace.tiled_windows().cloned().collect();
                if !tiled.is_empty() {
                    let area = workspace.available_area;
                    let separator_color = [0.1, 0.1, 0.1, 1.0]; // Dark gray separator
                    
                    // Render individual tab sections with separators
                    let tab_width = area.size.w / tiled.len() as i32;
                    for (i, _window) in tiled.iter().enumerate() {
                        let is_active = i == workspace.active_tab_index;
                        let color = if is_active {
                            FOCUSED_BORDER_COLOR  // Bright blue for active
                        } else {
                            UNFOCUSED_BORDER_COLOR  // Darker blue for inactive
                        };
                        
                        let tab_x = area.loc.x + (i as i32 * tab_width);
                        
                        // Calculate actual tab width (accounting for separator)
                        let actual_tab_width = if i < tiled.len() - 1 {
                            tab_width - 2  // Leave space for 2-pixel separator
                        } else {
                            tab_width  // Last tab takes remaining space
                        };
                        
                        // Render the tab
                        let tab_buffer = SolidColorBuffer::new(
                            (actual_tab_width, workspace::TAB_HEIGHT),
                            color,
                        );
                        let tab_element = SolidColorRenderElement::from_buffer(
                            &tab_buffer,
                            Point::from((tab_x, area.loc.y)).to_physical_precise_round(output_scale),
                            output_scale,
                            1.0,
                            smithay::backend::renderer::element::Kind::Unspecified,
                        );
                        elements.push(CosmicElement::SolidColor(tab_element));
                        
                        // Render separator after this tab (except for the last tab)
                        if i < tiled.len() - 1 {
                            let sep_buffer = SolidColorBuffer::new(
                                (2, workspace::TAB_HEIGHT),
                                separator_color,
                            );
                            let sep_element = SolidColorRenderElement::from_buffer(
                                &sep_buffer,
                                Point::from((tab_x + actual_tab_width, area.loc.y)).to_physical_precise_round(output_scale),
                                output_scale,
                                1.0,
                                smithay::backend::renderer::element::Kind::Unspecified,
                            );
                            elements.push(CosmicElement::SolidColor(sep_element));
                        }
                    }
                }
            }
            
            // Then render borders on top
            
            // 1. Focused window border overlay
            if let Some((location, rect_size)) = focused_window_rect {
                let border_buffer = SolidColorBuffer::new(
                    (rect_size.w + 2 * BORDER_WIDTH, rect_size.h + 2 * BORDER_WIDTH),
                    FOCUSED_BORDER_COLOR
                );
                let border_element = SolidColorRenderElement::from_buffer(
                    &border_buffer,
                    (location - Point::from((BORDER_WIDTH, BORDER_WIDTH))).to_physical_precise_round(output_scale),
                    output_scale,
                    1.0,
                    smithay::backend::renderer::element::Kind::Unspecified,
                );
                elements.push(CosmicElement::SolidColor(border_element));
            }
            
            // 2. Background with unfocused border color for the entire tiling area
            if !workspace.windows.is_empty() {
                let available_area = workspace.available_area;
                if available_area.size.w > 0 && available_area.size.h > 0 {
                    let background_buffer = SolidColorBuffer::new(
                        (available_area.size.w, available_area.size.h),
                        UNFOCUSED_BORDER_COLOR
                    );
                    let background_element = SolidColorRenderElement::from_buffer(
                        &background_buffer,
                        available_area.loc.to_physical_precise_round(output_scale),
                        output_scale,
                        1.0,
                        smithay::backend::renderer::element::Kind::Unspecified,
                    );
                    elements.push(CosmicElement::SolidColor(background_element));
                }
                }
            } // End of !has_focused_fullscreen block
        }
        
        // 3. Background and Bottom layers (bottommost, behind windows)
        // Skip Bottom layer if there's a focused fullscreen window, but always render Background
        for layer_surface in &layers {
            let layer = layer_surface.layer();
            if layer == Layer::Background || (layer == Layer::Bottom && !has_focused_fullscreen) {
                if let Some(geometry) = layer_map.layer_geometry(layer_surface) {
                    let surface_elements = layer_surface.render_elements(
                        renderer,
                        geometry.loc.to_physical_precise_round(output_scale),
                        output_scale,
                        1.0, // alpha
                    );
                    
                    elements.extend(
                        surface_elements.into_iter()
                            .map(|elem| CosmicElement::Surface(elem))
                    );
                }
            }
        }
        
        
        elements
    }
    
    /// Arrange windows on all outputs
    #[allow(dead_code)] // Will be used when we handle multi-output scenarios
    pub fn arrange(&mut self) {
        // Arrange windows on each output
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        for output in outputs {
            self.arrange_windows_on_output(&output);
        }
    }
    
    /// Legacy arrange method (removed)
//     #[allow(dead_code)]
//     fn arrange_old(&mut self) {
//         // Method removed - use arrange_windows_on_output instead
//         unimplemented!("Use arrange_windows_on_output instead")
//         // handle fullscreen window first
//         if let Some(fullscreen_window) = self.fullscreen_window.as_ref() {
//             // get the output size for fullscreen
//             let output = self.space.outputs().next();
//             if let Some(output) = output {
//                 let output_size = output.current_mode()
//                     .map(|mode| {
//                         let scale = output.current_scale().fractional_scale();
//                         Size::from((
//                             (mode.size.w as f64 / scale) as i32,
//                             (mode.size.h as f64 / scale) as i32,
//                         ))
//                     })
//                     .unwrap_or_else(|| (1920, 1080).into());
//                 
//                 // position fullscreen window at origin with full output size
//                 self.space.map_element(fullscreen_window.clone(), Point::from((0, 0)), false);
//                 
//                 if let Some(toplevel) = fullscreen_window.toplevel() {
//                     use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
//                     
//                     toplevel.with_pending_state(|state| {
//                         state.size = Some(output_size);
//                         state.bounds = Some(output_size);
//                         state.states.set(State::Fullscreen);
//                         
//                         // remove tiled states
//                         state.states.unset(State::TiledLeft);
//                         state.states.unset(State::TiledRight);
//                         state.states.unset(State::TiledTop);
//                         state.states.unset(State::TiledBottom);
//                     });
//                     
//                     if toplevel.is_initial_configure_sent() {
//                         toplevel.send_configure();
//                     }
//                 }
//                 
//                 tracing::debug!("Positioned fullscreen window at full output size: {:?}", output_size);
//             }
//         }
//         
//         // get the non-exclusive zone for tiling
//         let available_area = if let Some(output) = self.space.outputs().next() {
//             let layer_map = smithay::desktop::layer_map_for_output(output);
//             layer_map.non_exclusive_zone()
//         } else {
//             // fallback to full screen if no output
//             Rectangle::from_size(Size::from((1920, 1080)))
//         };
//         
//         // update tiling layout with the available area
//         self.tiling.set_available_area(available_area);
//         
//         // collect windows to tile (non-floating, non-fullscreen)
//         let mut windows_to_tile = Vec::new();
//         
//         // check if we have a window that just exited fullscreen
//         let unfullscreened_window = self.just_unfullscreened.take();
//         if unfullscreened_window.is_some() {
//             tracing::debug!("Window just exited fullscreen, restore index: {:?}", self.fullscreen_restore_index);
//         }
//         
//         for window in self.space.elements() {
//             if !self.floating_windows.contains(window) 
//                 && self.fullscreen_window.as_ref() != Some(window) {
//                 // skip the unfullscreened window for now
//                 if unfullscreened_window.as_ref() == Some(window) {
//                     continue;
//                 }
//                 windows_to_tile.push(window.clone());
//             }
//         }
//         
//         // if we have a window that just exited fullscreen, insert it at the saved position
//         if let Some(window) = unfullscreened_window {
//             if let Some(index) = self.fullscreen_restore_index.take() {
//                 // clamp index to valid range in case windows were closed
//                 let insert_pos = index.min(windows_to_tile.len());
//                 windows_to_tile.insert(insert_pos, window.clone());
//                 tracing::debug!("Restored unfullscreened window to position {}", insert_pos);
//             } else {
//                 // no saved index, add it at the end
//                 windows_to_tile.push(window.clone());
//                 tracing::debug!("No restore index, added unfullscreened window at end");
//             }
//             
//             // clear the restore geometry as we've handled it
//             self.fullscreen_restore = None;
//         }
//         
//         // get tile positions
//         let positions = self.tiling.tile(&windows_to_tile);
//         
//         // apply positions and sizes
//         for (window, rect) in positions {
//             // position the window
//             self.space.map_element(window.clone(), rect.loc, false);
//             
//             // resize the window if it has a toplevel surface
//             if let Some(toplevel) = window.toplevel() {
//                 use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
//                 use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
//                 
//                 toplevel.with_pending_state(|state| {
//                     state.size = Some(rect.size);
//                     state.bounds = Some(rect.size);
//                     
//                     // force server-side decorations (no client decorations)
//                     state.decoration_mode = Some(Mode::ServerSide);
//                     
//                     // set tiled states to remove decorations and inform the client
//                     state.states.set(State::TiledLeft);
//                     state.states.set(State::TiledRight);
//                     state.states.set(State::TiledTop);
//                     state.states.set(State::TiledBottom);
//                     
//                     // remove maximized/fullscreen states if present
//                     state.states.unset(State::Maximized);
//                     state.states.unset(State::Fullscreen);
//                 });
//                 
//                 // only send configure if initial configure was already sent
//                 if toplevel.is_initial_configure_sent() {
//                     toplevel.send_configure();
//                 }
//             }
//         }
//         
//         tracing::debug!("Arranged {} windows", windows_to_tile.len());
//         
//         // no need to send frame callbacks here - the render loop will handle that
//     }
    
    /// Toggle floating state for a window
    pub fn toggle_floating(&mut self, window: &Window, output: &Output) {
        if let Some(workspace) = self.active_workspace_mut(output) {
            if workspace.floating_windows.contains(window) {
                workspace.floating_windows.remove(window);
                tracing::debug!("Window no longer floating");
            } else {
                workspace.floating_windows.insert(window.clone());
                // Remove cached rectangle since it's now floating
                workspace.window_rectangles.remove(window);
                
                // clear tiled states when window becomes floating
                if let Some(toplevel) = window.toplevel() {
                    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
                    use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
                    
                    toplevel.with_pending_state(|state| {
                        // restore client-side decorations
                        state.decoration_mode = Some(Mode::ClientSide);
                        
                        state.states.unset(State::TiledLeft);
                        state.states.unset(State::TiledRight);
                        state.states.unset(State::TiledTop);
                        state.states.unset(State::TiledBottom);
                    });
                    
                    if toplevel.is_initial_configure_sent() {
                        toplevel.send_configure();
                    }
                }
                
                tracing::debug!("Window set to floating");
            }
            workspace.needs_arrange = true;
        }
        
        self.arrange_windows_on_output(output);
    }
    
    /// Zoom - swap focused window with first master window
    pub fn zoom(&mut self, output: &Output) {
        if let Some(focused) = self.focused_window.clone() {
            if let Some(workspace) = self.active_workspace_mut(output) {
                // find focused window in workspace
                if let Some(pos) = workspace.windows.iter().position(|w| w == &focused) {
                    if pos > 0 && !workspace.floating_windows.contains(&focused) {
                        // swap with first position
                        workspace.windows.swap(0, pos);
                        workspace.needs_arrange = true;
                        tracing::debug!("Zoomed window to master");
                        
                        self.arrange_windows_on_output(output);
                    }
                }
            }
        }
    }
    
    /// Focus the next window in visual/tiling order (like dwm/dwl)
    pub fn focus_next(&mut self, output: &Output) {
        // Get workspace name for the output
        let workspace_name = match self.active_workspaces.get(output).cloned() {
            Some(name) => name,
            None => return,
        };
        
        // Get next window in visual order
        let next_window = {
            let workspace = match self.workspaces.get_mut(&workspace_name) {
                Some(ws) => ws,
                None => return,
            };
            
            // clean up dead windows first
            workspace.refresh();
            
            // Get tiled windows only (non-floating)
            let tiled_windows: Vec<&Window> = workspace.windows.iter()
                .filter(|w| !workspace.floating_windows.contains(w))
                .collect();
            
            if tiled_windows.is_empty() {
                tracing::debug!("No tiled windows to cycle");
                return;
            }
            
            if tiled_windows.len() == 1 {
                tracing::debug!("Only one tiled window, nothing to cycle");
                return;
            }
            
            // Find current window position in visual order
            let current_pos = self.focused_window.as_ref()
                .and_then(|focused| tiled_windows.iter().position(|w| *w == focused));
            
            let next_window = if let Some(pos) = current_pos {
                // Move to next window, wrapping around
                let next_pos = (pos + 1) % tiled_windows.len();
                tracing::debug!("Switching focus from position {} to {} (of {} tiled windows)", 
                    pos, next_pos, tiled_windows.len());
                Some(tiled_windows[next_pos].clone())
            } else {
                // No current focus or focused window is floating, focus first tiled window
                tracing::debug!("Focusing first tiled window");
                Some(tiled_windows[0].clone())
            };
            
            next_window
        };
        
        // Update focus
        if let Some(next_window) = next_window {
            self.set_focus(next_window);
        }
    }
    
    /// Focus the previous window in visual/tiling order (like dwm/dwl)
    pub fn focus_prev(&mut self, output: &Output) {
        // Get workspace name for the output
        let workspace_name = match self.active_workspaces.get(output).cloned() {
            Some(name) => name,
            None => return,
        };
        
        // Get previous window in visual order
        let prev_window = {
            let workspace = match self.workspaces.get_mut(&workspace_name) {
                Some(ws) => ws,
                None => return,
            };
            
            // clean up dead windows first
            workspace.refresh();
            
            // Get tiled windows only (non-floating)
            let tiled_windows: Vec<&Window> = workspace.windows.iter()
                .filter(|w| !workspace.floating_windows.contains(w))
                .collect();
            
            if tiled_windows.is_empty() {
                tracing::debug!("No tiled windows to cycle");
                return;
            }
            
            if tiled_windows.len() == 1 {
                tracing::debug!("Only one tiled window, nothing to cycle");
                return;
            }
            
            // Find current window position in visual order
            let current_pos = self.focused_window.as_ref()
                .and_then(|focused| tiled_windows.iter().position(|w| *w == focused));
            
            let prev_window = if let Some(pos) = current_pos {
                // Move to previous window, wrapping around
                let prev_pos = if pos == 0 { tiled_windows.len() - 1 } else { pos - 1 };
                tracing::debug!("Switching focus from position {} to {} (of {} tiled windows)", 
                    pos, prev_pos, tiled_windows.len());
                Some(tiled_windows[prev_pos].clone())
            } else {
                // No current focus or focused window is floating, focus last tiled window
                tracing::debug!("Focusing last tiled window");
                Some(tiled_windows[tiled_windows.len() - 1].clone())
            };
            
            prev_window
        };
        
        // Update focus
        if let Some(prev_window) = prev_window {
            self.set_focus(prev_window);
            tracing::debug!("Focused previous window");
        }
    }
    
    /// Close the focused window
    pub fn close_focused(&mut self) {
        if let Some(window) = self.focused_window.clone() {
            if let Some(surface) = window.toplevel() {
                surface.send_close();
                tracing::info!("Sent close request to focused window");
            } else {
                tracing::warn!("Focused window has no toplevel surface");
            }
        } else {
            tracing::warn!("No focused window to close");
        }
    }
    
    /// Refresh focus to the topmost window in the focus stack
    /// Called when layer surfaces are destroyed or focus needs updating
    pub fn refresh_focus(&mut self) -> Option<Window> {
        // Find the topmost alive window from any visible workspace
        // We collect all focus stacks and then look for the last alive window
        let mut all_windows = Vec::new();
        for ws_name in self.active_workspaces.values() {
            if let Some(workspace) = self.workspaces.get(ws_name) {
                all_windows.extend(workspace.focus_stack.iter().cloned());
            }
        }
        
        let focused = all_windows.into_iter()
            .rev()
            .find(|w| w.alive());
        
        self.focused_window = focused.clone();
        
        if focused.is_some() {
            tracing::debug!("Refreshed focus to window from focus stack");
        } else {
            tracing::debug!("No alive window in focus stack to focus");
        }
        
        focused
    }
    
    /// Set keyboard focus to a window
    pub fn set_focus(&mut self, window: Window) {
        self.focused_window = Some(window.clone());
        
        // Update the focus stack in the window's workspace
        for workspace in self.workspaces.values_mut() {
            if workspace.windows.contains(&window) {
                workspace.append_focus(&window);
                
                // In tabbed mode, also update active_tab_index to match focused window
                if matches!(workspace.layout_mode, workspace::LayoutMode::Tabbed) {
                    let idx = workspace.tiled_windows()
                        .enumerate()
                        .find(|(_, w)| *w == &window)
                        .map(|(idx, _)| idx);
                    if let Some(idx) = idx {
                        workspace.active_tab_index = idx;
                    }
                }
                
                break;
            }
        }
    }
    
    // ========== Workspace Management ==========
    
    /// Get or create a workspace with the given name
    pub fn get_or_create_workspace(&mut self, name: String) -> &mut Workspace {
        self.workspaces.entry(name.clone()).or_insert_with(|| {
            tracing::info!("Creating new workspace: {}", name);
            Workspace::new(name)
        })
    }
    
    /// Get the active workspace for an output
    pub fn active_workspace(&self, output: &Output) -> Option<&Workspace> {
        self.active_workspaces.get(output)
            .and_then(|name| self.workspaces.get(name))
    }
    
    /// Get the active workspace for an output (mutable)
    pub fn active_workspace_mut(&mut self, output: &Output) -> Option<&mut Workspace> {
        self.active_workspaces.get(output)
            .cloned()
            .and_then(move |name| self.workspaces.get_mut(&name))
    }
    
    /// Switch to a workspace on the given output
    pub fn switch_to_workspace(&mut self, output: &Output, name: String) {
        tracing::info!("Switching to workspace {} on output {}", name, output.name());
        
        // Hide current workspace
        if let Some(current_name) = self.active_workspaces.get(output).cloned() {
            if current_name == name {
                tracing::debug!("Already on workspace {}", name);
                return;
            }
            
            if let Some(current) = self.workspaces.get_mut(&current_name) {
                tracing::debug!("Hiding workspace {} with {} windows", current_name, current.windows.len());
                for window in &current.windows {
                    self.space.unmap_elem(window);
                }
                current.output = None;
            }
        }
        
        // Get workspace info we need
        let (other_output_to_remove, windows_to_map, focus_target, _is_tabbed) = {
            let workspace = self.get_or_create_workspace(name.clone());
            
            // Check if workspace was on another output
            let other_output = if let Some(other) = &workspace.output {
                if other != output {
                    Some(other.clone())
                } else {
                    None
                }
            } else {
                None
            };
            
            // Update workspace geometry based on output
            let layer_map = smithay::desktop::layer_map_for_output(output);
            let available_area = layer_map.non_exclusive_zone();
            workspace.update_output_geometry(available_area);
            
            // Set new output
            workspace.output = Some(output.clone());
            
            // Get windows and focus target
            let windows = workspace.windows.clone();
            let focus = workspace.focus_stack.last().cloned();
            let is_tabbed = matches!(workspace.layout_mode, workspace::LayoutMode::Tabbed);
            
            // In tabbed mode, ensure active_tab_index is synchronized with the focused window
            if is_tabbed && focus.is_some() {
                let focused_window = focus.as_ref().unwrap();
                let idx = workspace.tiled_windows()
                    .enumerate()
                    .find(|(_, w)| *w == focused_window)
                    .map(|(idx, _)| idx);
                if let Some(idx) = idx {
                    workspace.active_tab_index = idx;
                }
            }
            
            (other_output, windows, focus, is_tabbed)
        };
        
        // Remove from other output if needed
        if let Some(other_output) = other_output_to_remove {
            tracing::debug!("Workspace {} was on output {}, removing it", name, other_output.name());
            self.active_workspaces.remove(&other_output);
        }
        
        tracing::debug!("Showing workspace {} with {} windows", name, windows_to_map.len());
        for window in windows_to_map {
            self.space.map_element(window, (0, 0), false);
        }
        
        // Update active workspace mapping
        self.active_workspaces.insert(output.clone(), name);
        
        // Restore focus
        if let Some(window) = focus_target {
            if window.alive() {
                self.set_focus(window);
            } else {
                self.focused_window = None;
            }
        } else {
            self.focused_window = None;
        }
        
        // Arrange windows in the new workspace
        self.arrange_windows_on_output(output);
    }
    
    /// Arrange windows on the given output according to the tiling layout
    pub fn arrange_windows_on_output(&mut self, output: &Output) {
        // Get the active workspace for this output
        let workspace_name = match self.active_workspaces.get(output).cloned() {
            Some(name) => name,
            None => {
                tracing::warn!("No active workspace on output {}", output.name());
                return; // No active workspace on this output
            }
        };
        
        let workspace = match self.workspaces.get_mut(&workspace_name) {
            Some(ws) => ws,
            None => return,
        };
        
        // Calculate and cache the available area
        let layer_map = smithay::desktop::layer_map_for_output(output);
        let available_area = layer_map.non_exclusive_zone();
        workspace.available_area = available_area;
        
        // Update the tiling layout with the available area
        workspace.update_output_geometry(available_area);
        
        // Clean up dead windows first
        workspace.refresh();
        
        // Validate workspace consistency for debugging
        workspace.validate_consistency();
        
        // Handle fullscreen window first
        if let Some(fullscreen_window) = &workspace.fullscreen {
            let output_size = output.current_mode()
                .map(|mode| {
                    let scale = output.current_scale().fractional_scale();
                    Size::from((
                        (mode.size.w as f64 / scale) as i32,
                        (mode.size.h as f64 / scale) as i32,
                    ))
                })
                .unwrap_or_else(|| (1920, 1080).into());
            
            // Position fullscreen window at origin with full output size
            self.space.map_element(fullscreen_window.clone(), Point::from((0, 0)), false);
            
            if let Some(toplevel) = fullscreen_window.toplevel() {
                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
                
                toplevel.with_pending_state(|state| {
                    state.size = Some(output_size);
                    state.bounds = Some(output_size);
                    state.states.set(State::Fullscreen);
                    
                    // Remove tiled states
                    state.states.unset(State::TiledLeft);
                    state.states.unset(State::TiledRight);
                    state.states.unset(State::TiledTop);
                    state.states.unset(State::TiledBottom);
                });
                
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_configure();
                }
            }
            
            workspace.needs_arrange = false;  // Clear the flag even for fullscreen
            return; // Don't arrange other windows when one is fullscreen
        }
        
        // Get tiled windows
        let windows_to_tile: Vec<_> = workspace.tiled_windows().cloned().collect();
        
        match workspace.layout_mode {
            workspace::LayoutMode::Tiling => {
                // Get tile positions
                let positions = workspace.tiling.tile(&windows_to_tile);
                
                // Clear old cached rectangles for tiled windows
                for window in &windows_to_tile {
                    workspace.window_rectangles.remove(window);
                }
                
                // Apply positions and sizes
                for (window, rect) in positions {
                    // Cache the rectangle for this window
                    workspace.window_rectangles.insert(window.clone(), rect);
                    
                    // Position the window, accounting for CSD shadow offsets
                    let window_geom = window.geometry();
                    let position = Point::new(
                        rect.loc.x - window_geom.loc.x,
                        rect.loc.y - window_geom.loc.y,
                    );
                    self.space.map_element(window.clone(), position, false);
                    
                    // Resize the window if it has a toplevel surface
                    if let Some(toplevel) = window.toplevel() {
                        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
                        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
                        
                        toplevel.with_pending_state(|state| {
                            state.size = Some(rect.size);
                            state.bounds = Some(rect.size);
                            
                            // Force server-side decorations (no client decorations)
                            state.decoration_mode = Some(Mode::ServerSide);
                            
                            // Set tiled states to remove decorations and inform the client
                            state.states.set(State::TiledLeft);
                            state.states.set(State::TiledRight);
                            state.states.set(State::TiledTop);
                            state.states.set(State::TiledBottom);
                        });
                        
                        // Send the configure event
                        if toplevel.is_initial_configure_sent() {
                            toplevel.send_configure();
                        }
                    }
                }
            }
            workspace::LayoutMode::Tabbed => {
                // Hide all tiled windows first
                for window in &windows_to_tile {
                    self.space.unmap_elem(window);
                }
                
                // Show only the active tab
                if let Some(active_window) = windows_to_tile.get(workspace.active_tab_index) {
                    let window_rect = Rectangle {
                        loc: Point::from((available_area.loc.x, available_area.loc.y + workspace::TAB_HEIGHT)),
                        size: Size::from((available_area.size.w, available_area.size.h - workspace::TAB_HEIGHT)),
                    };
                    
                    // Cache the rectangle
                    workspace.window_rectangles.insert(active_window.clone(), window_rect);
                    
                    // Map the active window, accounting for CSD shadow offsets
                    let window_geom = active_window.geometry();
                    let position = Point::new(
                        window_rect.loc.x - window_geom.loc.x,
                        window_rect.loc.y - window_geom.loc.y,
                    );
                    self.space.map_element(active_window.clone(), position, false);
                    
                    // Configure the window
                    if let Some(toplevel) = active_window.toplevel() {
                        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
                        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
                        
                        toplevel.with_pending_state(|state| {
                            state.size = Some(window_rect.size);
                            state.bounds = Some(window_rect.size);
                            
                            // Force server-side decorations (no client decorations)
                            state.decoration_mode = Some(Mode::ServerSide);
                            
                            // Set tiled states to remove decorations and inform the client
                            state.states.set(State::TiledLeft);
                            state.states.set(State::TiledRight);
                            state.states.set(State::TiledTop);
                            state.states.set(State::TiledBottom);
                        });
                        
                        // Send the configure event
                        if toplevel.is_initial_configure_sent() {
                            toplevel.send_configure();
                        }
                    }
                }
            }
        }
        
        workspace.needs_arrange = false;
    }
    
    /// Remove a window from all workspaces
    pub fn remove_window(&mut self, window: &Window) -> Option<Output> {
        let mut found_output = None;
        
        // Find and remove from all workspaces
        for workspace in self.workspaces.values_mut() {
            if workspace.remove_window(window) {
                found_output = workspace.output.clone();
            }
        }
        
        // Clear focused window if it was removed
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = None;
        }
        
        // Unmap from space
        self.space.unmap_elem(window);
        
        found_output
    }
    
    /// Handle tab click at the given position
    pub fn handle_tab_click(&mut self, output: &Output, point: Point<f64, Logical>) -> bool {
        if let Some(workspace) = self.active_workspace_mut(output) {
            if !matches!(workspace.layout_mode, workspace::LayoutMode::Tabbed) {
                return false;
            }
            
            let area = workspace.available_area;
            if point.y >= area.loc.y as f64 
                && point.y < (area.loc.y + workspace::TAB_HEIGHT) as f64 {
                
                let tiled_count = workspace.tiled_windows().count();
                if tiled_count > 0 {
                    let tab_width = area.size.w / tiled_count as i32;
                    let relative_x = (point.x - area.loc.x as f64) as i32;
                    
                    // Find which tab was clicked, accounting for separators
                    for i in 0..tiled_count {
                        let tab_start = i as i32 * tab_width;
                        let tab_end = if i < tiled_count - 1 {
                            tab_start + tab_width - 2  // Account for separator
                        } else {
                            (i + 1) as i32 * tab_width  // Last tab takes full width
                        };
                        
                        if relative_x >= tab_start && relative_x < tab_end {
                            workspace.active_tab_index = i;
                            workspace.needs_arrange = true;
                            
                            // Update focus to the clicked tab
                            let window = workspace.tiled_windows().nth(i).cloned();
                            if let Some(window) = window {
                                workspace.append_focus(&window);
                                self.focused_window = Some(window);
                            }
                            
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}
