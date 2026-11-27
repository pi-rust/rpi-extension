use crate::{lifecycle, PlanState};
use rpi_plugin_sdk::{PluginApi, RuntimeActionId, StbString, StbStringRef};
use serde_json::Value;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

pub(crate) const SLOTS: usize = 128;
static REGISTRATIONS: Mutex<[Option<Arc<Registration>>; SLOTS]> =
    Mutex::new([const { None }; SLOTS]);
static LEGACY: Mutex<Option<Arc<Registration>>> = Mutex::new(None);

pub(crate) struct Registration {
    api: PluginApi,
    pub(crate) state: Mutex<PlanState>,
    stopping: std::sync::atomic::AtomicBool,
}

impl Registration {
    pub(crate) fn new(api: PluginApi) -> Self {
        Self {
            api,
            state: Mutex::default(),
            stopping: false.into(),
        }
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn binding(self: &Arc<Self>) -> Binding {
        Binding {
            registration: self.clone(),
            runtime: self.api,
        }
    }
}

pub(crate) struct Binding {
    pub(crate) registration: Arc<Registration>,
    runtime: PluginApi,
}

impl Binding {
    pub(crate) fn call(&self, id: RuntimeActionId, args: Value) -> Result<Value, String> {
        if self.registration.is_stopping() {
            return Err("plan mode registration is shutting down".into());
        }
        let runtime = &self.runtime;
        let input = args.to_string();
        let mut output = StbString::empty();
        let rc = (runtime.runtime_action)(
            id as u32,
            StbStringRef::from_str(&input),
            &mut output,
            runtime.user_data,
        );
        let text = output.to_string_lossy();
        (runtime.free_string)(output);
        if rc != 0 {
            return Err(if text.is_empty() {
                format!("runtime action {id:?} failed")
            } else {
                text
            });
        }
        if text.trim().is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_str(&text).map_err(|e| format!("invalid runtime response: {e}"))
        }
    }
}

fn registrations() -> std::sync::MutexGuard<'static, [Option<Arc<Registration>>; SLOTS]> {
    REGISTRATIONS.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn install(api: PluginApi, cleanup: Option<lifecycle::Register>) -> Result<usize, i32> {
    let registration = Arc::new(Registration::new(api));
    let Some(cleanup) = cleanup else {
        *LEGACY.lock().unwrap_or_else(|p| p.into_inner()) = Some(registration);
        return Ok(SLOTS);
    };
    let slot = {
        let mut registrations = registrations();
        if api.user_data.is_null()
            || registrations
                .iter()
                .flatten()
                .any(|item| item.api.user_data == api.user_data)
        {
            return Err(1);
        }
        let slot = registrations.iter().position(Option::is_none).ok_or(1)?;
        registrations[slot] = Some(registration);
        slot
    };
    let rc = cleanup(begin, finish, api.user_data);
    if rc != 0 {
        registrations()[slot] = None;
        return Err(rc);
    }
    Ok(slot)
}

fn lookup(owner: *mut c_void) -> Option<Arc<Registration>> {
    registrations()
        .iter()
        .flatten()
        .find(|item| item.api.user_data == owner)
        .cloned()
}

fn legacy() -> Option<Binding> {
    Some(
        LEGACY
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()?
            .binding(),
    )
}

pub(crate) fn callback(owner: *mut c_void) -> Option<Binding> {
    let mut binding = match lookup(owner) {
        Some(registration) => registration.binding(),
        None => legacy()?,
    };
    if binding.registration.is_stopping() {
        return None;
    }
    if !owner.is_null() {
        binding.runtime.user_data = owner;
    }
    Some(binding)
}

pub(crate) fn tool<const SLOT: usize>(params: &Value) -> Result<Binding, String> {
    if SLOT == SLOTS {
        return legacy().ok_or_else(|| "plan mode runtime is unavailable".into());
    }
    let registration = registrations()[SLOT]
        .clone()
        .ok_or("plan mode registration is unavailable")?;
    let mut binding = registration.binding();
    let handle = params["__rpi"]["runtimeHandle"]
        .as_u64()
        .and_then(|handle| usize::try_from(handle).ok())
        .filter(|handle| *handle != 0)
        .ok_or("plan mode requires a host tool invocation runtimeHandle")?;
    binding.runtime.user_data = handle as *mut c_void;
    Ok(binding)
}

extern "C" fn begin(owner: *mut c_void) {
    if let Some(registration) = lookup(owner) {
        registration
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

extern "C" fn finish(owner: *mut c_void) -> i32 {
    let mut registrations = registrations();
    if let Some(slot) = registrations.iter_mut().find(|slot| {
        slot.as_ref()
            .is_some_and(|item| item.api.user_data == owner)
    }) {
        *slot = None;
    }
    0
}
