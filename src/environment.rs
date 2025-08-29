// SPDX-License-Identifier: GPL-3.0-only

use std::process::Command;
use tracing::{info, warn, error};

/// Update systemd user environment with WAYLAND_DISPLAY
pub fn update_systemd_environment(socket_name: &str) {
    // check if we're running under systemd
    if std::env::var("SYSTEMD_EXEC_PID").is_ok() || std::path::Path::new("/run/systemd/system").exists() {
        info!("Updating systemd user environment with WAYLAND_DISPLAY={}", socket_name);
        
        match Command::new("systemctl")
            .args(["--user", "import-environment", "WAYLAND_DISPLAY"])
            .env("WAYLAND_DISPLAY", socket_name)
            .status()
        {
            Ok(status) if status.success() => {
                info!("Successfully updated systemd environment");
            }
            Ok(status) => {
                warn!("Failed to import WAYLAND_DISPLAY into systemd: {:?}", status.code());
            }
            Err(err) => {
                error!("Failed to run systemctl: {}", err);
            }
        }
    }
}

/// Update D-Bus activation environment with WAYLAND_DISPLAY
pub fn update_dbus_environment(socket_name: &str) {
    info!("Updating D-Bus activation environment with WAYLAND_DISPLAY={}", socket_name);
    
    match Command::new("dbus-update-activation-environment")
        .arg("--systemd")
        .arg(format!("WAYLAND_DISPLAY={}", socket_name))
        .status()
    {
        Ok(status) if status.success() => {
            info!("Successfully updated D-Bus activation environment");
        }
        Ok(status) => {
            warn!("Failed to update D-Bus activation environment: {:?}", status.code());
        }
        Err(err) => {
            // dbus-update-activation-environment might not be available
            info!("Could not update D-Bus activation environment: {} (this is ok if not using D-Bus)", err);
        }
    }
}

/// Update both systemd and D-Bus environments
pub fn update_environment(socket_name: &str) {
    update_systemd_environment(socket_name);
    update_dbus_environment(socket_name);
}