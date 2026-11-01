//! canopy's MCP server, the part that is neither a socket nor a window.
//!
//! An LLM client (Claude Code, Claude Desktop) speaks the Model Context
//! Protocol to the `canopy-mcp` bridge, which forwards tool calls to the
//! running app. Both ends share what lives here: [`protocol`], the
//! JSON-RPC messages; [`tools`], every tool's name, description, schema
//! and body; [`units`], how the tools read and write beats, pitches and
//! parameters; and [`host::Host`], what the tools need from the app (the
//! project, undo, the transport, renders), which the app implements on
//! its UI thread and tests implement with a fake. [`connection`] is how
//! the bridge finds the app and proves it may talk to it.

pub mod connection;
pub mod host;
pub mod protocol;
pub mod tools;
pub mod units;

#[cfg(test)]
pub(crate) mod testing;
