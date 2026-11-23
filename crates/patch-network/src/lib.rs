//! Patch Network: Patch's sound to other machines on the local network,
//! and theirs into Patch.
//!
//! * **Send** the program, AUX 1 or AUX 2 to every machine on the network
//!   (RTP multicast, announced with SAP) or to one machine (RTP or VBAN).
//!   24-bit, 48 kHz, stereo: AES67's L24, which PipeWire, other Patches and
//!   AES67 receivers take.
//! * **Receive** a stream another machine announces: it plays into Patch as
//!   an application, and the Mixer treats it as one. Or take every VBAN
//!   stream sent here.
//!
//! The network work is PipeWire's (its RTP, SAP and VBAN modules, with
//! their clock recovery and jitter buffers); Patch chooses what goes where
//! and keeps the links running (see `link`). Transport, clock and recovery
//! are in `docs/network.md`.

pub mod conf;
pub mod config;
pub mod discover;
pub mod firewall;
pub mod link;
pub mod net;
pub mod ops;
pub mod sdp;
