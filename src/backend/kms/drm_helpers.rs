// SPDX-License-Identifier: GPL-3.0-only

use anyhow::Result;
use smithay::{
    backend::drm::DrmNode,
    reexports::drm::control::{
        connector::{self, State as ConnectorState},
        crtc,
        Device as ControlDevice,
    },
};
use std::collections::HashMap;
use tracing::{info, debug};

/// Find existing display configuration from the DRM device
#[allow(dead_code)] // will be used for output management in Phase 2f
pub fn display_configuration(
    device: &mut impl ControlDevice,
    _supports_atomic: bool,
) -> Result<HashMap<connector::Handle, Option<crtc::Handle>>> {
    let res_handles = device.resource_handles()?;
    let connectors = res_handles.connectors();
    
    let mut map = HashMap::new();
    
    // try to keep existing mappings to reduce flickering
    for conn in connectors
        .iter()
        .flat_map(|conn| device.get_connector(*conn, true).ok())
    {
        if let Some(enc) = conn.current_encoder() {
            if let Some(crtc) = device.get_encoder(enc)?.crtc() {
                if conn.state() == ConnectorState::Connected {
                    debug!("Found existing mapping: {:?} -> {:?}", conn.handle(), crtc);
                    map.insert(conn.handle(), Some(crtc));
                }
            }
        }
    }
    
    // match remaining connected connectors to available crtcs
    let unmatched_connectors: Vec<_> = connectors
        .iter()
        .flat_map(|conn| device.get_connector(*conn, false).ok())
        .filter(|conn| conn.state() == ConnectorState::Connected)
        .filter(|conn| !map.contains_key(&conn.handle()))
        .collect();
    
    for conn in unmatched_connectors {
        'outer: for encoder_info in conn
            .encoders()
            .iter()
            .flat_map(|encoder_handle| device.get_encoder(*encoder_handle))
        {
            for crtc in res_handles.filter_crtcs(encoder_info.possible_crtcs()) {
                if !map.values().any(|v| *v == Some(crtc)) {
                    debug!("Assigning connector {:?} to CRTC {:?}", conn.handle(), crtc);
                    map.insert(conn.handle(), Some(crtc));
                    break 'outer;
                }
            }
        }
        
        map.entry(conn.handle()).or_insert(None);
    }
    
    Ok(map)
}

/// Detect primary GPU based on boot_vga flag
#[allow(dead_code)] // will be used for multi-GPU support
pub fn find_primary_gpu(nodes: &[DrmNode]) -> Option<DrmNode> {
    // check for boot_vga flag to identify primary GPU
    for node in nodes {
        if let Some(path) = node.dev_path() {
            let boot_vga = path
                .parent()
                .and_then(|p| std::fs::read_to_string(p.join("boot_vga")).ok())
                .and_then(|s| s.trim().parse::<i32>().ok())
                .unwrap_or(0);
            
            if boot_vga == 1 {
                info!("Found primary GPU with boot_vga flag: {:?}", node);
                return Some(node.clone());
            }
        }
    }
    
    // fallback to first available node
    nodes.first().cloned()
}