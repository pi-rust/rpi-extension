//! Registration-owned state and loader teardown.

use crate::{control::Control, host::Runtime, kit::Workers, lifecycle, GraphState};
use rpi_plugin_sdk::PluginApi;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

pub(crate) const SHARED_SLOTS: usize = 128;
pub(crate) const LEGACY_SLOT: usize = SHARED_SLOTS;
type Slots = [Option<Arc<Registration>>; SHARED_SLOTS + 1];
static SLOTS: OnceLock<Mutex<Slots>> = OnceLock::new();

#[derive(Default)]
pub(crate) struct Registration {
    pub graph: Mutex<GraphState>,
    pub host: Option<Runtime>,
    pub stop: Control,
    pub workers: Workers,
}

fn slots() -> MutexGuard<'static, Slots> {
    SLOTS
        .get_or_init(|| Mutex::new(std::array::from_fn(|_| None)))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn get(slot: usize) -> Option<Arc<Registration>> {
    slots().get(slot)?.clone()
}

pub(crate) fn callback_data(slot: usize) -> *mut c_void {
    (slot + 1) as *mut c_void
}

pub(crate) fn callback(data: *mut c_void) -> Option<Arc<Registration>> {
    let registration = get((data as usize).checked_sub(1)?)?;
    (!registration.stop.is_cancelled()).then_some(registration)
}

pub(crate) fn install(api: &PluginApi, cleanup: lifecycle::Register) -> Result<usize, i32> {
    let slot = {
        let mut slots = slots();
        let slot = slots[..SHARED_SLOTS]
            .iter()
            .position(Option::is_none)
            .ok_or(1)?;
        slots[slot] = Some(new_registration(api));
        slot
    };
    let rc = cleanup(begin, finish, callback_data(slot));
    if rc != 0 {
        slots()[slot] = None;
        return Err(rc);
    }
    Ok(slot)
}

pub(crate) fn install_legacy(api: &PluginApi) {
    // Older loaders use a private image. Publish only after registration
    // succeeds, because those hosts have no rollback hook.
    slots()[LEGACY_SLOT] = Some(new_registration(api));
}

fn new_registration(api: &PluginApi) -> Arc<Registration> {
    Arc::new(Registration {
        host: Some(api.into()),
        ..Registration::default()
    })
}

extern "C" fn begin(data: *mut c_void) {
    if let Some(registration) = callback(data) {
        registration.stop.cancel();
    }
}

extern "C" fn finish(data: *mut c_void) -> i32 {
    let Some(slot) = (data as usize).checked_sub(1) else {
        return 1;
    };
    if let Some(registration) = get(slot) {
        registration.stop.cancel();
        registration.workers.finish();
        // The host has drained all callbacks for this registration. Retire
        // the slot only after its workers have also exited.
        slots()[slot] = None;
    }
    0
}
