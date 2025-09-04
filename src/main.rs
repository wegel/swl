// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use smithay::{
    reexports::{
        calloop::{self, EventLoop, Interest, Mode, PostAction},
        wayland_server::{Display, DisplayHandle},
    },
    wayland::socket::ListeningSocketSource,
};
use tracing::{error, info};

mod backend;
mod environment;
mod input;
mod shell;
mod startup;
mod state;
mod utils;
mod wayland;
use state::State;

fn main() {
    if let Err(err) = main_inner() {
        error!("Error occurred in main(): {}", err);
        std::process::exit(1);
    }
}

fn main_inner() -> Result<()> {
    // setup logger
    init_logger()?;
    info!("swl starting up!");
    tracing::debug!("Debug logging is working!");

    // init event loop
    let mut event_loop = EventLoop::try_new()
        .context("Failed to initialize event loop")?;
    
    // init wayland display
    let (display_handle, socket) = init_wayland_display(&mut event_loop)?;
    
    // init state
    let mut state = State::new(
        display_handle.clone(),
        socket,
        event_loop.handle(),
        event_loop.get_signal(),
    );
    
    // init backend
    backend::init_backend(&display_handle, &mut event_loop, &mut state)?;

    // update environment variables for systemd and D-Bus
    environment::update_environment(&state.socket_name);

    // run startup program if configured
    startup::run_startup_program();

    info!("Starting event loop");
    
    // run the event loop
    event_loop.run(None, &mut state, |state| {
        // shall we shut down?
        if state.should_stop {
            info!("Shutting down");
            state.loop_signal.stop();
            state.loop_signal.wakeup();
            return;
        }

        // send out pending events
        let _ = state.display_handle.flush_clients();
        
        // refresh focus if needed (deferred from layer_destroyed and other events)
        if state.needs_focus_refresh {
            state.needs_focus_refresh = false;
            state.refresh_focus();
        }
    })?;

    info!("Event loop exited");
    Ok(())
}

fn init_logger() -> Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};
    
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("swl=info"));
    
    fmt()
        .with_env_filter(filter)
        .init();
        
    Ok(())
}

fn init_wayland_display(
    event_loop: &mut EventLoop<'static, State>,
) -> Result<(DisplayHandle, String)> {
    // create the wayland display
    let display = Display::<State>::new()
        .context("Failed to create wayland display")?;
    let display_handle = display.handle();
    
    // create a listening socket
    let listening_socket = ListeningSocketSource::new_auto()
        .context("Failed to create listening socket")?;
    
    let socket_name = listening_socket
        .socket_name()
        .to_string_lossy()
        .into_owned();
    
    info!("Listening on wayland socket: {}", socket_name);
    
    event_loop
        .handle()
        .insert_source(listening_socket, |client_stream, _, state| {
            // accept new wayland clients
            match state
                .display_handle
                .insert_client(
                    client_stream, 
                    std::sync::Arc::new(crate::wayland::handlers::ClientState::new())
                ) {
                Ok(client) => {
                    tracing::trace!("New Wayland client connected: {:?}", client.id());
                }
                Err(err) => {
                    tracing::error!("Failed to insert client: {}", err);
                }
            }
        })
        .context("Failed to init wayland socket source")?;
    
    // insert the display as an event source
    event_loop
        .handle()
        .insert_source(
            calloop::generic::Generic::new(display, Interest::READ, Mode::Level),
            move |_, display, state: &mut State| {
                // dispatch pending messages from clients
                // SAFETY: We don't drop the display
                match unsafe { display.get_mut().dispatch_clients(state) } {
                    Ok(_) => Ok(PostAction::Continue),
                    Err(e) => {
                        tracing::error!("Failed to dispatch clients: {}", e);
                        state.should_stop = true;
                        Ok(PostAction::Continue)
                    }
                }
            }
        )
        .context("Failed to init display event source")?;
    
    Ok((display_handle, socket_name))
}