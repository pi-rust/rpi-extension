use super::*;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::LazyLock;

static OWNERS: LazyLock<Mutex<HashMap<usize, Arc<RuntimeContext>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
// Hosts without loader hooks use an isolated image and may pass a null or
// invocation-specific command handle. The two registration modes never mix.
static LEGACY: Mutex<Option<Arc<RuntimeContext>>> = Mutex::new(None);
static INSTALL: Mutex<()> = Mutex::new(());

pub(super) fn register(api: &PluginApi) -> i32 {
    let owner = Arc::new(RuntimeContext::new(*api));
    owner.auto_tts_enabled.store(
        std::env::var("RPI_VOICE_AUTO_TTS").is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "on" | "1" | "true" | "yes"
            )
        }),
        Ordering::Relaxed,
    );
    let key = api.user_data as usize;
    {
        let _install = INSTALL.lock().unwrap();
        let mut legacy = LEGACY.lock().unwrap();
        let mut owners = OWNERS.lock().unwrap();
        if legacy.is_some() || owners.contains_key(&key) {
            return 1;
        }
        if api.register_plugin_lifecycle.is_some() {
            if key == 0 {
                return 1;
            }
            owners.insert(key, owner.clone());
        } else {
            if !owners.is_empty() {
                return 1;
            }
            *legacy = Some(owner.clone());
        }
    }
    if let Some(hooks) = api.register_plugin_lifecycle {
        let rc = hooks(begin, finish, api.user_data);
        if rc != 0 {
            OWNERS.lock().unwrap().remove(&key);
            return rc;
        }
    }
    let rc = register_callbacks(api);
    if rc == 0 {
        owner.ready.store(true, Ordering::Release);
    } else {
        // Callbacks registered ahead of the failure cannot admit work. The
        // loader owns cleanup once the lifecycle registrar has succeeded.
        owner.begin();
        if api.register_plugin_lifecycle.is_none() {
            LEGACY.lock().unwrap().take();
        }
    }
    rc
}

fn lookup(data: *mut c_void) -> Option<Arc<RuntimeContext>> {
    OWNERS.lock().unwrap().get(&(data as usize)).cloned()
}

pub(super) fn callback(data: *mut c_void) -> Option<Arc<RuntimeContext>> {
    lookup(data)
        .or_else(|| LEGACY.lock().unwrap().clone())
        .filter(|owner| owner.ready.load(Ordering::Acquire) && !owner.is_stopping())
}

pub(super) fn session_shutdown(owner: &Arc<RuntimeContext>) {
    owner.begin();
    if owner.api.register_plugin_lifecycle.is_none() {
        owner.finish();
        LEGACY.lock().unwrap().take();
    }
}

extern "C" fn begin(data: *mut c_void) {
    rpi_plugin_sdk::guard_or((), || {
        if let Some(owner) = lookup(data) {
            owner.begin();
        }
    });
}

extern "C" fn finish(data: *mut c_void) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        let Some(owner) = lookup(data) else {
            return 0;
        };
        owner.finish();
        // CPAL's CoreAudio disconnect listener can keep its Stream alive.
        // Also retain Windows images after an audio open until WASAPI teardown
        // is qualified. The loader pins the context on a nonzero result.
        if !cfg!(all(target_os = "linux", target_env = "gnu"))
            && owner.native_audio_used.load(Ordering::Acquire)
        {
            return 1;
        }
        OWNERS.lock().unwrap().remove(&(data as usize));
        0
    })
}

#[cfg(test)]
pub(super) fn test_api(data: usize) -> PluginApi {
    extern "C" fn noop(_: u32, _: StbStringRef, _: *mut StbString, _: *mut c_void) -> i32 {
        0
    }
    PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        user_data: data as *mut c_void,
        runtime_action: noop,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        register_tool: None,
        register_command: None,
        register_shortcut: None,
        register_flag: None,
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: None,
        register_resources_discover: None,
        dispatch_event: None,
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
        register_streaming_provider: None,
        register_tool_with_wake: None,
        register_plugin_lifecycle: None,
        register_tool_prompt: None,
        register_oauth_provider_cancellable: None,
        register_tool_result: None,
        register_autocomplete_provider: None,
        register_command_completions: None,
        register_argument_suggester: None,
    }
}

#[cfg(test)]
pub(super) fn test_runtime() -> Arc<RuntimeContext> {
    static NEXT: AtomicUsize = AtomicUsize::new(10_000);
    Arc::new(RuntimeContext::new(test_api(
        NEXT.fetch_add(1, Ordering::Relaxed),
    )))
}

#[cfg(test)]
pub(super) fn with_test_callback<T>(owner: &Arc<RuntimeContext>, f: impl FnOnce() -> T) -> T {
    let key = owner.api.user_data as usize;
    owner.ready.store(true, Ordering::Release);
    OWNERS.lock().unwrap().insert(key, owner.clone());
    let result = f();
    OWNERS.lock().unwrap().remove(&key);
    result
}
