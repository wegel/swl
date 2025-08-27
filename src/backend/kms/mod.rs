// SPDX-License-Identifier: GPL-3.0-only

use crate::state::{BackendData, State};
use anyhow::{Context, Result};
use smithay::{
    backend::{
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
    },
    reexports::{
        calloop::{EventLoop, LoopHandle},
        input::Libinput,
        wayland_server::DisplayHandle,
    },
};
use tracing::info;

/// KMS backend state
#[derive(Debug)]
pub struct KmsState {
    pub session: LibSeatSession,
    pub libinput: Libinput,
}

pub fn init_backend(
    _dh: &DisplayHandle,
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
) -> Result<()> {
    info!("Initializing KMS backend");
    
    // establish session
    let (session, notifier) = LibSeatSession::new()
        .context("Failed to acquire session")?;
    
    info!("Session acquired on seat: {}", session.seat());
    
    // setup input
    let libinput_context = init_libinput(&session, &event_loop.handle())
        .context("Failed to initialize libinput backend")?;
    
    // handle session events
    event_loop
        .handle()
        .insert_source(notifier, move |event, &mut (), state| match event {
            SessionEvent::ActivateSession => {
                info!("Session activated");
                state.session_active(true);
            }
            SessionEvent::PauseSession => {
                info!("Session paused");
                state.session_active(false);
            }
        })
        .map_err(|err| err.error)
        .context("Failed to initialize session event source")?;
    
    // finish backend initialization
    state.backend = BackendData::Kms(KmsState {
        session,
        libinput: libinput_context,
    });
    
    Ok(())
}

fn init_libinput(
    session: &LibSeatSession,
    evlh: &LoopHandle<'static, State>,
) -> Result<Libinput> {
    let mut libinput_context = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
        session.clone().into()
    );
    
    libinput_context
        .udev_assign_seat(&session.seat())
        .map_err(|_| anyhow::anyhow!("Failed to assign seat to libinput"))?;
    
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());
    
    evlh.insert_source(libinput_backend, move |mut event, _, state| {
        // for now, just log input events
        match &mut event {
            InputEvent::DeviceAdded { device } => {
                info!("Input device added: {}", device.name());
            }
            InputEvent::DeviceRemoved { device } => {
                info!("Input device removed: {}", device.name());
            }
            _ => {
                // we'll handle actual input in a later phase
            }
        }
        
        state.process_input_event(event);
    })
    .map_err(|err| err.error)
    .context("Failed to initialize libinput event source")?;
    
    Ok(libinput_context)
}