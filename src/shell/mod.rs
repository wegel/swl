// SPDX-License-Identifier: GPL-3.0-only

pub mod tiling;
pub mod virtual_output;
pub mod workspace;

use virtual_output::VirtualOutput;

use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            AsRenderElements, RenderElementStates,
        },
        ImportAll, ImportMem, Renderer,
    },
    desktop::{
        utils::{surface_presentation_feedback_flags_from_states, OutputPresentationFeedback},
        Space, Window,
    },
    input::pointer::CursorImageStatus,
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Logical, Point, Rectangle, Scale},
};
use std::collections::HashMap;

use self::virtual_output::{VirtualOutputId, VirtualOutputManager};
use self::workspace::{Workspace, WorkspaceId};
use crate::backend::render::element::{AsGlowRenderer, SwlElement};
use crate::utils::coordinates::{
    GlobalPoint, GlobalRect, OutputExt, OutputRelativePoint, SpaceExt, VirtualOutputRelativePoint,
    VirtualOutputRelativeRect,
};

// window border configuration
pub const BORDER_WIDTH: i32 = 1;
const FOCUSED_BORDER_COLOR: [f32; 4] = [0.0, 0.5, 1.0, 1.0]; // bright blue
const UNFOCUSED_BORDER_COLOR: [f32; 4] = [0.0, 0.2, 0.5, 1.0]; // darker blue

/// Determine if a window should float by default
fn should_float_impl(window: &Window) -> bool {
    // check if window is a dialog
    if let Some(toplevel) = window.toplevel() {
        let has_parent = toplevel.parent().is_some();

        tracing::debug!(
            "should_float check - has_parent: {}, geometry: {:?}",
            has_parent,
            window.geometry()
        );

        if has_parent {
            // window has a parent, likely a dialog
            tracing::debug!("Window has parent, floating it");
            return true;
        }
    }

    // could add more checks here based on window size, app_id, etc.
    false
}

/// A simple shell for managing windows
pub struct Shell {
    /// The space containing all windows
    pub space: Space<Window>,

    /// All workspaces indexed by stable ID
    pub workspaces: HashMap<WorkspaceId, Workspace>,

    /// Workspace name to ID mapping
    workspace_names: HashMap<String, WorkspaceId>,

    /// Next workspace ID counter
    next_workspace_id: u64,

    /// The currently focused window (global)
    pub focused_window: Option<Window>,

    /// Cursor position (relative to space origin)
    pub cursor_position: Point<f64, Logical>,

    /// Cursor image status
    pub cursor_status: CursorImageStatus,

    /// Virtual output manager
    pub virtual_output_manager: VirtualOutputManager,

