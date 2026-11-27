//! Loader-owned registration cleanup.
use crate::register;

pub(crate) use rpi_plugin_sdk::RegisterPluginLifecycleFn as Register;

rpi_plugin_sdk::export_shared_plugin!(|api| register(api, api.register_plugin_lifecycle));
