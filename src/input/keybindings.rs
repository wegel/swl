// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::input::KeyState,
    input::keyboard::{keysyms as xkb, Keysym, ModifiersState},
};
use tracing::debug;

/// Actions that can be triggered by keybindings
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // window management
    FocusNext,
    FocusPrev,
    Zoom,
    CloseWindow,
    ToggleFloating,
    
    // layout control
    IncreaseMasterWidth,
    DecreaseMasterWidth, 
    IncreaseMasterCount,
    DecreaseMasterCount,
    
    // applications
    LaunchTerminal,
    LaunchMenu,
    
    // system
    Quit,
}

/// A keybinding definition
#[derive(Debug, Clone)]
pub struct Keybinding {
    pub modifiers: ModifiersState,
    pub key: u32,
    pub action: Action,
}

impl Keybinding {
    /// Create a new keybinding
    pub fn new(modifiers: ModifiersState, key: u32, action: Action) -> Self {
        Self { modifiers, key, action }
    }
    
    /// Check if this keybinding matches the given modifiers and key
    pub fn matches(&self, modifiers: &ModifiersState, key: Keysym) -> bool {
        // check for exact modifier match (like cosmic-comp does)
        let mod_match = self.modifiers.ctrl == modifiers.ctrl
            && self.modifiers.alt == modifiers.alt
            && self.modifiers.shift == modifiers.shift
            && self.modifiers.logo == modifiers.logo;
        
        mod_match && self.key == key.raw()
    }
}

/// Keybinding configuration
pub struct Keybindings {
    bindings: Vec<Keybinding>,
}

impl Keybindings {
    /// Create default keybindings
    pub fn new() -> Self {
        let modkey = Self::get_modkey();
        
        let mut bindings = Vec::new();
        
        // window management
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_j,
            Action::FocusNext,
        ));
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_k,
            Action::FocusPrev,
        ));
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_m,
            Action::Zoom,
        ));
        // close window
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_q,
            Action::CloseWindow,
        ));
        bindings.push(Keybinding::new(
            ModifiersState {
                shift: true,
                ..modkey
            },
            xkb::KEY_space,
            Action::ToggleFloating,
        ));
        
        // layout control
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_h,
            Action::DecreaseMasterWidth,
        ));
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_l,
            Action::IncreaseMasterWidth,
        ));
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_i,
            Action::IncreaseMasterCount,
        ));
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_comma,
            Action::DecreaseMasterCount,
        ));
        
        // applications
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_Return,
            Action::LaunchTerminal,
        ));
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_d,
            Action::LaunchMenu,
        ));
        
        // system
        // quit - handle uppercase E when shift is pressed
        bindings.push(Keybinding::new(
            ModifiersState {
                shift: true,
                ..modkey
            },
            xkb::KEY_E,  // uppercase when shift is pressed
            Action::Quit,
        ));
        
        debug!("Initialized {} keybindings", bindings.len());
        
        Self { bindings }
    }
    
    /// Get the modifier key from environment or default to Super
    fn get_modkey() -> ModifiersState {
        let modkey_str = std::env::var("SWL_MODKEY").unwrap_or_else(|_| "super".to_string());
        
        match modkey_str.to_lowercase().as_str() {
            "alt" => ModifiersState {
                alt: true,
                ..Default::default()
            },
            "super" | "logo" | "win" | "windows" | _ => ModifiersState {
                logo: true,
                ..Default::default()
            },
        }
    }
    
    /// Check if any keybinding matches and return its action
    pub fn check(&self, modifiers: &ModifiersState, key: Keysym, key_state: KeyState) -> Option<Action> {
        // only trigger on key press, not release
        if key_state != KeyState::Pressed {
            return None;
        }
        
        for binding in &self.bindings {
            if binding.matches(modifiers, key) {
                tracing::debug!("Keybinding matched: {:?}", binding.action);
                return Some(binding.action);
            }
        }
        
        None
    }
}