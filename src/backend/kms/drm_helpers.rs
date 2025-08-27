// SPDX-License-Identifier: GPL-3.0-only

use anyhow::Result;
use smithay::{
    backend::drm::{DrmDevice, DrmNode},
    reexports::drm::control::{
        connector::{self, State as ConnectorState, Interface},
        crtc,
        Device as ControlDevice,
        Mode,
    },
};
use std::collections::HashMap;
use tracing::{info, debug};

/// Find existing display configuration from the DRM device
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

/// Get the interface name for a connector
pub fn interface_name(drm: &mut DrmDevice, conn: connector::Handle) -> Result<String> {
    let conn_info = drm.get_connector(conn, false)?;
    let interface = conn_info.interface();
    let interface_id = conn_info.interface_id();
    
    Ok(format!("{}-{}", interface_short_name(interface), interface_id))
}

/// Get short name for connector interface type
fn interface_short_name(interface: Interface) -> &'static str {
    match interface {
        Interface::DVII => "DVI-I",
        Interface::DVID => "DVI-D",
        Interface::DVIA => "DVI-A",
        Interface::Composite => "Composite",
        Interface::SVideo => "S-VIDEO",
        Interface::LVDS => "LVDS",
        Interface::Component => "Component",
        Interface::NinePinDIN => "DIN",
        Interface::DisplayPort => "DP",
        Interface::HDMIA => "HDMI-A",
        Interface::HDMIB => "HDMI-B",
        Interface::TV => "TV",
        Interface::EmbeddedDisplayPort => "eDP",
        Interface::Virtual => "Virtual",
        Interface::DSI => "DSI",
        Interface::DPI => "DPI",
        Interface::Writeback => "Writeback",
        Interface::SPI => "SPI",
        Interface::Unknown => "Unknown",
        _ => "Unknown",
    }
}

/// Get EDID information for a connector
pub fn edid_info(_drm: &mut DrmDevice, _conn: connector::Handle) -> Result<EdidInfo> {
    // simplified - cosmic-comp reads actual EDID data from kernel
    // for now just return dummy data
    Ok(EdidInfo {
        make: None,
        model: None,
        serial: None,
    })
}

/// Placeholder for EDID information
pub struct EdidInfo {
    make: Option<String>,
    model: Option<String>,
    #[allow(dead_code)] // may be used in future for serial tracking
    serial: Option<String>,
}

impl EdidInfo {
    pub fn make(&self) -> Option<String> {
        self.make.clone()
    }
    
    pub fn model(&self) -> Option<String> {
        self.model.clone()
    }
    
    #[allow(dead_code)] // may be used in future for serial tracking
    pub fn serial(&self) -> Option<String> {
        self.serial.clone()
    }
}

/// Calculate refresh rate from a DRM mode
pub fn calculate_refresh_rate(mode: Mode) -> u32 {
    // using cosmic-comp's implementation
    let htotal = mode.hsync().2 as u32;
    let vtotal = mode.vsync().2 as u32;
    // calculate refresh rate in millihertz (1000 mHz = 1 Hz)
    let refresh = (mode.clock() as u64 * 1000000_u64 / htotal as u64 + vtotal as u64 / 2) / vtotal as u64;
    
    // simplified - cosmic-comp also handles interlace/dblscan flags
    refresh as u32
}