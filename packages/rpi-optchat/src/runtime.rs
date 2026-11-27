//! Bind callbacks and their state to the native registration that owns them.
use super::{compact, lifecycle, StateCache, ORCHESTRATOR_SUBAGENT_ENV};
use rpi_plugin_sdk::{PluginApi, StepHandle};
use std::{
    cell::Cell,
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

pub(crate) const SLOTS: usize = 128;
const LEGACY: usize = SLOTS;
static REGISTRATIONS: Mutex<[Option<Arc<Registration>>; SLOTS + 1]> =
    Mutex::new([const { None }; SLOTS + 1]);
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

pub(crate) struct Registration {
    pub(crate) api: PluginApi,
    pub(crate) state: Mutex<StateCache>,
    pub(crate) append_gate: Mutex<()>,
    pub(crate) is_subagent: bool,
    pub(crate) stopping: Arc<AtomicBool>,
    pub(crate) branch_generation: Arc<AtomicU64>,
    pub(crate) compactor: Mutex<Option<compact::Engine>>,
    pub(crate) has_cleanup: bool,
    slot: usize,
    id: usize,
}

impl Registration {
    fn new(api: PluginApi, slot: usize) -> Self {
        Self {
            api,
            state: Mutex::default(),
            append_gate: Mutex::new(()),
            is_subagent: std::env::var_os(ORCHESTRATOR_SUBAGENT_ENV).is_some()
                || std::env::var_os(rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV).is_some(),
            stopping: Arc::default(),
            branch_generation: Arc::default(),
            compactor: Mutex::new(None),
            has_cleanup: slot != LEGACY,
            slot,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub(crate) fn branch_generation(&self) -> u64 {
        self.branch_generation.load(Ordering::Acquire)
    }

    pub(crate) fn invalidate_compaction(&self) {
        self.branch_generation.fetch_add(1, Ordering::AcqRel);
    }
}

fn registrations() -> MutexGuard<'static, [Option<Arc<Registration>>; SLOTS + 1]> {
    REGISTRATIONS.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn reserve(api: PluginApi, cleanup: lifecycle::Register) -> Result<usize, i32> {
    let (slot, id) = {
        let mut registrations = registrations();
        if api.user_data.is_null()
            || registrations[..SLOTS]
                .iter()
                .flatten()
                .any(|registration| registration.api.user_data == api.user_data)
        {
            return Err(1);
        }
        let slot = registrations[..SLOTS]
            .iter()
            .position(Option::is_none)
            .ok_or(1)?;
        let registration = Registration::new(api, slot);
        let id = registration.id;
        registrations[slot] = Some(Arc::new(registration));
        (slot, id)
    };
    let rc = cleanup(begin, finish, id as *mut c_void);
    if rc != 0 {
        registrations()[slot] = None;
        return Err(rc);
    }
    Ok(slot)
}

pub(crate) fn install_legacy(api: PluginApi) {
    registrations()[LEGACY] = Some(Arc::new(Registration::new(api, LEGACY)));
}

fn by_id(id: usize) -> Option<Arc<Registration>> {
    registrations()[..SLOTS]
        .iter()
        .flatten()
        .find(|registration| registration.id == id)
        .cloned()
}

extern "C" fn begin(token: *mut c_void) {
    if let Some(registration) = by_id(token as usize) {
        registration.stopping.store(true, Ordering::Release);
    }
}

extern "C" fn finish(token: *mut c_void) -> i32 {
    begin(token);
    let registration = {
        let mut registrations = registrations();
        registrations[..SLOTS]
            .iter_mut()
            .find(|slot| {
                slot.as_ref()
                    .is_some_and(|registration| registration.id == token as usize)
            })
            .and_then(Option::take)
    };
    if let Some(registration) = registration {
        let engine = registration
            .compactor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(engine) = engine {
            engine.finish();
        }
    }
    0
}

pub(crate) fn callback(owner: *mut c_void) -> Option<Arc<Registration>> {
    let registrations = registrations();
    let registration = registrations[..SLOTS]
        .iter()
        .flatten()
        .find(|registration| registration.api.user_data == owner)
        .or_else(|| {
            registrations[LEGACY]
                .as_ref()
                .filter(|registration| owner.is_null() || registration.api.user_data == owner)
        })?;
    (!registration.is_stopping()).then(|| registration.clone())
}

pub(crate) fn tool<const SLOT: usize>() -> Option<Arc<Registration>> {
    let registration = registrations()[SLOT].as_ref()?.clone();
    (!registration.is_stopping()).then_some(registration)
}

thread_local! {
    // Keep TLS free of destructors that could outlive the mapped plug-in.
    static SCOPED: Cell<Option<CallbackScope>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
struct CallbackScope {
    slot: usize,
    id: usize,
    tool: Option<usize>,
}

pub(crate) fn current() -> Option<Arc<Registration>> {
    let scope = SCOPED.with(Cell::get)?;
    if scope.tool.is_some_and(|token| {
        super::HANDLES
            .get(token as StepHandle)
            .map_or(true, |entry| entry.cancelled.load(Ordering::Acquire))
    }) {
        return None;
    }
    let registration = registrations()[scope.slot].as_ref()?.clone();
    (registration.id == scope.id && !registration.is_stopping()).then_some(registration)
}

/// Thread-bound and nestable. Host calls run outside the registration lock.
pub(crate) struct Scope {
    previous: Option<CallbackScope>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        SCOPED.with(|scope| scope.set(self.previous));
    }
}

pub(crate) fn enter(registration: &Registration) -> Scope {
    enter_scope(registration, None)
}

pub(crate) fn enter_tool(registration: &Registration, handle: StepHandle) -> Scope {
    enter_scope(registration, Some(handle as usize))
}

fn enter_scope(registration: &Registration, tool: Option<usize>) -> Scope {
    Scope {
        previous: SCOPED.with(|scope| {
            scope.replace(Some(CallbackScope {
                slot: registration.slot,
                id: registration.id,
                tool,
            }))
        }),
        _thread_bound: PhantomData,
    }
}
