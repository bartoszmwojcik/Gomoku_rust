//! The MCP endpoint for LLM agents: `POST /api/mcp`, stateless Streamable HTTP served by `rmcp`, with our own layer in
//! front (origin, per-address limit, JSON-RPC shaped refusals) and the four tools behind it.

pub mod input;
mod server;
pub mod tools;
pub mod view;

pub use server::{mcp_router, tool_list};
