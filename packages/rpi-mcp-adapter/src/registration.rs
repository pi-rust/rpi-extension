use crate::{kit::Workers, lifecycle, runtime, CoreRequestContext, McpRuntime};
use rpi_plugin_sdk::{PluginApi, StbString, StbStringRef};
use serde_json::{json, Value};
use std::{
    ffi::c_void,
    sync::{Arc, Mutex, MutexGuard},
};

pub(crate) const SLOT_COUNT: usize = 128;

struct Slots {
    entries: [Option<Arc<Registration>>; SLOT_COUNT],
    next_id: usize,
}

static SLOTS: Mutex<Slots> = Mutex::new(Slots {
    entries: [const { None }; SLOT_COUNT],
    next_id: 1,
});

fn slots() -> MutexGuard<'static, Slots> {
    SLOTS.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) struct Registration {
    id: usize,
    api: PluginApi,
    pub(crate) runtime: McpRuntime,
    pub(crate) workers: Workers,
    legacy_scope: Option<Mutex<Option<String>>>,
}

// GetExecutionContext is available on native hosts ahead of the pinned SDK.
const GET_EXECUTION_CONTEXT: u32 = 23;

impl Registration {
    pub(crate) fn call_host(&self, id: u32, args: Value) -> Result<Value, String> {
        let mut out = StbString::empty();
        let input = args.to_string();
        let rc = (self.api.runtime_action)(
            id,
            StbStringRef::from_str(&input),
            &mut out,
            self.api.user_data,
        );
        let text = rpi_plugin_sdk::stb_take!(out, Some(self.api.free_string));
        if rc != 0 {
            return Err(if text.is_empty() {
                "runtime action failed".into()
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

    pub(crate) fn context(&self, params: &Value) -> CoreRequestContext {
        let context = runtime::context(params);
        if let Some(last_scope) = &self.legacy_scope {
            *last_scope.lock().unwrap_or_else(|p| p.into_inner()) = Some(context.scope.clone());
        }
        context
    }

    pub(crate) fn active_scope(&self) -> Option<String> {
        match self.call_host(GET_EXECUTION_CONTEXT, json!({})) {
            Ok(context) => runtime::session_scope(&context),
            Err(_) => {
                // Prefix-only hosts used the session environment and tool context.
                let last_scope = self.legacy_scope.as_ref()?;
                if let Some(id) = std::env::var_os(rpi_plugin_sdk::SESSION_ID_ENV) {
                    return id
                        .to_str()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(|id| format!("session:{id}"));
                }
                last_scope.lock().unwrap_or_else(|p| p.into_inner()).clone()
            }
        }
    }

    pub(crate) fn shutdown(&self) {
        self.workers.stop();
        self.workers.join();
        self.runtime.shutdown_connections();
    }

    pub(crate) fn cookie(&self) -> *mut c_void {
        self.id as *mut c_void
    }

    pub(crate) fn release_legacy_slot(&self) {
        if self.legacy_scope.is_some() {
            release(self.id);
        }
    }
}

pub(crate) fn install(
    api: PluginApi,
    lifecycle: Option<lifecycle::Register>,
) -> Result<(usize, Arc<Registration>), i32> {
    let (slot, registration) = {
        let mut slots = slots();
        let slot = slots.entries.iter().position(Option::is_none).ok_or(1)?;
        let id = slots.next_id;
        slots.next_id = id.checked_add(1).ok_or(1)?;
        let registration = Arc::new(Registration {
            id,
            api,
            runtime: runtime::new_runtime(),
            workers: Workers::default(),
            legacy_scope: lifecycle.is_none().then(Mutex::default),
        });
        slots.entries[slot] = Some(registration.clone());
        (slot, registration)
    };
    if let Some(register) = lifecycle {
        let rc = register(begin, finish, registration.cookie());
        if rc != 0 {
            release(registration.id);
            return Err(rc);
        }
    }
    Ok((slot, registration))
}

pub(crate) fn for_tool(slot: usize) -> Option<Arc<Registration>> {
    slots().entries[slot].clone()
}

pub(crate) fn for_event(cookie: *mut c_void) -> Option<Arc<Registration>> {
    slots()
        .entries
        .iter()
        .flatten()
        .find(|entry| entry.id == cookie as usize)
        .cloned()
}

fn release(id: usize) {
    let removed = {
        let mut slots = slots();
        slots
            .entries
            .iter_mut()
            .find(|entry| entry.as_ref().is_some_and(|entry| entry.id == id))
            .and_then(Option::take)
    };
    drop(removed);
}

extern "C" fn begin(cookie: *mut c_void) {
    if let Some(registration) = for_event(cookie) {
        registration.workers.stop();
    }
}

extern "C" fn finish(cookie: *mut c_void) -> i32 {
    std::panic::catch_unwind(|| {
        if let Some(registration) = for_event(cookie) {
            registration.shutdown();
            release(registration.id);
        }
    })
    .map_or(1, |()| 0)
}
