// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    output::{Mode, Output, WeakOutput},
    reexports::{
        wayland_protocols_wlr::output_management::v1::server::{
            zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
            zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
            zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
            zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
            zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
            Resource,
        },
    },
    utils::{Logical, Physical, Point, Size, Transform},
};
use std::{convert::TryInto, sync::Mutex};

pub trait OutputConfigurationHandler: Sized {
    fn output_configuration_state(&mut self) -> &mut OutputConfigurationState;
    fn test_configuration(&mut self, configs: Vec<(Output, OutputConfiguration)>) -> bool;
    fn apply_configuration(&mut self, configs: Vec<(Output, OutputConfiguration)>) -> bool;
}

pub struct OutputMngrGlobalData {
    filter: Box<dyn for<'a> Fn(&'a Client) -> bool + Send + Sync>,
}

#[derive(Debug)]
pub struct OutputConfigurationState {
    outputs: Vec<Output>,
    instances: Vec<OutputMngrInstance>,
    serial_counter: u32,
    _global: GlobalId, // kept alive to maintain global
    dh: DisplayHandle,
}

#[derive(Debug)]
struct OutputMngrInstance {
    obj: ZwlrOutputManagerV1,
    heads: Vec<OutputHeadInstance>,
}

#[derive(Debug)]
struct OutputHeadInstance {
    obj: ZwlrOutputHeadV1,
    output: Output,
    modes: Vec<ZwlrOutputModeV1>,
}

#[derive(Debug, Default)]
pub struct PendingConfigurationInner {
    serial: u32,
    used: bool,
    heads: Vec<(ZwlrOutputHeadV1, Option<ZwlrOutputConfigurationHeadV1>)>,
}

pub type PendingConfiguration = Mutex<PendingConfigurationInner>;

#[derive(Debug, Clone)]
pub enum ModeConfiguration {
    Mode(Mode),
    Custom {
        size: Size<i32, Physical>,
        refresh: Option<i32>,
    },
}

#[derive(Debug, Default, Clone)]
pub struct PendingOutputConfigurationInner {
    mode: Option<ModeConfiguration>,
    position: Option<Point<i32, Logical>>,
    transform: Option<Transform>,
    scale: Option<f64>,
    adaptive_sync: Option<bool>,
}

pub type PendingOutputConfiguration = Mutex<PendingOutputConfigurationInner>;

#[derive(Debug, Clone)]
pub enum OutputConfiguration {
    Enabled {
        mode: Option<ModeConfiguration>,
        position: Option<Point<i32, Logical>>,
        transform: Option<Transform>,
        scale: Option<f64>,
        #[allow(dead_code)] // stored but not yet implemented in compositor
        adaptive_sync: Option<bool>,
    },
    Disabled,
}

impl TryFrom<&mut PendingOutputConfigurationInner> for OutputConfiguration {
    type Error = zwlr_output_configuration_head_v1::Error;
    
    fn try_from(value: &mut PendingOutputConfigurationInner) -> Result<OutputConfiguration, Self::Error> {
        Ok(OutputConfiguration::Enabled {
            mode: value.mode.clone(),
            position: value.position,
            transform: value.transform,
            scale: value.scale,
            adaptive_sync: value.adaptive_sync,
        })
    }
}

