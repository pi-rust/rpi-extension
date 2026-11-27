//! Loader-owned registration cleanup.
use crate::register;

pub(crate) use rpi_plugin_sdk::RegisterPluginLifecycleFn as Register;

#[cfg(unix)]
rpi_plugin_sdk::export_shared_plugin!(|api| register(api, api.register_plugin_lifecycle));

#[cfg(not(unix))]
rpi_plugin_sdk::export_plugin!(|api| register(api, api.register_plugin_lifecycle));
