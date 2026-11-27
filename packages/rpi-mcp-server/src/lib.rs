#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod http;
mod plugin;
pub mod protocol;

pub use http::{serve_until as serve_http_until, HttpOptions};
pub use protocol::{run, serve, McpServer, RpcError, ServerOptions};

// Passive registrations share the image. The active stdio owner retains its
// own callbacks and worker until the host finishes that registration.
rpi_plugin_sdk::export_shared_plugin!(plugin::register);
