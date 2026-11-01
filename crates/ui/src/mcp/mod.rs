//! LLM clients in the app: the listener the `canopy-mcp` bridge connects
//! to, and the pump that runs their tool calls on the UI thread.
//!
//! A connection's thread reads calls and sends them down one channel
//! ([`listener`]); every frame, before the session updates, [`pump`] runs
//! what has arrived against [`AppHost`], the app as the tools see it
//! (`mcp::host::Host`), within a few milliseconds. Reads answer at once;
//! edits wait while the user is mid-gesture or a dialog is open, so they
//! never share an undo step with a drag; with View → "Allow LLM
//! connections" off, every call is refused. Renders for the analysis
//! tools run on workers of their own ([`renders`]).

mod host;
mod listener;
mod pump;
pub mod renders;

pub use host::AppHost;
pub use listener::Server;
pub use pump::{FRAME_BUDGET, pump};
