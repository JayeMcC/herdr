use super::*;

/// Internal events for the client event loop.
pub(super) enum ClientLoopEvent {
    #[cfg(unix)]
    /// Raw stdin bytes and the instant they were read.
    StdinInput(Vec<u8>, std::time::Instant),
    #[cfg(unix)]
    PixelMouse(
        Vec<u8>,
        crate::input::mouse::HostGeometry,
        std::time::Instant,
    ),
    #[cfg(unix)]
    DirectGraphicsResponse(direct_graphics::Response),
    #[cfg(windows)]
    StdinEvents(Vec<crate::protocol::ClientInputEvent>),
    Resize(u16, u16, u32, u32, bool),
    TerminalUnavailable(io::Error),
    ServerMessage {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
        message: Box<ServerMessage>,
    },
    ServerDisconnected {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
    },
    EndpointSupervisor(endpoint::EndpointSupervisorEvent),
    EndpointCatalog(Result<Vec<endpoint::SavedSshEndpoint>, String>),
    ActivateEndpoint {
        endpoint_id: endpoint::ClientEndpointId,
        target: Option<shell::ClientEndpointFocusTarget>,
        /// A superseded handoff deliberately starts a fresh target-on epoch even when source and
        /// latest target have the same identity after restoration.
        force: bool,
    },
    Timer,
}