impl OutputConfigurationState {
    pub fn new<F>(
        dh: &DisplayHandle,
        client_filter: F,
    ) -> OutputConfigurationState
    where
        F: for<'a> Fn(&'a Client) -> bool + Clone + Send + Sync + 'static,
    {
        let global = dh.create_global::<State, ZwlrOutputManagerV1, _>(
            4,
            OutputMngrGlobalData {
                filter: Box::new(client_filter),
            },
        );

        OutputConfigurationState {
            outputs: Vec::new(),
            instances: Vec::new(),
            serial_counter: 0,
            _global: global,
            dh: dh.clone(),
        }
    }

    pub fn add_heads<'a>(&mut self, outputs: impl Iterator<Item = &'a Output>) {
        let new_outputs = outputs
            .filter(|o| !self.outputs.contains(o))
            .collect::<Vec<_>>();

        for output in new_outputs {
            self.outputs.push(output.clone());
        }
    }

    #[allow(dead_code)] // TODO: should be called when outputs are removed
    pub fn remove_heads<'a>(&mut self, outputs: impl Iterator<Item = &'a Output>) {
        let to_remove: Vec<_> = outputs.cloned().collect();
        
        // notify clients about removed heads
        for output in &to_remove {
            for instance in &mut self.instances {
                for head in instance.heads.iter_mut().filter(|h| &h.output == output) {
                    // send finished event for modes
                    for mode in &head.modes {
                        mode.finished();
                    }
                    // send finished event for head
                    head.obj.finished();
                }
                // remove the head from the instance
                instance.heads.retain(|h| !to_remove.contains(&h.output));
            }
        }
        
        self.outputs.retain(|o| !to_remove.contains(o));
    }

    pub fn update(&mut self) {
        self.serial_counter += 1;

        // update all clients with current output state
        for manager in self.instances.iter_mut() {
            for output in &self.outputs {
                send_head_to_client::<State>(&self.dh, manager, output);
            }
            manager.obj.done(self.serial_counter);
        }
    }
}

fn send_head_to_client<D>(dh: &DisplayHandle, mngr: &mut OutputMngrInstance, output: &Output)
where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputMngrGlobalData>
        + Dispatch<ZwlrOutputManagerV1, ()>
        + Dispatch<ZwlrOutputHeadV1, WeakOutput>
        + Dispatch<ZwlrOutputModeV1, Mode>
        + Dispatch<ZwlrOutputConfigurationV1, PendingConfiguration>
        + OutputConfigurationHandler
        + 'static,
{
    let instance = match mngr.heads.iter_mut().find(|i| i.output == *output) {
        Some(i) => i,
        None => {
            // create new head
            if let Ok(client) = dh.get_client(mngr.obj.id()) {
                if let Ok(head) = client.create_resource::<ZwlrOutputHeadV1, _, D>(
                    dh,
                    mngr.obj.version(),
                    output.downgrade(),
                ) {
                    mngr.obj.head(&head);
                    let data = OutputHeadInstance {
                        obj: head,
                        modes: Vec::new(),
                        output: output.clone(),
                    };
                    mngr.heads.push(data);
                    mngr.heads.last_mut().unwrap()
                } else {
                    return;
                }
            } else {
                return;
            }
        }
    };

    // send output properties
    instance.obj.name(output.name());
    instance.obj.description(output.description());
    
    let physical = output.physical_properties();
    if physical.size.w != 0 && physical.size.h != 0 {
        instance.obj.physical_size(physical.size.w, physical.size.h);
    }

    // send modes
    let output_modes = output.modes();
    
    // remove old modes
    instance.modes.retain_mut(|m| {
        if !output_modes.contains(m.data::<Mode>().unwrap()) {
            m.finished();
            false
        } else {
            true
        }
    });

    // add new modes
    for output_mode in output_modes.into_iter() {
        if let Some(mode) = if let Some(wlr_mode) = instance
            .modes
            .iter()
            .find(|mode| *mode.data::<Mode>().unwrap() == output_mode)
        {
            Some(wlr_mode)
        } else if let Ok(client) = dh.get_client(instance.obj.id()) {
            // create the mode
            if let Ok(mode) = client.create_resource::<ZwlrOutputModeV1, _, D>(
                dh,
                instance.obj.version(),
                output_mode,
            ) {
                instance.obj.mode(&mode);
                mode.size(output_mode.size.w, output_mode.size.h);
                mode.refresh(output_mode.refresh);
                if output.preferred_mode().map(|p| p == output_mode).unwrap_or(false) {
                    mode.preferred();
                }
                instance.modes.push(mode);
                instance.modes.last()
            } else {
                None
            }
        } else {
            None
        } {
            // mark current mode
            if output.current_mode().map(|c| c == output_mode).unwrap_or(false) {
                instance.obj.current_mode(mode);
            }
        }
    }

    // send current state
    instance.obj.enabled(1);
    let point = output.current_location();
    instance.obj.position(point.x, point.y);
    instance.obj.transform(output.current_transform().into());
    
    let scale = output.current_scale().fractional_scale();
    instance.obj.scale(scale);

    // send make/model/serial if supported
    if instance.obj.version() >= zwlr_output_head_v1::EVT_MAKE_SINCE {
        if physical.make != "Unknown" {
            instance.obj.make(physical.make.clone());
        }
        if physical.model != "Unknown" {
            instance.obj.model(physical.model);
        }
        if physical.serial_number != "Unknown" {
            instance.obj.serial_number(physical.serial_number);
        }
    }

    // adaptive sync support (version 4)
    if instance.obj.version() >= zwlr_output_head_v1::EVT_ADAPTIVE_SYNC_SINCE {
        // for now, we don't support adaptive sync
        instance.obj.adaptive_sync(zwlr_output_head_v1::AdaptiveSyncState::Disabled);
    }
}

