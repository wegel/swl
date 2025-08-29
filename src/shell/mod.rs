// SPDX-License-Identifier: GPL-3.0-only

pub mod tiling;

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
    utils::{IsAlive, Logical, Point, Rectangle, Scale, Size},
};
use std::collections::{HashMap, HashSet};

use crate::backend::render::element::{AsGlowRenderer, CosmicElement};
use self::tiling::TilingLayout;

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
    
    /// Geometry to restore when exiting fullscreen
    pub fullscreen_restore: Option<Rectangle<i32, Logical>>,
    
    /// Cursor position (relative to space origin)
    pub cursor_position: Point<f64, Logical>,
    
    /// Cursor image status
    pub cursor_status: CursorImageStatus,
    
    /// Tiling layout manager
    pub tiling: TilingLayout,
    
    /// Windows that are floating (exempt from tiling)
    pub floating_windows: HashSet<Window>,
    
    /// Ordered list of windows for focus cycling
    pub focus_stack: Vec<Window>,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            space: Space::default(),
            windows: HashMap::new(),
            next_window_id: 1,
            focused_window: None,
            fullscreen_window: None,
            fullscreen_restore: None,
            // start cursor off-screen to avoid rendering on all outputs at startup
            cursor_position: Point::from((-1000.0, -1000.0)),
            cursor_status: CursorImageStatus::default_named(),
            tiling: TilingLayout::new((1920, 1080).into()), // default size, will be updated
            floating_windows: HashSet::new(),
            focus_stack: Vec::new(),
        }
    }
    
    /// Add an output to the shell's space
    pub fn add_output(&mut self, output: &Output) {
        // map the output at origin (we don't support multi-monitor positioning yet)
        self.space.map_output(output, Point::from((0, 0)));
        
        // update tiling layout with output size
        if let Some(mode) = output.current_mode() {
            // convert physical size to logical
            let scale = output.current_scale().fractional_scale();
            let logical_size = Size::from((
                (mode.size.w as f64 / scale) as i32,
                (mode.size.h as f64 / scale) as i32,
            ));
            self.tiling.set_output_size(logical_size);
        }
        
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
        
        // map window to a temporary location first, then arrange
        self.space.map_element(window.clone(), Point::from((0, 0)), false);
        self.arrange();
        
        // add to focus stack and set as focused
        self.append_focus(window.clone());
        tracing::debug!("Set window {} as focused", id);
        
        tracing::info!("Window {} added successfully. Total windows: {}", id, self.windows.len());
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
            // store current geometry before going fullscreen
            if let Some(geometry) = self.space.element_geometry(&window) {
                self.fullscreen_restore = Some(geometry);
            }
            self.fullscreen_window = Some(window);
        } else if self.fullscreen_window.as_ref() == Some(&window) {
            self.fullscreen_window = None;
            // geometry will be restored by the caller using take_fullscreen_restore()
        }
    }
    
    /// Take the fullscreen restore geometry (used when exiting fullscreen)
    pub fn take_fullscreen_restore(&mut self) -> Option<Rectangle<i32, Logical>> {
        self.fullscreen_restore.take()
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
    
    /// Get render elements for all windows and layer surfaces on the given output
    pub fn render_elements<R>(&self, output: &Output, renderer: &mut R) -> Vec<CosmicElement<R>>
    where
        R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + 'static,
    {
        let mut elements = Vec::new();
        let output_scale = Scale::from(output.current_scale().fractional_scale());
        
        use smithay::wayland::shell::wlr_layer::Layer;
        
        // render layer surfaces in correct order
        let layer_map = smithay::desktop::layer_map_for_output(output);
        let layers: Vec<_> = layer_map.layers().cloned().collect();
        
        // elements should be in front-to-back order for smithay's damage tracker
        // (first element is topmost, last element is bottommost)
        
        // 1. Top and Overlay layers (topmost, in front of windows)
        for layer_surface in &layers {
            let layer = layer_surface.layer();
            if layer == Layer::Top || layer == Layer::Overlay {
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
        
        // 2. Windows (in the middle)
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
        
        // 3. Background and Bottom layers (bottommost, behind windows)
        for layer_surface in &layers {
            let layer = layer_surface.layer();
            if layer == Layer::Background || layer == Layer::Bottom {
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
    
    /// Arrange windows according to the current tiling layout
    pub fn arrange(&mut self) {
        tracing::info!("arrange() called");
        
        // collect windows to tile (non-floating, non-fullscreen)
        let mut windows_to_tile = Vec::new();
        for window in self.space.elements() {
            if !self.floating_windows.contains(window) 
                && self.fullscreen_window.as_ref() != Some(window) {
                windows_to_tile.push(window.clone());
            }
        }
        
        // get tile positions
        let positions = self.tiling.tile(&windows_to_tile);
        
        // apply positions and sizes
        for (window, rect) in positions {
            // position the window
            self.space.map_element(window.clone(), rect.loc, false);
            
            // resize the window if it has a toplevel surface
            if let Some(toplevel) = window.toplevel() {
                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
                use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
                
                toplevel.with_pending_state(|state| {
                    state.size = Some(rect.size);
                    state.bounds = Some(rect.size);
                    
                    // force server-side decorations (no client decorations)
                    state.decoration_mode = Some(Mode::ServerSide);
                    
                    // set tiled states to remove decorations and inform the client
                    state.states.set(State::TiledLeft);
                    state.states.set(State::TiledRight);
                    state.states.set(State::TiledTop);
                    state.states.set(State::TiledBottom);
                    
                    // remove maximized/fullscreen states if present
                    state.states.unset(State::Maximized);
                    state.states.unset(State::Fullscreen);
                });
                
                // only send configure if initial configure was already sent
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_configure();
                }
            }
        }
        
        tracing::info!("Arranged {} windows", windows_to_tile.len());
        
        // no need to send frame callbacks here - the render loop will handle that
    }
    
    /// Toggle floating state for a window
    pub fn toggle_floating(&mut self, window: &Window) {
        if self.floating_windows.contains(window) {
            self.floating_windows.remove(window);
            tracing::debug!("Window no longer floating");
        } else {
            self.floating_windows.insert(window.clone());
            
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
        self.arrange();
    }
    
    /// Zoom - swap focused window with first master window
    pub fn zoom(&mut self) {
        if let Some(focused) = self.focused_window.clone() {
            // find focused window in focus stack
            if let Some(pos) = self.focus_stack.iter().position(|w| w == &focused) {
                if pos > 0 {
                    // swap with first window
                    self.focus_stack.swap(0, pos);
                    self.arrange();
                    tracing::debug!("Zoomed window to master");
                }
            }
        }
    }
    
    /// Focus the next window in the stack
    pub fn focus_next(&mut self) {
        if self.focus_stack.len() <= 1 {
            return;
        }
        
        if let Some(focused) = &self.focused_window {
            if let Some(pos) = self.focus_stack.iter().position(|w| w == focused) {
                let next_pos = (pos + 1) % self.focus_stack.len();
                let next_window = self.focus_stack[next_pos].clone();
                self.append_focus(next_window);
                tracing::debug!("Focused next window");
            }
        } else if !self.focus_stack.is_empty() {
            let first_window = self.focus_stack[0].clone();
            self.append_focus(first_window);
        }
    }
    
    /// Focus the previous window in the stack
    pub fn focus_prev(&mut self) {
        if self.focus_stack.len() <= 1 {
            return;
        }
        
        if let Some(focused) = &self.focused_window {
            if let Some(pos) = self.focus_stack.iter().position(|w| w == focused) {
                let prev_pos = if pos == 0 { self.focus_stack.len() - 1 } else { pos - 1 };
                let prev_window = self.focus_stack[prev_pos].clone();
                self.append_focus(prev_window);
                tracing::debug!("Focused previous window");
            }
        } else if !self.focus_stack.is_empty() {
            let last_window = self.focus_stack[self.focus_stack.len() - 1].clone();
            self.append_focus(last_window);
        }
    }
    
    /// Close the focused window
    pub fn close_focused(&mut self) {
        if let Some(window) = self.focused_window.clone() {
            if let Some(surface) = window.toplevel() {
                surface.send_close();
                tracing::info!("Sent close request to focused window");
            }
        } else {
            tracing::warn!("No focused window to close");
        }
    }
    
    /// Refresh focus to the topmost window in the focus stack
    /// Called when layer surfaces are destroyed or focus needs updating
    pub fn refresh_focus(&mut self) -> Option<Window> {
        // find the last alive window in the focus stack
        let focused = self.focus_stack.iter()
            .rev()
            .find(|w| w.alive())
            .cloned();
        
        self.focused_window = focused.clone();
        
        if focused.is_some() {
            tracing::debug!("Refreshed focus to window from focus stack");
        } else {
            tracing::debug!("No alive window in focus stack to focus");
        }
        
        focused
    }
    
    /// Update the focus stack when a window receives focus
    pub fn append_focus(&mut self, window: Window) {
        // remove dead windows from the stack
        self.focus_stack.retain(|w| w.alive());
        
        // remove the window if it's already in the stack
        if let Some(pos) = self.focus_stack.iter().position(|w| w == &window) {
            self.focus_stack.remove(pos);
        }
        
        // add it to the end (most recently focused)
        self.focus_stack.push(window.clone());
        self.focused_window = Some(window);
        
        tracing::trace!("Focus stack updated, {} windows tracked", self.focus_stack.len());
    }
}