//! Shared ABI adapters without exporting a plugin entry per vendor module.
use rpi_plugin_sdk::{provider::ProviderContext, PluginApi, StbStringRef};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock},
};

/// The SDK passes only `&ProviderContext` to a provider factory. Index the
/// vendor state by that context's address so OAuth and streaming can use the
/// same registration without reaching into the SDK's private layout.
pub(crate) struct Registrations<S> {
    active: Mutex<BTreeMap<usize, Registration<S>>>,
    legacy: OnceLock<Arc<S>>,
}

struct Registration<S> {
    context: Arc<ProviderContext>,
    state: Arc<S>,
    stopping: bool,
}

impl<S: Default> Registrations<S> {
    pub(crate) const fn new() -> Self {
        Self {
            active: Mutex::new(BTreeMap::new()),
            legacy: OnceLock::new(),
        }
    }

    pub(crate) fn attach(
        &self,
        api: &PluginApi,
        begin: rpi_plugin_sdk::PluginBeginShutdownFn,
        finish: rpi_plugin_sdk::PluginFinishShutdownFn,
    ) -> Result<*mut c_void, i32> {
        let Some(register) = api.register_plugin_lifecycle else {
            // Legacy hosts without teardown use isolated images. Streaming
            // uses the SDK's per-request runtime on this path.
            return Ok(std::ptr::null_mut());
        };
        let context = Arc::new(ProviderContext::new());
        let data = Arc::as_ptr(&context).cast_mut().cast::<c_void>();
        self.active.lock().unwrap().insert(
            data as usize,
            Registration {
                context,
                state: Arc::new(S::default()),
                stopping: false,
            },
        );
        let rc = register(begin, finish, data);
        if rc != 0 {
            self.remove(data);
            return Err(rc);
        }
        // Once accepted, finish also runs when a later registration step fails.
        Ok(data)
    }

    pub(crate) fn state(&self, data: *const c_void) -> Result<Arc<S>, String> {
        if data.is_null() {
            return Ok(self.legacy.get_or_init(|| Arc::new(S::default())).clone());
        }
        self.active
            .lock()
            .unwrap()
            .get(&(data as usize))
            .filter(|registration| !registration.stopping)
            .map(|registration| registration.state.clone())
            .ok_or_else(|| "provider is shutting down".to_owned())
    }

    pub(crate) fn begin_shutdown(&self, data: *mut c_void) {
        let context = {
            let mut active = self.active.lock().unwrap();
            let Some(registration) = active.get_mut(&(data as usize)) else {
                return;
            };
            registration.stopping = true;
            registration.context.clone()
        };
        context.begin_shutdown();
    }

    pub(crate) fn finish_shutdown(&self, data: *mut c_void) {
        let context = self.active.lock().unwrap()[&(data as usize)]
            .context
            .clone();
        context.finish_shutdown();
        self.remove(data);
    }

    fn remove(&self, data: *mut c_void) {
        let removed = self.active.lock().unwrap().remove(&(data as usize));
        drop(removed);
    }
}

pub(crate) fn register_scoped_stream(
    api: &PluginApi,
    id: &str,
    base: &str,
    style: &str,
    manifest: serde_json::Value,
    (request, stream): (
        rpi_plugin_sdk::ProviderRequestFn,
        rpi_plugin_sdk::ProviderStreamRequestFn,
    ),
    data: *mut c_void,
) -> i32 {
    if let Some(declare) = api.declare {
        let json = serde_json::json!({"providers":[manifest]}).to_string();
        let rc = declare(StbStringRef::from_str(&json));
        if rc != 0 {
            return rc;
        }
    }
    let id = StbStringRef::from_str(id);
    let base = StbStringRef::from_str(base);
    let style = StbStringRef::from_str(style);
    let free = rpi_plugin_sdk::callbacks::free_string;
    if let Some(register) = api.register_streaming_provider {
        register(id, base, style, stream, free, data)
    } else if let Some(register) = api.register_provider {
        register(id, base, style, request, free, data)
    } else {
        1
    }
}

macro_rules! register_provider {
    ($id:expr, $base:expr, $api:expr, $manifest:expr, $factory:path) => {
        rpi_plugin_sdk::provider_callbacks!($factory);

        pub(crate) fn register(api: &rpi_plugin_sdk::PluginApi) -> i32 {
            rpi_plugin_sdk::provider::register_with_context(
                api, $id, $base, $api, $manifest, request, stream,
            )
        }
    };
}
pub(crate) use register_provider;

macro_rules! register_scoped_provider {
    ($id:expr, $base:expr, $api:expr, $manifest:expr, $state:ty, $factory:path) => {
        use rpi_plugin_sdk::provider::ProviderContext;
        use std::ffi::c_void;

        pub(super) static REGISTRATIONS: $crate::registration::Registrations<$state> =
            $crate::registration::Registrations::new();

        fn create_scoped(
            context: &ProviderContext,
            model: &rpi_ai::Model,
            _: &rpi_ai::SimpleStreamOptions,
        ) -> Result<std::sync::Arc<dyn rpi_ai::Provider>, String> {
            $factory(
                context,
                model,
                REGISTRATIONS.state(std::ptr::from_ref(context).cast())?,
            )
        }

        fn create_legacy(
            context: &ProviderContext,
            model: &rpi_ai::Model,
            _: &rpi_ai::SimpleStreamOptions,
        ) -> Result<std::sync::Arc<dyn rpi_ai::Provider>, String> {
            $factory(context, model, REGISTRATIONS.state(std::ptr::null())?)
        }

        fn select_factory(data: *mut c_void) -> rpi_plugin_sdk::provider::ContextFactory {
            if data.is_null() {
                create_legacy
            } else {
                create_scoped
            }
        }
        rpi_plugin_sdk::provider_callbacks!(select_factory = select_factory);

        extern "C" fn begin(data: *mut c_void) {
            rpi_plugin_sdk::guard_or((), || REGISTRATIONS.begin_shutdown(data));
        }

        extern "C" fn finish(data: *mut c_void) -> i32 {
            rpi_plugin_sdk::guard_or(1, || {
                REGISTRATIONS.finish_shutdown(data);
                0
            })
        }

        pub(super) fn register(api: &rpi_plugin_sdk::PluginApi) -> Result<*mut c_void, i32> {
            let data = REGISTRATIONS.attach(api, begin, finish)?;
            let rc = $crate::registration::register_scoped_stream(
                api,
                $id,
                $base,
                $api,
                $manifest,
                (request, stream),
                data,
            );
            if rc == 0 {
                Ok(data)
            } else {
                Err(rc)
            }
        }
    };
}
pub(crate) use register_scoped_provider;