// import State type for the handlers
use crate::State;

// global dispatch for the output manager
impl GlobalDispatch<ZwlrOutputManagerV1, OutputMngrGlobalData, State> for OutputConfigurationState {
    fn bind(
        state: &mut State,
        dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _global_data: &OutputMngrGlobalData,
        data_init: &mut DataInit<'_, State>,
    ) {
        let mut instance = OutputMngrInstance {
            obj: data_init.init(resource, ()),
            heads: Vec::new(),
        };

        let mngr_state = state.output_configuration_state();
        for output in &mngr_state.outputs {
            send_head_to_client::<State>(dh, &mut instance, output);
        }
        instance.obj.done(mngr_state.serial_counter);
        mngr_state.instances.push(instance);
    }

    fn can_view(client: Client, global_data: &OutputMngrGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

// dispatch for the output manager
impl Dispatch<ZwlrOutputManagerV1, (), State> for OutputConfigurationState {
    fn request(
        state: &mut State,
        _client: &Client,
        obj: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                let conf = data_init.init(
                    id,
                    Mutex::new(PendingConfigurationInner {
                        serial,
                        used: false,
                        heads: Vec::new(),
                    }),
                );

                let state = state.output_configuration_state();
                if serial != state.serial_counter {
                    conf.cancelled();
                }
            }
            zwlr_output_manager_v1::Request::Stop => {
                let state = state.output_configuration_state();
                state.instances.retain(|instance| instance.obj != *obj);
                obj.finished();
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut State,
        _client: ClientId,
        obj: &ZwlrOutputManagerV1,
        _data: &(),
    ) {
        let state = state.output_configuration_state();
        state.instances.retain(|instance| instance.obj != *obj);
    }
}

// dispatch for output heads
impl Dispatch<ZwlrOutputHeadV1, WeakOutput, State> for OutputConfigurationState {
    fn request(
        state: &mut State,
        _client: &Client,
        obj: &ZwlrOutputHeadV1,
        request: zwlr_output_head_v1::Request,
        _data: &WeakOutput,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwlr_output_head_v1::Request::Release => {
                for instance in &mut state.output_configuration_state().instances {
                    instance.heads.retain(|h| &h.obj != obj);
                }
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut State, _client: ClientId, obj: &ZwlrOutputHeadV1, _data: &WeakOutput) {
        for instance in &mut state.output_configuration_state().instances {
            instance.heads.retain(|h| &h.obj != obj);
        }
    }
}

// dispatch for output modes
impl Dispatch<ZwlrOutputModeV1, Mode, State> for OutputConfigurationState {
    fn request(
        state: &mut State,
        _client: &Client,
        obj: &ZwlrOutputModeV1,
        request: zwlr_output_mode_v1::Request,
        _data: &Mode,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwlr_output_mode_v1::Request::Release => {
                let state = state.output_configuration_state();
                for instance in &mut state.instances {
                    for head in &mut instance.heads {
                        head.modes.retain(|mode| mode != obj)
                    }
                }
            }
            _ => {}
        }
    }
}

// dispatch for output configuration
impl Dispatch<ZwlrOutputConfigurationV1, PendingConfiguration, State> for OutputConfigurationState {
    fn request(
        state: &mut State,
        _client: &Client,
        obj: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &PendingConfiguration,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwlr_output_configuration_v1::Request::EnableHead { id, head } => {
                let mut pending = data.lock().unwrap();
                if pending.heads.iter().any(|(h, _)| *h == head) {
                    obj.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        format!("{:?} was already configured", head),
                    );
                    return;
                }

                let conf_head = data_init.init(id, Mutex::new(PendingOutputConfigurationInner::default()));
                pending.heads.push((head, Some(conf_head)));
            }
            zwlr_output_configuration_v1::Request::DisableHead { head } => {
                let mut pending = data.lock().unwrap();
                if pending.heads.iter().any(|(h, _)| *h == head) {
                    obj.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        format!("{:?} was already configured", head),
                    );
                    return;
                }

                pending.heads.push((head, None));
            }
            x @ zwlr_output_configuration_v1::Request::Apply
            | x @ zwlr_output_configuration_v1::Request::Test => {
                let mut pending = data.lock().unwrap();

                if pending.used {
                    return obj.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "Configuration object was used already".to_string(),
                    );
                }
                pending.used = true;

                let inner = state.output_configuration_state();
                if pending.serial != inner.serial_counter {
                    obj.cancelled();
                    return;
                }

                // build final configuration
                let final_conf = match pending
                    .heads
                    .iter_mut()
                    .map(|(head, conf)| {
                        let output = inner
                            .instances
                            .iter()
                            .find_map(|instance| instance.heads.iter().find(|h| h.obj == *head))
                            .map(|i| i.output.clone())
                            .ok_or(zwlr_output_configuration_head_v1::Error::InvalidMode)?;

                        match conf {
                            Some(head) => {
                                let head_data = head.data::<PendingOutputConfiguration>().unwrap();
                                let mut config = head_data.lock().unwrap();
                                (&mut *config)
                                    .try_into()
                                    .map(|c| (output, c))
                            }
                            None => Ok((output, OutputConfiguration::Disabled)),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(conf) => conf,
                    Err(code) => {
                        return obj.post_error(code, "Incomplete configuration".to_string());
                    }
                };

                let result = if matches!(x, zwlr_output_configuration_v1::Request::Test) {
                    state.test_configuration(final_conf)
                } else {
                    state.apply_configuration(final_conf)
                };

                if result {
                    obj.succeeded();
                } else {
                    obj.failed();
                }
            }
            zwlr_output_configuration_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// dispatch for configuration heads
impl Dispatch<ZwlrOutputConfigurationHeadV1, PendingOutputConfiguration, State> for OutputConfigurationState {
    fn request(
        _state: &mut State,
        _client: &Client,
        obj: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &PendingOutputConfiguration,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwlr_output_configuration_head_v1::Request::SetMode { mode } => {
                let mut pending = data.lock().unwrap();
                if pending.mode.is_some() {
                    obj.post_error(
                        zwlr_output_configuration_head_v1::Error::AlreadySet,
                        format!("{:?} already had a mode configured", obj),
                    );
                    return;
                }
                let mode_data = mode.data::<Mode>().cloned()
                    .ok_or_else(|| {
                        obj.post_error(
                            zwlr_output_configuration_head_v1::Error::InvalidMode,
                            "Invalid mode".to_string(),
                        );
                    });
                if let Ok(mode) = mode_data {
                    pending.mode = Some(ModeConfiguration::Mode(mode));
                }
            }
            zwlr_output_configuration_head_v1::Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                let mut pending = data.lock().unwrap();
                if pending.mode.is_some() {
                    obj.post_error(
                        zwlr_output_configuration_head_v1::Error::AlreadySet,
                        format!("{:?} already had a mode configured", obj),
                    );
                    return;
                }
                pending.mode = Some(ModeConfiguration::Custom {
                    size: Size::from((width, height)),
                    refresh: if refresh == 0 { None } else { Some(refresh) },
                });
            }
            zwlr_output_configuration_head_v1::Request::SetPosition { x, y } => {
                let mut pending = data.lock().unwrap();
                if pending.position.is_some() {
                    obj.post_error(
                        zwlr_output_configuration_head_v1::Error::AlreadySet,
                        format!("{:?} already had a position configured", obj),
                    );
                    return;
                }
                pending.position = Some(Point::from((x, y)));
            }
            zwlr_output_configuration_head_v1::Request::SetScale { scale } => {
                let mut pending = data.lock().unwrap();
                if pending.scale.is_some() {
                    obj.post_error(
                        zwlr_output_configuration_head_v1::Error::AlreadySet,
                        format!("{:?} already had a scale configured", obj),
                    );
                    return;
                }
                pending.scale = Some(scale);
            }
            zwlr_output_configuration_head_v1::Request::SetTransform { transform } => {
                let mut pending = data.lock().unwrap();
                if pending.transform.is_some() {
                    obj.post_error(
                        zwlr_output_configuration_head_v1::Error::AlreadySet,
                        format!("{:?} already had a transform configured", obj),
                    );
                    return;
                }
                pending.transform = Some(match transform.into_result() {
                    Ok(transform) => transform.into(),
                    Err(err) => {
                        obj.post_error(
                            zwlr_output_configuration_head_v1::Error::InvalidTransform,
                            format!("Invalid transform: {:?}", err),
                        );
                        return;
                    }
                });
            }
            zwlr_output_configuration_head_v1::Request::SetAdaptiveSync { state } => {
                let mut pending = data.lock().unwrap();
                if pending.adaptive_sync.is_some() {
                    obj.post_error(
                        zwlr_output_configuration_head_v1::Error::AlreadySet,
                        format!("{:?} already had adaptive sync configured", obj),
                    );
                    return;
                }
                pending.adaptive_sync = Some(match state.into_result() {
                    Ok(state) => match state {
                        zwlr_output_head_v1::AdaptiveSyncState::Enabled => true,
                        _ => false,
                    },
                    Err(err) => {
                        obj.post_error(
                            zwlr_output_configuration_head_v1::Error::InvalidAdaptiveSyncState,
                            format!("Invalid adaptive sync value: {:?}", err),
                        );
                        return;
                    }
                });
            }
            _ => {}
        }
    }
}

// macro to delegate the protocol implementation
#[macro_export]
macro_rules! delegate_output_configuration {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_manager_v1::ZwlrOutputManagerV1: $crate::wayland::output_configuration::OutputMngrGlobalData
        ] => $crate::wayland::output_configuration::OutputConfigurationState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_manager_v1::ZwlrOutputManagerV1: ()
        ] => $crate::wayland::output_configuration::OutputConfigurationState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_head_v1::ZwlrOutputHeadV1: smithay::output::WeakOutput
        ] => $crate::wayland::output_configuration::OutputConfigurationState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_mode_v1::ZwlrOutputModeV1: smithay::output::Mode
        ] => $crate::wayland::output_configuration::OutputConfigurationState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_configuration_v1::ZwlrOutputConfigurationV1: $crate::wayland::output_configuration::PendingConfiguration
        ] => $crate::wayland::output_configuration::OutputConfigurationState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1: $crate::wayland::output_configuration::PendingOutputConfiguration
        ] => $crate::wayland::output_configuration::OutputConfigurationState);
    };
}