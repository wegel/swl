// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    delegate_layer_shell,
    desktop::{LayerSurface, layer_map_for_output, WindowSurfaceType},
    output::Output,
    reexports::wayland_server::protocol::wl_output::WlOutput,
    wayland::shell::{
        wlr_layer::{
            Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
        },
        xdg::PopupSurface,
    },
};
use tracing::{debug, info};

use crate::State;

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }
    
    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        info!("New layer surface requested: {} on layer {:?}", namespace, layer);
        
        // Debug: log the surface's desired size
        debug!("Layer surface {} created for output {:?}", namespace, 
            wl_output.as_ref().and_then(Output::from_resource).map(|o| o.name()));
        
        // get the output for this layer surface
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.outputs.first().cloned());
            
        if let Some(output) = output {
            debug!("Output {} geometry for layer surface: {:?}", output.name(), output.current_mode());
            
            // create the layer surface
            let layer_surface = LayerSurface::new(surface, namespace.clone());
            
            // map it to the output
            let mut layer_map = layer_map_for_output(&output);
            layer_map.map_layer(&layer_surface).unwrap();
            
            // arrange layers to compute proper geometry
            let changed = layer_map.arrange();
            
            // now send configure with the computed dimensions
            layer_surface.layer_surface().send_configure();
            
            debug!("Layer surface mapped to output {}", output.name());
            
            debug!("Layer surface {} mapped to output {} and configure sent", namespace, output.name());
            
            // if layer arrangement changed (e.g. new exclusive zone), re-arrange windows
            if changed {
                debug!("Layer arrangement changed, windows will be re-arranged on next render");
                // Don't call arrange() here as it may cause deadlock
                // It will be called when the layer surface commits
            }
            
            // keyboard focus will be handled in commit handler when the surface is ready
            
            // schedule render for the output
            self.backend.schedule_render(&output);
        } else {
            debug!("No output available for layer surface");
        }
    }
    
    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        info!("Layer surface destroyed - START");
        
        // find which output has this layer surface
        let maybe_output = self.outputs.iter().find(|o| {
            let map = layer_map_for_output(o);
            map.layer_for_surface(surface.wl_surface(), WindowSurfaceType::TOPLEVEL)
                .is_some()
        }).cloned();
        
        if let Some(output) = maybe_output {
            info!("Found output for layer surface");
            // unmap the layer
            let mut map = layer_map_for_output(&output);
            if let Some(layer) = map.layer_for_surface(surface.wl_surface(), WindowSurfaceType::TOPLEVEL) {
                let layer = layer.clone();
                map.unmap_layer(&layer);
                info!("Layer surface unmapped from output {}", output.name());
            }
            
            // re-arrange layers and windows if exclusive zones changed
            map.arrange();
            
            // Always mark windows for re-arrangement when a layer surface is destroyed
            // as it may have had exclusive zones that affected window layout
            let mut shell = self.shell.write().unwrap();
            shell.apply_to_all_workspaces_on_output(&output, |workspace| {
                workspace.needs_arrange = true;
            });
            
            // schedule render for the output
            self.backend.schedule_render(&output);
            
            // mark that we need to refresh focus (will happen in main loop)
            self.needs_focus_refresh = true;
            info!("Marked for focus refresh");
        }
        
        info!("Layer surface destroyed - END");
    }
    
    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        // configure the popup
        let _ = popup.send_configure();
    }
}

delegate_layer_shell!(State);