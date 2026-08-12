//! mc-mgmt as a [`Console`].
//!
//! The whole of this plugin's contribution to the lifecycle. The policy — who
//! is warned, in what order, what happens when the count cannot be obtained,
//! how long the JVM gets to flush — lives in `mc_console::hooks` and is shared
//! with every other console. All that is left here is the mapping from those
//! six operations onto protocol methods.

use mc_common::error::Result;
use mc_common::paths::Paths;
use mc_common::properties::Properties;
use mc_console::{Console, PlayerCount};

use crate::rpc::Client;
use crate::transport::WebSocketTransport;
use crate::{endpoint, methods};

pub struct MgmtConsole {
    client: Client<WebSocketTransport>,
}

impl MgmtConsole {
    /// Connect using whatever `server.properties` currently says.
    ///
    /// Read at the point of use rather than cached: the JVM rewrites that file,
    /// and the secret in particular can change between one invocation and the
    /// next.
    pub fn connect(paths: &Paths) -> Result<Self> {
        let props = Properties::load(&paths.server_properties());
        let endpoint = endpoint::require(&props)?;
        let transport = WebSocketTransport::connect(&endpoint)?;
        Ok(Self {
            client: Client::new(transport),
        })
    }

    /// Can this console talk to the server right now?
    ///
    /// A full connect and handshake, not a look at the config: the properties
    /// describe what the *next* start will listen on, and a server still
    /// running from before the setting changed would elect a console that then
    /// cannot deliver the countdown it promised.
    pub fn usable(paths: &Paths) -> bool {
        Self::connect(paths).is_ok()
    }

    pub fn client(&mut self) -> &mut Client<WebSocketTransport> {
        &mut self.client
    }
}

impl Console for MgmtConsole {
    fn say(&mut self, message: &str) -> Result<()> {
        methods::say(&mut self.client, message)
    }

    fn player_count(&mut self) -> PlayerCount {
        methods::player_count(&mut self.client)
    }

    fn save_now(&mut self) -> Result<()> {
        // `flush: true` waits for the write rather than scheduling it, which is
        // the whole point of calling it before an archive reads the directory.
        methods::save(&mut self.client, true)
    }

    fn set_autosave(&mut self, enabled: bool) -> Result<()> {
        methods::set_autosave(&mut self.client, enabled)
    }

    fn stop(&mut self) -> Result<()> {
        methods::stop(&mut self.client)
    }
}
