// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    backend::input::KeyState,
    input::keyboard::{keysyms as xkb, Keysym, ModifiersState},
};
use tracing::debug;

/// Actions that can be triggered by keybindings
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // window management
    FocusNext,
    FocusPrev,
    Zoom,
    CloseWindow,
    ToggleFloating,
    Fullscreen,
    
    // layout control
    IncreaseMasterWidth,
    DecreaseMasterWidth, 
    IncreaseMasterCount,
    DecreaseMasterCount,
    
    // tabbed mode
    ToggleLayoutMode,
    NextTab,
    PrevTab,
    
    // applications
    LaunchTerminal,
    LaunchMenu,
    
    // workspace management
    SwitchToWorkspace(String),
    MoveToWorkspace(String),
    
    // system
    Quit,
    VtSwitch(i32),
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
        // check for exact modifier match
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
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_f,
            Action::Fullscreen,
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
        
        // tabbed mode
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_t,
            Action::ToggleLayoutMode,
        ));
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_Tab,
            Action::NextTab,
        ));
        bindings.push(Keybinding::new(
            ModifiersState {
                shift: true,
                ..modkey
            },
            xkb::KEY_Tab,
            Action::PrevTab,
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
        // quit - Super+Shift+e
        bindings.push(Keybinding::new(
            ModifiersState {
                shift: true,
                ..modkey
            },
            xkb::KEY_e,  // lowercase e, since we now use raw_latin_sym_or_raw_current_sym
            Action::Quit,
        ));
        
        // Workspace switching - Super+1-9 and Super+0 for workspace 10
        for i in 1..=9 {
            bindings.push(Keybinding::new(
                modkey,
                xkb::KEY_1 + (i - 1),
                Action::SwitchToWorkspace(i.to_string()),
            ));
        }
        // Super+0 for workspace 10
        bindings.push(Keybinding::new(
            modkey,
            xkb::KEY_0,
            Action::SwitchToWorkspace("10".to_string()),
        ));
        
        // Move window to workspace - Super+Shift+1-9 and Super+Shift+0 for workspace 10
        for i in 1..=9 {
            bindings.push(Keybinding::new(
                ModifiersState {
                    shift: true,
                    ..modkey
                },
                xkb::KEY_1 + (i - 1),
                Action::MoveToWorkspace(i.to_string()),
            ));
        }
        // Super+Shift+0 for workspace 10
        bindings.push(Keybinding::new(
            ModifiersState {
                shift: true,
                ..modkey
            },
            xkb::KEY_0,
            Action::MoveToWorkspace("10".to_string()),
        ));
        
        // VT switching - Ctrl+Alt+F1-F12 
        for vt in 1..=12 {
            bindings.push(Keybinding::new(
                ModifiersState {
                    ctrl: true,
                    alt: true,
                    ..Default::default()
                },
                xkb::KEY_F1 + (vt - 1),
                Action::VtSwitch(vt as i32),
            ));
        }
        
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
                return Some(binding.action.clone());
            }
        }
        
        None
    }
}