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
        
        // get the output for this layer surface
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.outputs.first().cloned());
            
        if let Some(output) = output {
            // create the layer surface
            let layer_surface = LayerSurface::new(surface, namespace);
            
            // map it to the output
            let mut layer_map = layer_map_for_output(&output);
            layer_map.map_layer(&layer_surface).unwrap();
            
            // arrange layers to compute proper geometry
            layer_map.arrange();
            
            // now send configure with the computed dimensions
            layer_surface.layer_surface().send_configure();
            
            debug!("Layer surface mapped to output {}", output.name());
            
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