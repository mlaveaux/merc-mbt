//! An interactive, adapter-role peer for debugging `merc-mbt` by hand.
//!
//! `merc-mbt` is a WebSocket *client* speaking the tool role of the mCRL2
//! MBT <-> Adapter protocol; there is normally a real adapter wrapping a
//! system under test on the other end. This crate plays that adapter role
//! itself, as a WebSocket *server* a human drives one command at a time
//! from a REPL, so the tool's behaviour can be probed directly — without a
//! real SUT, and without writing a new Rust test per scenario.

mod command;
mod error;
mod repl;
pub mod testing;

pub use error::AdapterError;

use std::io::BufRead;
use std::io::Write;
use std::net::TcpListener;

/// Binds `bind_addr`, e.g. `127.0.0.1:0` to let the OS pick a free port.
pub fn bind(bind_addr: &str) -> Result<TcpListener, AdapterError> {
    Ok(TcpListener::bind(bind_addr)?)
}

/// Accepts exactly one connection on `listener` and drives the REPL to
/// completion over `input`/`output`.
pub fn serve(listener: TcpListener, input: impl BufRead, output: &mut impl Write) -> Result<(), AdapterError> {
    let local_addr = listener.local_addr()?;
    writeln!(
        output,
        "Listening on {local_addr}. Point merc-mbt at `ws://{local_addr}/` to connect."
    )?;

    let (stream, peer) = listener.accept()?;
    log::info!("Accepted a connection from {peer}.");
    let socket = tungstenite::accept(stream).map_err(error::handshake_error)?;

    repl::run(socket, input, output)
}