    /// Currently focused virtual output (for fallback operations)
    pub focused_virtual_output_id: Option<virtual_output::VirtualOutputId>,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            space: Space::default(),
            workspaces: HashMap::new(),
            workspace_names: HashMap::new(),
            next_workspace_id: 1,
            focused_window: None,
            // start cursor off-screen to avoid rendering on all outputs at startup
            // using negative coordinates as sentinel for "not on any output"
            // TODO: convert to Option<GlobalPoint<f64>> for better type safety
            cursor_position: Point::from((-1000.0, -1000.0)),
            cursor_status: CursorImageStatus::default_named(),
            virtual_output_manager: VirtualOutputManager::new(),
            focused_virtual_output_id: None,
        }
    }

    /// Find or create a workspace ID for the given name
    pub fn find_or_create_workspace_id(&mut self, workspace_name: &str) -> WorkspaceId {
        if let Some(&workspace_id) = self.workspace_names.get(workspace_name) {
            workspace_id
        } else {
            let workspace_id = WorkspaceId(self.next_workspace_id);
            self.next_workspace_id += 1;

            let workspace = Workspace::new(workspace_name.to_string());
            self.workspaces.insert(workspace_id, workspace);
            self.workspace_names
                .insert(workspace_name.to_string(), workspace_id);

            tracing::info!("Creating new workspace: {}", workspace_name);
            workspace_id
        }
    }

    /// Find which virtual output currently owns the given workspace (if any)
    fn find_workspace_owner(&self, workspace_id: WorkspaceId) -> Option<VirtualOutputId> {
        for vout in self.virtual_output_manager.all() {
            if vout.active_workspace() == Some(workspace_id) {
                return Some(vout.id);
            }
        }
        None
    }

    /// Switch a virtual output to a specific workspace, enforcing one-to-one relationship
    pub fn switch_workspace_on_virtual(
        &mut self,
        virtual_id: VirtualOutputId,
        workspace_name: &str,
    ) {
        let workspace_id = self.find_or_create_workspace_id(workspace_name);

        tracing::debug!(
            "Switching virtual output {:?} to workspace '{}'",
            virtual_id,
            workspace_name
        );

        // check if this workspace is already active on a different virtual output
        if let Some(current_owner) = self.find_workspace_owner(workspace_id) {
            if current_owner != virtual_id {
                tracing::debug!("Workspace '{}' is currently visible on {:?}, will hide from there and show on {:?}", 
                    workspace_name, current_owner, virtual_id);

                // find a different workspace for the current owner to switch to
                let fallback_workspace =
                    self.find_fallback_workspace_for_virtual_output(current_owner, workspace_id);

                // hide windows from the workspace on current owner
                if let Some(workspace) = self.workspaces.get(&workspace_id) {
                    tracing::debug!(
                        "Unmapping {} windows from workspace '{}' on {:?}",
                        workspace.windows.len(),
                        workspace_name,
                        current_owner
                    );
                    for window in &workspace.windows {
                        self.space.unmap_elem(window);
                    }
                }

                // switch current owner to fallback workspace
                if let Some(current_vout) = self.virtual_output_manager.get_mut(current_owner) {
                    current_vout.set_active_workspace(fallback_workspace);

                    // show windows from fallback workspace if any
                    if let Some(fallback_id) = fallback_workspace {
                        if let Some(fallback_ws) = self.workspaces.get(&fallback_id) {
                            tracing::debug!(
                                "Mapping {} windows for fallback workspace on {:?}",
                                fallback_ws.windows.len(),
                                current_owner
                            );
                            for window in &fallback_ws.windows {
                                self.space.map_element(window.clone(), (0, 0), false);
                            }
                        }
                    }
                }
            }
        }

        // get current workspace of target virtual output (to potentially unmap it)
        let old_workspace_id = self
            .virtual_output_manager
            .get(virtual_id)
            .and_then(|vout| vout.active_workspace());

        // hide windows from old workspace
        if let Some(old_id) = old_workspace_id {
            if let Some(old_workspace) = self.workspaces.get(&old_id) {
                tracing::debug!(
                    "Unmapping {} windows from old workspace",
                    old_workspace.windows.len()
                );
                for window in &old_workspace.windows {
                    self.space.unmap_elem(window);
                }
            }
        }

        // assign workspace to virtual output
        if let Some(vout) = self.virtual_output_manager.get_mut(virtual_id) {
            vout.set_active_workspace(Some(workspace_id));

            // update workspace geometry to match virtual output
            if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
                workspace.update_output_geometry(vout.logical_geometry);
                workspace.virtual_output_id = Some(virtual_id);
            }
        }

        // show windows from new workspace
        if let Some(new_workspace) = self.workspaces.get(&workspace_id) {
            tracing::debug!(
                "Mapping {} windows for workspace '{}'",
                new_workspace.windows.len(),
                workspace_name
            );
            for window in &new_workspace.windows {
                self.space.map_element(window.clone(), (0, 0), false);
            }

            // mark for arrangement
            if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
                workspace.needs_arrange = true;
            }
        }

        // trigger arrangement if we have a physical output
        let physical_output = self
            .virtual_output_manager
            .get(virtual_id)
            .and_then(|vout| vout.regions.first())
            .map(|region| region.physical_output.clone());

        if let Some(output) = physical_output {
            self.arrange_windows_on_output(&output);
        }
    }

    /// Add an output to the shell's space
    pub fn add_output(&mut self, output: &Output) {
        // use the output's current configured position instead of hardcoding (0,0)
        let position = output.current_location_typed();
        self.space.map_output(output, position.as_point());

        // update virtual outputs when physical output is added
        self.virtual_output_manager
            .update_all(&self.space.outputs().cloned().collect::<Vec<_>>());

        // create default virtual output if none exists
        let vouts = self
            .virtual_output_manager
            .virtual_outputs_for_physical(output);
        tracing::debug!(
            "Found {} existing virtual outputs for physical output {}",
            vouts.len(),
            output.name()
        );

        if vouts.is_empty() {
            tracing::debug!("Creating default virtual output for {}", output.name());
            let vout_id = self.virtual_output_manager.create_default(output);
            tracing::debug!("Created virtual output {:?}", vout_id);
            // switch to workspace "1" on the new virtual output
            self.switch_workspace_on_virtual(vout_id, "1");
        } else {
            tracing::debug!("Virtual outputs already exist for {}", output.name());
        }

        tracing::info!("Added output {} to shell space", output.name());
    }

    /// Update output position in the space (call this after output configuration changes)
    pub fn update_output_position(&mut self, output: &Output) {
        let position = output.current_location_typed();
        // smithay's space will automatically handle position updates when we remap
        self.space.map_output(output, position.as_point());

        // update virtual outputs to reflect the new position
        self.virtual_output_manager
            .update_all(&self.space.outputs().cloned().collect::<Vec<_>>());

        tracing::debug!(
            "Updated output {} position to {:?}",
            output.name(),
            position
        );
    }

    /// Find virtual output containing a specific point
    pub fn virtual_output_at_point(&self, point: Point<f64, Logical>) -> Option<VirtualOutputId> {
        tracing::debug!("virtual_output_at_point: checking point {:?}", point);
        for vout in self.virtual_output_manager.all() {
            tracing::debug!(
                "virtual_output_at_point: checking vout {:?} with geometry {:?}",
                vout.id,
                vout.logical_geometry
            );
            if vout.logical_geometry.to_f64().contains(point) {
                tracing::debug!("virtual_output_at_point: found match in vout {:?}", vout.id);
                return Some(vout.id);
            }
        }
        tracing::debug!("virtual_output_at_point: no match found");
        None
    }

    /// Find a fallback workspace for a virtual output when its current workspace is claimed
    fn find_fallback_workspace_for_virtual_output(
        &mut self,
        virtual_output_id: VirtualOutputId,
        exclude_workspace: WorkspaceId,
    ) -> Option<WorkspaceId> {
        // strategy: find the first workspace that is not currently visible on any virtual output
        for (workspace_id, _) in &self.workspaces {
            if *workspace_id == exclude_workspace {
                continue; // Skip the workspace being claimed
            }

            // check if this workspace is currently visible on any virtual output
            let is_visible = self
                .virtual_output_manager
                .all()
                .any(|vout| vout.active_workspace() == Some(*workspace_id));

            if !is_visible {
                tracing::debug!(
                    "Found fallback workspace {:?} for virtual output {:?}",
                    workspace_id,
                    virtual_output_id
                );
                return Some(*workspace_id);
            }
        }

        // if all workspaces are visible, create a new one
        // Find next available workspace number
        let next_number = (1..=100)
            .find(|&n| !self.workspace_names.contains_key(&n.to_string()))
            .unwrap_or(1);

        let workspace_id = self.find_or_create_workspace_id(&next_number.to_string());
        tracing::debug!(
            "Created new fallback workspace {:?} ('{}') for virtual output {:?}",
            workspace_id,
            next_number,
            virtual_output_id
        );

        Some(workspace_id)
    }

    /// Add a window to a specific virtual output
    pub fn add_window_to_virtual_output(
        &mut self,
        window: Window,
        virtual_output_id: VirtualOutputId,
    ) {
        // Log window properties for debugging
        let geometry = window.geometry();
        tracing::info!("Adding window - geometry: {:?}", geometry);

        tracing::debug!("Adding window to virtual output {:?}", virtual_output_id);

        // Get active workspace or create default
        let workspace_id = {
            let virtual_output = self.virtual_output_manager.get(virtual_output_id);
            match virtual_output.and_then(|vo| vo.active_workspace()) {
                Some(id) => id,
                None => {
                    // Create workspace "1" and assign it to virtual output
                    let workspace_id = self.find_or_create_workspace_id("1");
                    if let Some(vout_mut) = self.virtual_output_manager.get_mut(virtual_output_id) {
                        vout_mut.set_active_workspace(Some(workspace_id));
                    }
                    workspace_id
                }
            }
        };

        tracing::debug!(
            "Adding window to virtual output {:?}, workspace: {:?}",
            virtual_output_id,
            workspace_id
        );

        // Add window to workspace
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.virtual_output_id = Some(virtual_output_id);
            workspace.add_window(window.clone(), should_float_impl(&window));
            workspace.append_focus(&window);
        }

        // Map window in smithay space at virtual output's global position
        let vout_position = self
            .virtual_output_manager
            .get(virtual_output_id)
            .map(|vout| vout.logical_geometry.location().as_point())
            .unwrap_or_default();
        self.space.map_element(window.clone(), vout_position, false);
        tracing::debug!(
            "Mapped window to smithay space at {:?} (virtual output global position)",
            vout_position
        );

        tracing::debug!("Setting focus to window");
        self.focused_window = Some(window.clone());

        tracing::debug!("Set focus to new window");

        // arrange windows - we need to get the output from virtual output
        let outputs_to_arrange: Vec<_> =
            if let Some(vout) = self.virtual_output_manager.get(virtual_output_id) {
                vout.regions
                    .iter()
                    .map(|region| region.physical_output.clone())
                    .collect()
            } else {
                Vec::new()
            };

        for output in outputs_to_arrange {
            self.arrange_windows_on_output(&output);
        }
    }

    /// Add a new window to the shell (legacy method - uses cursor position to determine virtual output)
    pub fn add_window(&mut self, window: Window, output: &Output) {
        // Log window properties for debugging temporary windows
        let geometry = window.geometry();
        tracing::info!("Adding window - geometry: {:?}", geometry);

        // find first virtual output on this physical output
        let vout = self
            .virtual_output_manager
            .virtual_outputs_for_physical(output)
            .first()
            .and_then(|v| Some(v.id))
            .unwrap_or_else(|| {
                // create default if none exists
                self.virtual_output_manager.create_default(output)
            });

        tracing::debug!("Adding window to virtual output {:?}", vout);

        // Get active workspace or create default
        let workspace_id = {
            let virtual_output = self.virtual_output_manager.get(vout);
            match virtual_output.and_then(|vo| vo.active_workspace()) {
                Some(id) => id,
                None => {
                    // Create workspace "1" and assign it to virtual output
                    let workspace_id = self.find_or_create_workspace_id("1");
                    if let Some(vout_mut) = self.virtual_output_manager.get_mut(vout) {
                        vout_mut.set_active_workspace(Some(workspace_id));
                    }
                    workspace_id
                }
            }
        };

        tracing::debug!(
            "Adding window to virtual output {:?}, workspace: {:?}",
            vout,
            workspace_id
        );

        // Add window to workspace
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.virtual_output_id = Some(vout);
            workspace.add_window(window.clone(), should_float_impl(&window));
        }

        tracing::debug!(
            "Added window to workspace {:?} on virtual output {:?}",
            workspace_id,
            vout
        );

        // map at global origin initially (will be repositioned by tiling)
        let initial_position = GlobalPoint::new(0, 0);
        self.space
            .map_element(window.clone(), initial_position.as_point(), false);
        tracing::debug!("Mapped window to smithay space at global origin");
        self.set_focus(window.clone());
        tracing::debug!("Set focus to new window");

        // arrange windows after adding new window
        self.arrange_windows_on_output(output);
    }

    /// Move a window to a specific workspace
    pub fn move_window_to_workspace(
        &mut self,
        window: Window,
        workspace_name: String,
        output: &Output,
    ) {
        // First, remove window from all workspaces
        self.remove_window(&window);

        // Find or create the workspace ID
        let workspace_id = self.find_or_create_workspace_id(&workspace_name);

        // Determine if window should be floating
        let floating = should_float_impl(&window);

        // Add window to the specific workspace
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.add_window(window.clone(), floating);
        }

        // If this workspace is currently active on any virtual output on this physical output, map the window
        let should_map = self
            .virtual_output_manager
            .virtual_outputs_for_physical(output)
            .iter()
            .any(|vout| vout.active_workspace() == Some(workspace_id));

        if should_map {
            // map at global origin initially (will be repositioned by tiling)
            let initial_position = GlobalPoint::new(0, 0);
            self.space
                .map_element(window.clone(), initial_position.as_point(), false);
        }

        // Set as focused
        self.set_focus(window);
    }

    /// Get the window under the given point
    pub fn window_under(&self, point: Point<f64, Logical>) -> Option<Window> {
        use tracing::debug;

        for window in self.space.elements() {
            // get the window's position in space
            let location = self
                .space
                .element_location_typed(window)
                .unwrap_or_default();
            // get the window's bounding box (includes decorations)
            let bbox = window.bbox();
            // translate bbox to global coordinates
            let bbox_global_origin = GlobalPoint::new(
                location.as_point().x + bbox.loc.x,
                location.as_point().y + bbox.loc.y,
            );
            let global_bbox = GlobalRect::from_loc_and_size(bbox_global_origin, bbox.size);

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
    pub fn surface_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        use smithay::desktop::WindowSurfaceType;
        use smithay::wayland::shell::wlr_layer::Layer;
        use tracing::trace;

        //trace!("Looking for surface under point: {:?}", point);

        // Find which output contains the point
        let output = self.space.outputs().find(|o| {
            self.space
                .output_geometry(o)
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
                if let Some((surface, surf_loc)) =
                    layer.surface_under(layer_relative, WindowSurfaceType::ALL)
                {
                    let global_loc =
                        surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
                    trace!("Found overlay layer surface at {:?}", global_loc);
                    return Some((surface, global_loc));
                }
            }
        }

        // 2. Top layer (above windows)
        if let Some(layer) = layer_map.layer_under(Layer::Top, relative_point) {
            if let Some(layer_geo) = layer_map.layer_geometry(layer) {
                let layer_relative = relative_point - layer_geo.loc.to_f64();
                if let Some((surface, surf_loc)) =
                    layer.surface_under(layer_relative, WindowSurfaceType::ALL)
                {
                    let global_loc =
                        surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
                    trace!("Found top layer surface at {:?}", global_loc);
                    return Some((surface, global_loc));
                }
            }
        }

        // 3. Windows
        for window in self.space.elements() {
            // get the window's position in space
            let location = self
                .space
                .element_location_typed(window)
                .unwrap_or_default();

            // check if this window is fullscreen
            let is_fullscreen = window.toplevel()
                .map(|t| t.current_state().states.contains(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen))
                .unwrap_or(false);

            // for fullscreen windows, we need to account for CSD offset
            let (hit_test_rect, surface_under_offset) = if is_fullscreen {
                // fullscreen windows: use geometry for hit test (no visible CSD)
                // but surface_under still needs coordinates accounting for CSD
                let geom = window.geometry();
                let global_geom = GlobalRect::from_loc_and_size(location, geom.size);
                // surface_under needs the offset to account for hidden CSD
                (global_geom, geom.loc.to_f64())
            } else {
                // normal windows: use bbox for hit test (includes CSD)
                let bbox = window.bbox();
                let bbox_global_origin = GlobalPoint::new(
                    location.as_point().x + bbox.loc.x,
                    location.as_point().y + bbox.loc.y,
                );
                let global_bbox = GlobalRect::from_loc_and_size(bbox_global_origin, bbox.size);
                (global_bbox, Point::<f64, Logical>::from((0.0, 0.0)))
            };

            trace!(
                "Window hit test rect (fullscreen={}): {:?}",
                is_fullscreen,
                hit_test_rect
            );
            if hit_test_rect.to_f64().contains(point) {
                // convert point to window-relative coordinates
                // for fullscreen, adjust for the CSD offset
                let window_relative = point - location.to_f64() + surface_under_offset;
                trace!(
                    "Window-relative point (adjusted for CSD): {:?}",
                    window_relative
                );

                // check for surface under this point (including decorations)
                if let Some((surface, loc)) =
                    window.surface_under(window_relative, WindowSurfaceType::ALL)
                {
                    // convert back to global coordinates (and to f64)
                    // subtract the CSD offset we added earlier
                    let global_loc = (loc + location).to_f64() - surface_under_offset;
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
                if let Some((surface, surf_loc)) =
                    layer.surface_under(layer_relative, WindowSurfaceType::ALL)
                {
                    let global_loc =
                        surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
                    trace!("Found bottom layer surface at {:?}", global_loc);
                    return Some((surface, global_loc));
                }
            }
        }

        // 5. Background layer (bottommost)
        if let Some(layer) = layer_map.layer_under(Layer::Background, relative_point) {
            if let Some(layer_geo) = layer_map.layer_geometry(layer) {
                let layer_relative = relative_point - layer_geo.loc.to_f64();
                if let Some((surface, surf_loc)) =
                    layer.surface_under(layer_relative, WindowSurfaceType::ALL)
                {
                    let global_loc =
                        surf_loc.to_f64() + layer_geo.loc.to_f64() + output_geo.loc.to_f64();
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
        // Check all virtual outputs on this physical output for fullscreen windows
        for virtual_output in self
            .virtual_output_manager
            .virtual_outputs_for_physical(output)
        {
            if let Some(workspace_name) = &virtual_output.active_workspace {
                if let Some(workspace) = self.workspaces.get(workspace_name) {
                    if let Some(fullscreen_window) = &workspace.fullscreen {
                        return Some(fullscreen_window);
                    }
                }
            }
        }
        None
    }

    /// Set a window as fullscreen
    pub fn set_fullscreen(&mut self, window: Window, fullscreen: bool, output: &Output) {
        if let Some(workspace) = self.workspace_containing_window_mut(&window) {
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

    /// Toggle fullscreen for the focused window
    pub fn toggle_fullscreen(&mut self, output: &Output) {
        if let Some(focused_window) = self.focused_window.clone() {
            // find virtual output containing the focused window
            let vout_id = self
                .workspaces
                .values()
                .find(|ws| ws.windows.contains(&focused_window))
                .and_then(|ws| ws.virtual_output_id);

            if let Some(vout_id) = vout_id {
                if let Some(vout) = self.virtual_output_manager.get(vout_id) {
                    // find the workspace that contains the focused window
                    if let Some(workspace_name) = &vout.active_workspace {
                        if let Some(workspace) = self.workspaces.get_mut(workspace_name) {
                            // check if the focused window is already fullscreen
                            let is_fullscreen =
                                workspace.fullscreen.as_ref() == Some(&focused_window);

                            if is_fullscreen {
                                // unfullscreen the window
                                workspace.fullscreen = None;
                            } else {
                                // fullscreen the focused window
                                workspace.fullscreen = Some(focused_window);
                            }

                            workspace.needs_arrange = true;
                        }
                    }
                }
            }

            // arrange windows after fullscreen change
            self.arrange_windows_on_output(output);
        }
    }

    /// Refresh the space (needed for damage tracking)
    pub fn refresh(&mut self) {
        self.space.refresh();
    }

    /// Find which output a surface is visible on
    pub fn visible_output_for_surface(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> Option<&Output> {
        // Try to find the output by locating the window that contains this surface
        // (including subsurfaces and popups), then intersecting that window with outputs.
        for window in self.space.elements() {
            // Fast path: direct toplevel match
            let mut contains_surface = window
                .toplevel()
                .map_or(false, |t| t.wl_surface() == surface);

            // If not a direct match, scan the window's full surface tree (includes popups when tracked)
            if !contains_surface {
                window.with_surfaces(|s, _| {
                    if s == surface {
                        contains_surface = true;
                    }
                });
            }

            if !contains_surface {
                continue;
            }

            // We found the window that owns this surface; determine which output it is visible on
            for output in self.space.outputs() {
                let output_geometry = self.space.output_geometry(output).unwrap();
                if let Some(window_location) = self.space.element_location_typed(window) {
                    // check if window intersects with output
                    let window_geometry =
                        GlobalRect::from_loc_and_size(window_location, window.geometry().size);
                    if output_geometry.overlaps(window_geometry.as_rectangle()) {
                        return Some(output);
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
            if let Some(window_location) = self.space.element_location_typed(window) {
                let output_geometry = self.space.output_geometry(output).unwrap();
                let window_geometry =
                    GlobalRect::from_loc_and_size(window_location, window.geometry().size);

                if output_geometry.overlaps(window_geometry.as_rectangle()) {
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
        self.space
            .outputs()
            .find(|output| {
                let geometry = self.space.output_geometry(output).unwrap();
                geometry.to_f64().contains(position)
            })
            .cloned()
    }

    /// Get render elements for all windows and layer surfaces on the given output
    pub fn render_elements<R>(&self, output: &Output, renderer: &mut R) -> Vec<SwlElement<R>>
    where
        R: AsGlowRenderer + Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + 'static,
    {
        let mut elements = Vec::new();
        let output_scale = Scale::from(output.current_scale().fractional_scale());
        let output_position = output.current_location_typed().as_point();

        use smithay::wayland::shell::wlr_layer::Layer;

        // Get all virtual outputs that overlap this physical output
        let vouts = self
            .virtual_output_manager
            .virtual_outputs_for_physical(output);

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
                        surface_elements
                            .into_iter()
                            .map(|elem| SwlElement::Surface(elem)),
                    );
                }
            }
        }

        tracing::debug!("render_elements called");

        // render windows from virtual outputs
        for vout in vouts {
            // only render windows from the active workspace of this virtual output
            if let Some(workspace_name) = &vout.active_workspace {
                if let Some(workspace) = self.workspaces.get(workspace_name) {
                    // check if we have a fullscreen window in this workspace
                    let has_fullscreen = workspace.fullscreen.is_some();

                    // get regions of this virtual output on this physical output
                    for region in vout.regions.iter().filter(|r| &r.physical_output == output) {
                        // First, collect all window elements
                        let mut window_elements = Vec::new();
                        let mut focused_window_rect = None;

                        // when there's a fullscreen window, only render that window
                        if has_fullscreen {
                            if let Some(fullscreen_window) = &workspace.fullscreen {
                                if let Some(location) =
                                    self.space.element_location_typed(fullscreen_window)
                                {
                                    // check if fullscreen window intersects with this virtual output region
                                    let window_rect =
                                        Rectangle::from_size(fullscreen_window.geometry().size);
                                    let window_rect =
                                        GlobalRect::from_loc_and_size(location, window_rect.size);

                                    //tracing::debug!("Fullscreen render overlap check: region.logical_rect={:?} window_rect={:?} overlaps={}",
                                    //    region.logical_rect, window_rect, region.logical_rect.as_rectangle().overlaps(window_rect.as_rectangle()));
                                    if region
                                        .logical_rect
                                        .as_rectangle()
                                        .overlaps(window_rect.as_rectangle())
                                    {
                                        // render only the fullscreen window
                                        // convert global coordinates to output-relative coordinates
                                        let output_position = output.current_location_typed();
                                        let output_relative_location =
                                            location.to_output_relative(output_position);
                                        let surface_elements = fullscreen_window.render_elements(
                                            renderer,
                                            output_relative_location
                                                .as_point()
                                                .to_physical_precise_round(output_scale),
                                            output_scale,
                                            1.0,
                                        );
                                        window_elements.extend(
                                            surface_elements
                                                .into_iter()
                                                .map(|elem| SwlElement::Surface(elem)),
                                        );
                                    }
                                }
                            }
                        } else {
                            // normal rendering for all windows when not in fullscreen
                            // clip and translate windows to this region
                            for window in &workspace.windows {
                                if let Some(location) = self.space.element_location_typed(window) {
                                    // check if window intersects with this virtual output region
                                    let window_rect = Rectangle::from_size(window.geometry().size);
                                    let window_rect =
                                        GlobalRect::from_loc_and_size(location, window_rect.size);

                                    //tracing::debug!("Render overlap check: region.logical_rect={:?} window_rect={:?} overlaps={}",
                                    //    region.logical_rect, window_rect, region.logical_rect.as_rectangle().overlaps(window_rect.as_rectangle()));
                                    if region
                                        .logical_rect
                                        .as_rectangle()
                                        .overlaps(window_rect.as_rectangle())
                                    {
                                        // render the window (existing window rendering code)
                                        // convert global coordinates to output-relative coordinates
                                        let output_position = output.current_location_typed();
                                        let output_relative_location =
                                            location.to_output_relative(output_position);
                                        let surface_elements = window.render_elements(
                                            renderer,
                                            output_relative_location
                                                .as_point()
                                                .to_physical_precise_round(output_scale),
                                            output_scale,
                                            1.0,
                                        );
                                        //tracing::debug!("Window render_elements: global {:?} -> output-relative {:?} (physical {:?})",
                                        //    location, output_relative_location, output_relative_location.as_point().to_physical_precise_round::<_, i32>(output_scale));
                                        window_elements.extend(
                                            surface_elements
                                                .into_iter()
                                                .map(|elem| SwlElement::Surface(elem)),
                                        );

                                        // Track focused window rectangle for border rendering
                                        if self.focused_window.as_ref() == Some(window)
                                            && !workspace.floating_windows.contains(window)
                                        {
                                            if let Some(rect) =
                                                workspace.window_rectangles.get(window)
                                            {
                                                if rect.size().w > 0 && rect.size().h > 0 {
                                                    // Convert from virtual-output-relative to global coordinates
                                                    let vout_origin =
                                                        vout.logical_geometry.location();
                                                    let global_location =
                                                        rect.location().to_global(vout_origin);
                                                    focused_window_rect = Some((
                                                        global_location.as_point(),
                                                        rect.size(),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            } // end of normal window rendering loop
                        } // end of else block (not fullscreen)

                        // Add window elements first (they will render behind borders in front-to-back order)
                        //tracing::debug!("Adding {} window elements to render list", window_elements.len());
                        elements.extend(window_elements);

                        // Render tab bar if in tabbed mode
                        if matches!(workspace.layout_mode, workspace::LayoutMode::Tabbed) {
                            let tiled: Vec<_> = workspace.tiled_windows().cloned().collect();
                            if !tiled.is_empty() {
                                let area = workspace.available_area;
                                let separator_color = [0.1, 0.1, 0.1, 1.0]; // dark gray separator

                                // render individual tab sections with separators
                                let tab_width = area.size().w / tiled.len() as i32;
                                for (i, _window) in tiled.iter().enumerate() {
                                    let is_active = i == workspace.active_tab_index;
                                    let color = if is_active {
                                        FOCUSED_BORDER_COLOR // bright blue for active
                                    } else {
                                        UNFOCUSED_BORDER_COLOR // darker blue for inactive
                                    };

                                    let tab_x =
                                        area.location().as_point().x + (i as i32 * tab_width);

                                    // calculate actual tab width (accounting for separator)
                                    let actual_tab_width = if i < tiled.len() - 1 {
                                        tab_width - 2 // leave space for 2-pixel separator
                                    } else {
                                        tab_width // last tab takes remaining space
                                    };

                                    // render the tab
                                    let tab_buffer = SolidColorBuffer::new(
                                        (actual_tab_width, workspace::TAB_HEIGHT),
                                        color,
                                    );
                                    // convert tab position from virtual-output-relative to output-relative for rendering
                                    let tab_global = VirtualOutputRelativePoint::new(
                                        tab_x,
                                        area.location().as_point().y,
                                    )
                                    .to_global(vout.logical_geometry.location());
                                    let tab_output_relative = tab_global
                                        .to_output_relative(GlobalPoint::from(output_position));
                                    let tab_element = SolidColorRenderElement::from_buffer(
                                        &tab_buffer,
                                        tab_output_relative
                                            .as_point()
                                            .to_physical_precise_round(output_scale),
                                        output_scale,
                                        1.0,
                                        smithay::backend::renderer::element::Kind::Unspecified,
                                    );
                                    elements.push(SwlElement::SolidColor(tab_element));

                                    // render separator after this tab (except for the last tab)
                                    if i < tiled.len() - 1 {
                                        let sep_buffer = SolidColorBuffer::new(
                                            (2, workspace::TAB_HEIGHT),
                                            separator_color,
                                        );
                                        // convert separator position from virtual-output-relative to output-relative
                                        let sep_global = VirtualOutputRelativePoint::new(
                                            tab_x + actual_tab_width,
                                            area.location().as_point().y,
                                        )
                                        .to_global(vout.logical_geometry.location());
                                        let sep_output_relative = sep_global
                                            .to_output_relative(GlobalPoint::from(output_position));
                                        let sep_element = SolidColorRenderElement::from_buffer(
                                            &sep_buffer,
                                            sep_output_relative
                                                .as_point()
                                                .to_physical_precise_round(output_scale),
                                            output_scale,
                                            1.0,
                                            smithay::backend::renderer::element::Kind::Unspecified,
                                        );
                                        elements.push(SwlElement::SolidColor(sep_element));
                                    }
                                }
                            }
                        }

                        // Then render borders on top

                        // 1. focused window border overlay
                        if let Some((location, rect_size)) = focused_window_rect {
                            // Convert from global to output-relative coordinates for rendering
                            let global_location = GlobalPoint::from(location);
                            let output_position_typed = GlobalPoint::from(output_position);
                            let output_relative_location =
                                global_location.to_output_relative(output_position_typed);

                            let border_buffer = SolidColorBuffer::new(
                                (
                                    rect_size.w + 2 * BORDER_WIDTH,
                                    rect_size.h + 2 * BORDER_WIDTH,
                                ),
                                FOCUSED_BORDER_COLOR,
                            );
                            let border_element = SolidColorRenderElement::from_buffer(
                                &border_buffer,
                                output_relative_location
                                    .offset_by(-BORDER_WIDTH, -BORDER_WIDTH)
                                    .as_point()
                                    .to_physical_precise_round(output_scale),
                                output_scale,
                                1.0,
                                smithay::backend::renderer::element::Kind::Unspecified,
                            );
                            elements.push(SwlElement::SolidColor(border_element));
                        }

                        // 2. background with unfocused border color for the entire tiling area
                        if !workspace.windows.is_empty() {
                            let available_area = workspace.available_area;
                            if available_area.size().w > 0 && available_area.size().h > 0 {
                                // Convert from virtual-output-relative to global, then to output-relative for rendering
                                let vout_origin = vout.logical_geometry.location();
                                let global_location =
                                    available_area.location().to_global(vout_origin);
                                let output_position_typed = GlobalPoint::from(output_position);
                                let output_relative_location =
                                    global_location.to_output_relative(output_position_typed);

                                let background_buffer = SolidColorBuffer::new(
                                    (available_area.size().w, available_area.size().h),
                                    UNFOCUSED_BORDER_COLOR,
                                );
                                let background_element = SolidColorRenderElement::from_buffer(
                                    &background_buffer,
                                    output_relative_location
                                        .as_point()
                                        .to_physical_precise_round(output_scale),
                                    output_scale,
                                    1.0,
                                    smithay::backend::renderer::element::Kind::Unspecified,
                                );
                                elements.push(SwlElement::SolidColor(background_element));
                            }
                        }
                    }
                }
            }
        }

        // 2. Top layer surfaces (above windows but below overlay)
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
                        surface_elements
                            .into_iter()
                            .map(|elem| SwlElement::Surface(elem)),
                    );
                }
            }
        }

        // 3. Background and Bottom layers (bottommost)
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
                        surface_elements
                            .into_iter()
                            .map(|elem| SwlElement::Surface(elem)),
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
        if let Some(workspace) = self.workspace_containing_window_mut(window) {
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
            if let Some(workspace) = self.workspace_containing_window_mut(&focused) {
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
    pub fn focus_next(&mut self, _output: &Output) {
        tracing::debug!("focus_next called");

        // Find workspace containing the currently focused window
        let workspace_name = self.focused_window.as_ref().and_then(|focused_window| {
            // Find which workspace contains this window
            for (name, workspace) in &self.workspaces {
                if workspace.windows.contains(focused_window) {
                    tracing::debug!("Found focused window in workspace: {}", name);
                    return Some(name.clone());
                }
            }
            None
        });

        let workspace_name = match workspace_name {
            Some(name) => name,
            None => {
                tracing::debug!("No focused window or workspace found, returning");
                return;
            }
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
            let tiled_windows: Vec<&Window> = workspace
                .windows
                .iter()
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
            let current_pos = self
                .focused_window
                .as_ref()
                .and_then(|focused| tiled_windows.iter().position(|w| *w == focused));

            let next_window = if let Some(pos) = current_pos {
                // Move to next window, wrapping around
                let next_pos = (pos + 1) % tiled_windows.len();
                tracing::debug!(
                    "Switching focus from position {} to {} (of {} tiled windows)",
                    pos,
                    next_pos,
                    tiled_windows.len()
                );
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
    pub fn focus_prev(&mut self, _output: &Output) {
        tracing::debug!("focus_prev called");

        // Find workspace containing the currently focused window
        let workspace_name = self.focused_window.as_ref().and_then(|focused_window| {
            // Find which workspace contains this window
            for (name, workspace) in &self.workspaces {
                if workspace.windows.contains(focused_window) {
                    tracing::debug!("Found focused window in workspace: {}", name);
                    return Some(name.clone());
                }
            }
            None
        });

        let workspace_name = match workspace_name {
            Some(name) => name,
            None => {
                tracing::debug!("No focused window or workspace found, returning");
                return;
            }
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
            let tiled_windows: Vec<&Window> = workspace
                .windows
                .iter()
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
            let current_pos = self
                .focused_window
                .as_ref()
                .and_then(|focused| tiled_windows.iter().position(|w| *w == focused));

            let prev_window = if let Some(pos) = current_pos {
                // Move to previous window, wrapping around
                let prev_pos = if pos == 0 {
                    tiled_windows.len() - 1
                } else {
                    pos - 1
                };
                tracing::debug!(
                    "Switching focus from position {} to {} (of {} tiled windows)",
                    pos,
                    prev_pos,
                    tiled_windows.len()
                );
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
        // We only collect from virtual output workspaces (no more physical output workspaces)
        let mut all_windows = Vec::new();

        // Collect from virtual output workspaces only
        for virtual_output in self.virtual_output_manager.all() {
            if let Some(ws_name) = &virtual_output.active_workspace {
                if let Some(workspace) = self.workspaces.get(ws_name) {
                    all_windows.extend(workspace.focus_stack.iter().cloned());
                }
            }
        }

        let focused = all_windows.into_iter().rev().find(|w| w.alive());

        self.focused_window = focused.clone();
        self.update_focused_virtual_output();

        if focused.is_some() {
            tracing::debug!("Refreshed focus to window from focus stack");
        } else {
            tracing::debug!("No alive window in focus stack to focus");
        }

        focused
    }

    /// Set keyboard focus to a window
    pub fn set_focus(&mut self, window: Window) {
        tracing::debug!("Setting focus to window");
        self.focused_window = Some(window.clone());
        self.update_focused_virtual_output();

        // Update the focus stack in the window's workspace
        for workspace in self.workspaces.values_mut() {
            if workspace.windows.contains(&window) {
                workspace.append_focus(&window);

                // In tabbed mode, also update active_tab_index to match focused window
                if matches!(workspace.layout_mode, workspace::LayoutMode::Tabbed) {
                    let idx = workspace
                        .tiled_windows()
                        .enumerate()
                        .find(|(_, w)| *w == &window)
                        .map(|(idx, _)| idx);
                    if let Some(idx) = idx {
                        let old_index = workspace.active_tab_index;
                        workspace.active_tab_index = idx;
                        // trigger rearrangement if tab index changed
                        if old_index != idx {
                            workspace.needs_arrange = true;
                        }
                    }
                }

                break;
            }
        }
    }

    // ========== Workspace Management ==========

    /// Get or create a workspace with the given name
    /// Get workspace name by ID
    fn get_workspace_name(&self, workspace_id: WorkspaceId) -> Option<String> {
        for (name, &id) in &self.workspace_names {
            if id == workspace_id {
                return Some(name.clone());
            }
        }
        None
    }

    /// Get the virtual output and workspace for the currently focused window
    pub fn focused_virtual_output(&self) -> Option<(&VirtualOutput, &Workspace, String)> {
        let focused_window = self.focused_window.as_ref()?;

        // find the workspace containing the focused window
        for (workspace_id, workspace) in &self.workspaces {
            if workspace.windows.contains(focused_window) {
                // find the virtual output with this active workspace
                for virtual_output in self.virtual_output_manager.all() {
                    if virtual_output.active_workspace() == Some(*workspace_id) {
                        let workspace_name = self
                            .get_workspace_name(*workspace_id)
                            .unwrap_or_else(|| format!("workspace-{}", workspace_id.0));
                        return Some((virtual_output, workspace, workspace_name));
                    }
                }
            }
        }

        None
    }

    /// Get the workspace for the currently focused window (mutable)
    pub fn focused_workspace_mut(&mut self) -> Option<&mut Workspace> {
        let focused_window = self.focused_window.as_ref()?.clone();

        // find the workspace containing the focused window
        for workspace in self.workspaces.values_mut() {
            if workspace.windows.contains(&focused_window) {
                return Some(workspace);
            }
        }

        None
    }

    /// Get the physical outputs for the currently focused virtual output
    pub fn focused_physical_outputs(&self) -> Vec<Output> {
        if let Some((virtual_output, _, _)) = self.focused_virtual_output() {
            virtual_output
                .regions
                .iter()
                .map(|r| r.physical_output.clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get the workspace containing a specific window (mutable)
    pub fn workspace_containing_window_mut(&mut self, window: &Window) -> Option<&mut Workspace> {
        for workspace in self.workspaces.values_mut() {
            if workspace.windows.contains(window) {
                return Some(workspace);
            }
        }
        None
    }

    /// Find virtual output at a specific position on the given physical output
    pub fn virtual_output_at_position(
        &self,
        output: &Output,
        position: Point<f64, Logical>,
    ) -> Option<virtual_output::VirtualOutputId> {
        let position_i32 = position.to_i32_round();

        for virtual_output in self
            .virtual_output_manager
            .virtual_outputs_for_physical(output)
        {
            if virtual_output.logical_geometry.contains(position_i32) {
                return Some(virtual_output.id);
            }
        }
        None
    }

    /// Get workspace at a specific position on the given physical output (mutable)
    pub fn workspace_at_position_mut(
        &mut self,
        output: &Output,
        position: Point<f64, Logical>,
    ) -> Option<&mut Workspace> {
        if let Some(virtual_output_id) = self.virtual_output_at_position(output, position) {
            if let Some(virtual_output) = self.virtual_output_manager.get(virtual_output_id) {
                if let Some(workspace_name) = &virtual_output.active_workspace {
                    return self.workspaces.get_mut(workspace_name);
                }
            }
        }
        None
    }

    /// Get all workspace names on a given physical output
    #[allow(dead_code)]
    pub fn workspace_names_on_output(&self, output: &Output) -> Vec<String> {
        self.virtual_output_manager
            .virtual_outputs_for_physical(output)
            .iter()
            .filter_map(|vout| vout.active_workspace())
            .filter_map(|workspace_id| self.get_workspace_name(workspace_id))
            .collect()
    }

    /// Apply a function to all workspaces on a given physical output
    pub fn apply_to_all_workspaces_on_output<F>(&mut self, output: &Output, mut f: F)
    where
        F: FnMut(&mut Workspace),
    {
        let workspace_ids: Vec<WorkspaceId> = self
            .virtual_output_manager
            .virtual_outputs_for_physical(output)
            .iter()
            .filter_map(|vout| vout.active_workspace())
            .collect();
        for workspace_id in workspace_ids {
            if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
                f(workspace);
            }
        }
    }

    /// Check if any workspace on a given physical output needs arrangement
    pub fn any_workspace_needs_arrange_on_output(&self, output: &Output) -> bool {
        for virtual_output in self
            .virtual_output_manager
            .virtual_outputs_for_physical(output)
        {
            if let Some(workspace_name) = &virtual_output.active_workspace {
                if let Some(workspace) = self.workspaces.get(workspace_name) {
                    if workspace.needs_arrange {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Update focused virtual output when focus changes
    pub fn update_focused_virtual_output(&mut self) {
        if let Some((virtual_output, _, _)) = self.focused_virtual_output() {
            self.focused_virtual_output_id = Some(virtual_output.id);
        } else {
            self.focused_virtual_output_id = None;
        }
    }

    /// Switch to a workspace on the given output (delegates to virtual output)
    pub fn switch_to_workspace(&mut self, output: &Output, name: String) {
        tracing::info!(
            "Switching to workspace {} on output {} (via virtual output)",
            name,
            output.name()
        );

        // Find first virtual output on this physical output
        let binding = self
            .virtual_output_manager
            .virtual_outputs_for_physical(output);
        let vout_id = binding.first().map(|vout| vout.id);

        if let Some(vout_id) = vout_id {
            self.switch_workspace_on_virtual(vout_id, &name);
        } else {
            tracing::warn!(
                "No virtual output found for physical output {}",
                output.name()
            );
        }
    }

    /// Arrange windows on the given output according to the tiling layout
    pub fn arrange_windows_on_output(&mut self, output: &Output) {
        // collect virtual output info to avoid borrowing conflicts
        let virtual_output_info: Vec<_> = self
            .virtual_output_manager
            .virtual_outputs_for_physical(output)
            .into_iter()
            .filter_map(|vout| {
                if let Some(workspace_name) = &vout.active_workspace {
                    Some((workspace_name.clone(), vout.logical_geometry, vout.id))
                } else {
                    None
                }
            })
            .collect();

        // Calculate non-exclusive zone from physical output (in output-relative coordinates)
        let non_exclusive_zone = {
            let layer_map = smithay::desktop::layer_map_for_output(output);
            layer_map.non_exclusive_zone()
        };

        // Convert non-exclusive zone to global coordinates
        let output_position = output.current_location_typed();
        let non_exclusive_zone_origin =
            OutputRelativePoint::new(non_exclusive_zone.loc.x, non_exclusive_zone.loc.y);
        let non_exclusive_zone_global_origin = non_exclusive_zone_origin.to_global(output_position);
        let non_exclusive_zone_global = GlobalRect::from_loc_and_size(
            non_exclusive_zone_global_origin,
            non_exclusive_zone.size,
        );

        for (workspace_name, logical_geometry, _vout_id) in virtual_output_info {
            if let Some(workspace) = self.workspaces.get_mut(&workspace_name) {
                // Intersect virtual output geometry with non-exclusive zone
                // For now, assume 1:1 virtual output, so use the non-exclusive zone directly
                // TODO: For multi-virtual output, need to calculate intersection properly
                let available_geometry_global = if self
                    .virtual_output_manager
                    .virtual_outputs_for_physical(output)
                    .len()
                    == 1
                {
                    // Single virtual output - use full non-exclusive zone in global coords
                    non_exclusive_zone_global.as_rectangle()
                } else {
                    // Multiple virtual outputs - intersect with virtual output bounds
                    // This is a simplified version - proper implementation would clip to virtual output region
                    non_exclusive_zone_global
                        .as_rectangle()
                        .intersection(logical_geometry.as_rectangle())
                        .unwrap_or(logical_geometry.as_rectangle())
                };

                // Convert to virtual-output-relative coordinates (translate to origin)
                let vout_origin = logical_geometry.location();
                let available_global_origin = GlobalPoint::new(
                    available_geometry_global.loc.x,
                    available_geometry_global.loc.y,
                );
                let available_relative_origin = VirtualOutputRelativePoint::new(
                    available_global_origin.as_point().x - vout_origin.as_point().x,
                    available_global_origin.as_point().y - vout_origin.as_point().y,
                );
                let available_geometry_relative = VirtualOutputRelativeRect::from_loc_and_size(
                    available_relative_origin,
                    available_geometry_global.size,
                );

                workspace.update_output_geometry(available_geometry_relative);

                // clean up dead windows first
                workspace.refresh();

                // validate workspace consistency for debugging
                workspace.validate_consistency();

                // handle fullscreen window first
                if let Some(fullscreen_window) = &workspace.fullscreen {
                    // for fullscreen, we need the actual output's logical size after transform
                    // the virtual output's logical_geometry might be pre-transform
                    let fullscreen_size = if let Some(mode) = output.current_mode() {
                        let transform = output.current_transform();
                        let scale = output.current_scale().fractional_scale();

                        // apply transform to get the logical size
                        let (width, height) = match transform {
                            smithay::utils::Transform::_90
                            | smithay::utils::Transform::_270
                            | smithay::utils::Transform::Flipped90
                            | smithay::utils::Transform::Flipped270 => {
                                // rotated: swap width and height
                                (
                                    (mode.size.h as f64 / scale) as i32,
                                    (mode.size.w as f64 / scale) as i32,
                                )
                            }
                            _ => {
                                // not rotated
                                (
                                    (mode.size.w as f64 / scale) as i32,
                                    (mode.size.h as f64 / scale) as i32,
                                )
                            }
                        };
                        smithay::utils::Size::from((width, height))
                    } else {
                        logical_geometry.size()
                    };

                    // position fullscreen window at virtual output origin
                    self.space.map_element(
                        fullscreen_window.clone(),
                        logical_geometry.location().as_point(),
                        false,
                    );

                    if let Some(toplevel) = fullscreen_window.toplevel() {
                        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;

                        toplevel.with_pending_state(|state| {
                            state.size = Some(fullscreen_size);
                            state.bounds = Some(fullscreen_size);
                            state.states.set(State::Fullscreen);

                            // remove tiled states
                            state.states.unset(State::TiledLeft);
                            state.states.unset(State::TiledRight);
                            state.states.unset(State::TiledTop);
                            state.states.unset(State::TiledBottom);
                        });

                        if toplevel.is_initial_configure_sent() {
                            toplevel.send_configure();
                        }
                    }

                    workspace.needs_arrange = false;
                    continue; // don't arrange other windows when one is fullscreen
                }

                // get tiled windows
                let windows_to_tile: Vec<_> = workspace.tiled_windows().cloned().collect();

                match workspace.layout_mode {
                    workspace::LayoutMode::Tiling => {
                        // get tile positions
                        let positions = workspace.tiling.tile(&windows_to_tile);

                        // clear old cached rectangles for tiled windows
                        for window in &windows_to_tile {
                            workspace.window_rectangles.remove(window);
                        }

                        // apply positions and sizes
                        for (window, rect) in positions {
                            // cache the rectangle for this window
                            workspace
                                .window_rectangles
                                .insert(window.clone(), VirtualOutputRelativeRect::from(rect));

                            // position the window, accounting for CSD shadow offsets and virtual output global position
                            let window_geom = window.geometry();
                            let vout_origin = logical_geometry.location();
                            let rect_origin =
                                VirtualOutputRelativePoint::new(rect.loc.x, rect.loc.y);
                            let window_global = rect_origin.to_global(vout_origin);
                            let position = GlobalPoint::new(
                                window_global.as_point().x - window_geom.loc.x,
                                window_global.as_point().y - window_geom.loc.y,
                            );
                            tracing::debug!("Tiling: positioning window at global {:?} (vout offset {:?} + local {:?} - geom {:?})", 
                                position, vout_origin.as_point(), rect.loc, window_geom.loc);
                            self.space
                                .map_element(window.clone(), position.as_point(), false);

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
                                });

                                // send the configure event
                                if toplevel.is_initial_configure_sent() {
                                    toplevel.send_configure();
                                }
                            }
                        }
                    }
                    workspace::LayoutMode::Tabbed => {
                        // hide all tiled windows first
                        for window in &windows_to_tile {
                            self.space.unmap_elem(window);
                        }

                        // show only the active tab
                        if let Some(active_window) = windows_to_tile.get(workspace.active_tab_index)
                        {
                            let available_area = workspace.available_area;
                            let window_rect = VirtualOutputRelativeRect::with_y_offset(
                                &available_area,
                                workspace::TAB_HEIGHT,
                            );

                            // cache the rectangle
                            workspace
                                .window_rectangles
                                .insert(active_window.clone(), window_rect);

                            // map the active window, accounting for CSD shadow offsets and virtual output global position
                            let window_geom = active_window.geometry();
                            let vout_origin = logical_geometry.location();
                            let window_global = window_rect.location().to_global(vout_origin);
                            let position = GlobalPoint::new(
                                window_global.as_point().x - window_geom.loc.x,
                                window_global.as_point().y - window_geom.loc.y,
                            );
                            self.space.map_element(
                                active_window.clone(),
                                position.as_point(),
                                false,
                            );

                            // configure the window
                            if let Some(toplevel) = active_window.toplevel() {
                                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
                                use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;

                                toplevel.with_pending_state(|state| {
                                    state.size = Some(window_rect.size());
                                    state.bounds = Some(window_rect.size());

                                    // force server-side decorations (no client decorations)
                                    state.decoration_mode = Some(Mode::ServerSide);

                                    // set tiled states to remove decorations and inform the client
                                    state.states.set(State::TiledLeft);
                                    state.states.set(State::TiledRight);
                                    state.states.set(State::TiledTop);
                                    state.states.set(State::TiledBottom);
                                });

                                // send the configure event
                                if toplevel.is_initial_configure_sent() {
                                    toplevel.send_configure();
                                }
                            }
                        }
                    }
                }

                workspace.needs_arrange = false;
            }
        }
    }

    /// Remove a window from all workspaces
    pub fn remove_window(&mut self, window: &Window) -> Vec<Output> {
        let mut found_workspace_name = None;

        // Find and remove from all workspaces
        for (workspace_name, workspace) in self.workspaces.iter_mut() {
            if workspace.remove_window(window) {
                found_workspace_name = Some(workspace_name.clone());
                break;
            }
        }

        // Clear focused window if it was removed
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = None;
            self.update_focused_virtual_output();
        }

        // Unmap from space
        self.space.unmap_elem(window);

        // Find all affected outputs via virtual output manager
        if let Some(workspace_name) = found_workspace_name {
            for virtual_output in self.virtual_output_manager.all() {
                if virtual_output.active_workspace.as_ref() == Some(&workspace_name) {
                    return virtual_output
                        .regions
                        .iter()
                        .map(|r| r.physical_output.clone())
                        .collect();
                }
            }
        }

        Vec::new()
    }

    /// Handle tab click at the given position
    pub fn handle_tab_click(&mut self, output: &Output, point: Point<f64, Logical>) -> bool {
        if let Some(workspace) = self.workspace_at_position_mut(output, point) {
            if !matches!(workspace.layout_mode, workspace::LayoutMode::Tabbed) {
                return false;
            }

            let area = workspace.available_area;
            if point.y >= area.location().as_point().y as f64
                && point.y < (area.location().as_point().y + workspace::TAB_HEIGHT) as f64
            {
                let tiled_count = workspace.tiled_windows().count();
                if tiled_count > 0 {
                    let tab_width = area.size().w / tiled_count as i32;
                    let relative_x = (point.x - area.location().as_point().x as f64) as i32;

                    // Find which tab was clicked, accounting for separators
                    for i in 0..tiled_count {
                        let tab_start = i as i32 * tab_width;
                        let tab_end = if i < tiled_count - 1 {
                            tab_start + tab_width - 2 // Account for separator
                        } else {
                            (i + 1) as i32 * tab_width // Last tab takes full width
                        };

                        if relative_x >= tab_start && relative_x < tab_end {
                            workspace.active_tab_index = i;
                            workspace.needs_arrange = true;

                            // Update focus to the clicked tab
                            let window = workspace.tiled_windows().nth(i).cloned();
                            if let Some(window) = window {
                                workspace.append_focus(&window);
                                self.focused_window = Some(window);
                                self.update_focused_virtual_output();
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
