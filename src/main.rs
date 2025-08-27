// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use smithay::{
    reexports::{
        calloop::EventLoop,
        wayland_server::Display,
    },
    wayland::socket::ListeningSocketSource,
};
use tracing::{error, info};

mod backend;
mod input;
mod state;
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

    // init event loop
    let mut event_loop = EventLoop::try_new()
        .context("Failed to initialize event loop")?;
    
    // init wayland display
    let (display, socket) = init_wayland_display(&mut event_loop)?;
    
    // init state
    let mut state = State::new(
        &display,
        socket,
        event_loop.handle(),
        event_loop.get_signal(),
    );
    
    // init backend
    backend::init_backend(&display.handle(), &mut event_loop, &mut state)?;

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
) -> Result<(Display<State>, String)> {
    // create the wayland display
    let display = Display::<State>::new()
        .context("Failed to create wayland display")?;
    
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
            let _ = state
                .display_handle
                .insert_client(
                    client_stream, 
                    std::sync::Arc::new(crate::wayland::handlers::ClientState::new())
                );
        })
        .context("Failed to init wayland socket source")?;
    
    Ok((display, socket_name))
}