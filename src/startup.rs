// SPDX-License-Identifier: GPL-3.0-only

use std::{
    env,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};
use tracing::{info, warn, error};

/// Find and execute the startup program
/// 
/// Checks in order:
/// 1. SWL_RUN environment variable (path to executable)
/// 2. $XDG_CONFIG_HOME/swl/run
/// 3. $HOME/.config/swl/run
pub fn run_startup_program() {
    // First check SWL_RUN environment variable
    if let Ok(program) = env::var("SWL_RUN") {
        info!("Running startup program from SWL_RUN: {}", program);
        execute_program(&program);
        return;
    }
    
    // Then check config directories
    let config_path = find_config_program();
    if let Some(path) = config_path {
        info!("Running startup program from: {}", path.display());
        execute_program(path.to_string_lossy().as_ref());
    } else {
        info!("No startup program found, skipping");
    }
}

fn find_config_program() -> Option<PathBuf> {
    // Try XDG_CONFIG_HOME first
    if let Ok(xdg_config) = env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(xdg_config).join("swl/run");
        if path.exists() {
            return Some(path);
        }
    }
    
    // Fall back to $HOME/.config/swl/run
    if let Ok(home) = env::var("HOME") {
        let path = PathBuf::from(home).join(".config/swl/run");
        if path.exists() {
            return Some(path);
        }
    }
    
    None
}

fn execute_program(program_path: &str) {
    // Check if the program is executable
    let path = PathBuf::from(program_path);
    if path.exists() && !is_executable(&path) {
        warn!("Startup program {} exists but is not executable", program_path);
        return;
    }
    
    // Fork and execute the program directly
    // The program will inherit all environment variables including WAYLAND_DISPLAY
    match Command::new(program_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Detach the child process
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        warn!("Startup program exited with status: {}", status);
                    }
                }
                Ok(None) => {
                    // Child is still running, which is fine
                    info!("Startup program launched successfully");
                }
                Err(e) => {
                    error!("Failed to check startup program status: {}", e);
                }
            }
        }
        Err(e) => {
            error!("Failed to execute startup program: {}", e);
        }
    }
}

#[cfg(unix)]
fn is_executable(path: &PathBuf) -> bool {
    use std::os::unix::fs::PermissionsExt;
    
    match fs::metadata(path) {
        Ok(metadata) => {
            let permissions = metadata.permissions();
            // Check if any execute bit is set (user, group, or other)
            permissions.mode() & 0o111 != 0
        }
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(_path: &PathBuf) -> bool {
    // On non-Unix systems, assume scripts are executable
    true
}