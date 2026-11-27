//! Registration state stays with its owner across overlapping reloads.
use rpi_plugin_sdk::PluginApi;
use serde_json::{json, Value};
use std::{
    cell::Cell,
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock, RwLock},
};

pub(crate) const SLOTS: usize = 128;
static REGISTRATIONS: Mutex<[Option<Arc<Session>>; SLOTS]> = Mutex::new([const { None }; SLOTS]);
static LEGACY_API: RwLock<Option<PluginApi>> = RwLock::new(None);
static LEGACY_STATE: OnceLock<Arc<Session>> = OnceLock::new();

#[derive(Default)]
pub(crate) struct Session {
    api: Option<PluginApi>,
    pub(crate) branch: crate::state::BranchState,
    pub(crate) notices: crate::inject::Notices,
    pub(crate) events: crate::events::RunEvents,
    pub(crate) worker: crate::dispatch::Worker,
}

#[derive(Clone)]
pub(crate) struct Binding {
    pub(crate) session: Arc<Session>,
    api: PluginApi,
}

#[derive(Clone, Copy)]
struct CallbackScope {
    registration_owner: Option<*mut c_void>,
    api: PluginApi,
}

thread_local! {
    // A host thread can outlive the plugin image. Store only Copy values here.
    static SCOPED: Cell<Option<CallbackScope>> = const { Cell::new(None) };
}

fn legacy_state() -> Arc<Session> {
    LEGACY_STATE
        .get_or_init(|| Arc::new(Session::default()))
        .clone()
}

fn registrations() -> std::sync::MutexGuard<'static, [Option<Arc<Session>>; SLOTS]> {
    REGISTRATIONS.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn install(
    api: PluginApi,
    cleanup: Option<crate::lifecycle::Register>,
) -> Result<usize, i32> {
    let Some(cleanup) = cleanup else {
        *LEGACY_API.write().unwrap_or_else(|p| p.into_inner()) = Some(api);
        return Ok(SLOTS);
    };
    let slot = {
        let mut registrations = registrations();
        if api.user_data.is_null()
            || registrations.iter().flatten().any(|session| {
                session
                    .api
                    .is_some_and(|other| other.user_data == api.user_data)
            })
        {
            return Err(1);
        }
        let slot = registrations.iter().position(Option::is_none).ok_or(1)?;
        registrations[slot] = Some(Arc::new(Session {
            api: Some(api),
            ..Session::default()
        }));
        slot
    };
    let rc = cleanup(begin, finish, api.user_data);
    if rc != 0 {
        registrations()[slot] = None;
        return Err(rc);
    }
    Ok(slot)
}

fn lookup(owner: *mut c_void) -> Option<Binding> {
    registrations().iter().flatten().find_map(|session| {
        let api = session.api?;
        (api.user_data == owner).then(|| Binding {
            session: session.clone(),
            api,
        })
    })
}

fn legacy(owner: *mut c_void) -> Option<Binding> {
    let mut api = (*LEGACY_API.read().unwrap_or_else(|p| p.into_inner()))?;
    if !owner.is_null() {
        api.user_data = owner;
    }
    Some(Binding {
        session: legacy_state(),
        api,
    })
}

pub(crate) fn callback(owner: *mut c_void) -> Option<Binding> {
    lookup(owner)
        .or_else(|| legacy(owner))
        .filter(|binding| !binding.session.worker.is_stopping())
}

// Legacy callbacks may still need to checkpoint after receiving SessionShutdown.
pub(crate) fn event(owner: *mut c_void) -> Option<Binding> {
    lookup(owner)
        .filter(|binding| !binding.session.worker.is_stopping())
        .or_else(|| legacy(owner))
}

pub(crate) fn tool<const SLOT: usize>(params: &Value) -> Result<Binding, String> {
    if SLOT == SLOTS {
        return legacy(std::ptr::null_mut()).ok_or_else(|| "goal runtime is unavailable".into());
    }
    let session = registrations()[SLOT]
        .clone()
        .ok_or("goal registration is unavailable")?;
    if session.worker.is_stopping() {
        return Err("goal registration is shutting down".into());
    }
    let mut api = session.api.ok_or("goal runtime is unavailable")?;
    let handle = params["__rpi"]["runtimeHandle"]
        .as_u64()
        .and_then(|handle| usize::try_from(handle).ok())
        .filter(|handle| *handle != 0)
        .ok_or("goal requires a host tool invocation runtimeHandle")?;
    api.user_data = handle as *mut c_void;
    Ok(Binding { session, api })
}

pub(crate) fn authorize_tool(binding: &Binding) -> Result<(), String> {
    if binding.session.worker.is_stopping() {
        return Err("goal registration is shutting down".into());
    }
    if crate::is_subagent() {
        return Err("Subagents cannot manage the parent goal".into());
    }
    if binding.session.api.is_some() {
        let context =
            crate::call_runtime_raw(crate::raw_actions::GET_EXECUTION_CONTEXT, json!({}))?;
        // childAgent reports whether this execution may spawn a child.
        if context["childAgent"].as_bool() == Some(false) {
            return Err("Subagents cannot manage the parent goal".into());
        }
    }
    Ok(())
}

pub(crate) fn state() -> Arc<Session> {
    current()
        .map(|binding| binding.session)
        .unwrap_or_else(legacy_state)
}

pub(crate) fn current() -> Option<Binding> {
    let Some(scope) = SCOPED.with(Cell::get) else {
        return legacy(std::ptr::null_mut());
    };
    let session = match scope.registration_owner {
        Some(owner) => lookup(owner)?.session,
        None => legacy_state(),
    };
    Some(Binding {
        session,
        api: scope.api,
    })
}

pub(crate) fn snapshot() -> Option<PluginApi> {
    current().map(|binding| binding.api)
}

impl Session {
    pub(crate) fn registration_binding(self: &Arc<Self>) -> Option<Binding> {
        self.api.map(|api| Binding {
            session: self.clone(),
            api,
        })
    }
}

pub(crate) struct Scope {
    previous: Option<CallbackScope>,
    _binding: Option<Binding>,
    _thread_bound: PhantomData<Rc<()>>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        SCOPED.with(|scope| scope.set(self.previous));
    }
}
pub(crate) fn enter(binding: Option<Binding>) -> Scope {
    let callback = binding.as_ref().map(|binding| CallbackScope {
        registration_owner: binding.session.api.map(|api| api.user_data),
        api: binding.api,
    });
    Scope {
        previous: SCOPED.with(|scope| scope.replace(callback)),
        _binding: binding,
        _thread_bound: PhantomData,
    }
}

extern "C" fn begin(owner: *mut c_void) {
    if let Some(binding) = lookup(owner) {
        binding.session.worker.stop();
    }
}

extern "C" fn finish(owner: *mut c_void) -> i32 {
    let Some(binding) = lookup(owner) else {
        return 0;
    };
    binding.session.worker.finish();
    let mut registrations = registrations();
    if let Some(slot) = registrations.iter_mut().find(|slot| {
        slot.as_ref()
            .is_some_and(|session| Arc::ptr_eq(session, &binding.session))
    }) {
        *slot = None;
    }
    0
}
