//! Talking to the running server.

use mc_common::error::Result;
use mc_common::paths::Paths;
use mc_common::properties::{self, Properties};

use crate::password;
use crate::protocol::Connection;

/// Open an authenticated connection to the local server.
///
/// The port comes from `server.properties` at the point of use, never from a
/// cached copy: the JVM owns that file and an operator or a modpack may set
/// `rcon.port` by hand.
pub fn connect(paths: &Paths) -> Result<Connection> {
    let props = Properties::load(&paths.server_properties());
    let port = properties::rcon_port(&props);
    let secret = password::read(paths)?;

    let mut connection = Connection::connect("127.0.0.1", port)?;
    connection.authenticate(&secret)?;
    Ok(connection)
}

/// Run one command and return the reply.
pub fn run(paths: &Paths, command: &str) -> Result<String> {
    connect(paths)?.exec(command)
}

/// Broadcast to every player, with no sender prefix.
///
/// Goes through `mc_common::chat::say`, which builds a `tellraw` rather than a
/// `say`: the server renders `say` from an RCON client as `[Rcon] …`.
pub fn announce(connection: &mut Connection, message: &str) -> Result<String> {
    connection.exec(&mc_common::chat::say(message))
}

/// Whether RCON is usable at all: a password exists and the server says it is
/// enabled.
pub fn configured(paths: &Paths) -> bool {
    if !password::exists(paths) {
        return false;
    }
    Properties::load(&paths.server_properties()).get("enable-rcon") == Some("true")
}
