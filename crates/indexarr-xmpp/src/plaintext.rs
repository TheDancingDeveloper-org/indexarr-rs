//! Plaintext `ServerConnector` for tokio-xmpp.
//!
//! `tokio-xmpp` 4.0's stock `ServerConfig` (the `starttls` module) is
//! hard-coded to require STARTTLS — if the server doesn't advertise it,
//! the connection fails. That's a reasonable default for the public
//! internet, but the Indexarr discovery server deliberately accepts
//! plaintext clients on its DNS-only public port. The generated password is
//! derived deterministically from the public contributor id, so it is not a
//! user secret and confidentiality of the SASL exchange is not load-bearing.
//!
//! This connector just opens a TCP stream and starts the XMPP session
//! without any TLS upgrade. It is used whenever `INDEXARR_XMPP_SERVER` is set,
//! including the public `conference.indexarr.net:5222` default.

use std::io;

use tokio::net::TcpStream;
use tokio_xmpp::Error as XmppError;
use tokio_xmpp::connect::{ServerConnector, ServerConnectorError};
use tokio_xmpp::parsers::jid::Jid;
use tokio_xmpp::xmpp_stream::XMPPStream;

#[derive(Debug, Clone)]
pub struct PlaintextConnector {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum PlaintextError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("xmpp: {0}")]
    Xmpp(#[from] XmppError),
}

impl ServerConnectorError for PlaintextError {}

impl ServerConnector for PlaintextConnector {
    type Stream = TcpStream;
    type Error = PlaintextError;

    async fn connect(&self, jid: &Jid, ns: &str) -> Result<XMPPStream<Self::Stream>, Self::Error> {
        let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
        let stream = XMPPStream::start(tcp, jid.clone(), ns.to_owned()).await?;
        Ok(stream)
    }
}
