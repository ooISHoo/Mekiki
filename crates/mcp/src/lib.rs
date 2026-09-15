//! Mekiki as an MCP server.
//!
//! Exposes the engine over the Model Context Protocol so an AI agent can
//! explore a desktop, write a `.rhai` script and verify it. See
//! [`server`] for what the tools are and why the security posture is what it
//! is, and `docs/architecture/mcp.md` for the design this follows.
//!
//! Split out as its own crate so that tokio and rmcp stay here: nothing the IDE
//! or the CLI builds pulls in an async runtime.

pub mod desktop;
pub mod engine;
pub mod hotkey;
pub mod imaging;
pub mod notes;
pub mod server;
pub mod tray;

pub use engine::EngineHandle;
pub use server::{Limits, MekikiServer};
