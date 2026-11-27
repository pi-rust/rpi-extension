//! Loader-owned registration cleanup.
use crate::{execute, register, Registration};
use rpi_plugin_sdk::{PluginApi, ToolExecuteFn};
use std::{
    ffi::c_void,
    sync::{atomic::Ordering, Arc, Mutex, MutexGuard},
};

pub(crate) use rpi_plugin_sdk::RegisterPluginLifecycleFn as Register;

#[cfg(unix)]
rpi_plugin_sdk::export_shared_plugin!(|api| register(api, api.register_plugin_lifecycle));

#[cfg(not(unix))]
rpi_plugin_sdk::export_plugin!(|api| register(api, api.register_plugin_lifecycle));

// A tool execute callback has no registration context. Give each live
// registration its own callback and reclaim its slot after the host drains it.
const SLOTS: usize = 128;
const LEGACY_SLOT: usize = SLOTS;
const EMPTY_SLOT: Option<Arc<Registration>> = None;
static SHARED: Mutex<[Option<Arc<Registration>>; SLOTS]> = Mutex::new([EMPTY_SLOT; SLOTS]);
static LEGACY: Mutex<Option<Arc<Registration>>> = Mutex::new(None);

#[rustfmt::skip]
const EXECUTE: [ToolExecuteFn; SLOTS] = [
    execute::<0>, execute::<1>, execute::<2>, execute::<3>, execute::<4>, execute::<5>, execute::<6>, execute::<7>,
    execute::<8>, execute::<9>, execute::<10>, execute::<11>, execute::<12>, execute::<13>, execute::<14>, execute::<15>,
    execute::<16>, execute::<17>, execute::<18>, execute::<19>, execute::<20>, execute::<21>, execute::<22>, execute::<23>,
    execute::<24>, execute::<25>, execute::<26>, execute::<27>, execute::<28>, execute::<29>, execute::<30>, execute::<31>,
    execute::<32>, execute::<33>, execute::<34>, execute::<35>, execute::<36>, execute::<37>, execute::<38>, execute::<39>,
    execute::<40>, execute::<41>, execute::<42>, execute::<43>, execute::<44>, execute::<45>, execute::<46>, execute::<47>,
    execute::<48>, execute::<49>, execute::<50>, execute::<51>, execute::<52>, execute::<53>, execute::<54>, execute::<55>,
    execute::<56>, execute::<57>, execute::<58>, execute::<59>, execute::<60>, execute::<61>, execute::<62>, execute::<63>,
    execute::<64>, execute::<65>, execute::<66>, execute::<67>, execute::<68>, execute::<69>, execute::<70>, execute::<71>,
    execute::<72>, execute::<73>, execute::<74>, execute::<75>, execute::<76>, execute::<77>, execute::<78>, execute::<79>,
    execute::<80>, execute::<81>, execute::<82>, execute::<83>, execute::<84>, execute::<85>, execute::<86>, execute::<87>,
    execute::<88>, execute::<89>, execute::<90>, execute::<91>, execute::<92>, execute::<93>, execute::<94>, execute::<95>,
    execute::<96>, execute::<97>, execute::<98>, execute::<99>, execute::<100>, execute::<101>, execute::<102>, execute::<103>,
    execute::<104>, execute::<105>, execute::<106>, execute::<107>, execute::<108>, execute::<109>, execute::<110>, execute::<111>,
    execute::<112>, execute::<113>, execute::<114>, execute::<115>, execute::<116>, execute::<117>, execute::<118>, execute::<119>,
    execute::<120>, execute::<121>, execute::<122>, execute::<123>, execute::<124>, execute::<125>, execute::<126>, execute::<127>,
];

fn shared() -> MutexGuard<'static, [Option<Arc<Registration>>; SLOTS]> {
    SHARED.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn legacy() -> Option<Arc<Registration>> {
    LEGACY.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub(crate) fn install_legacy(api: PluginApi) {
    *LEGACY.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(Registration::new(api)));
}

pub(crate) fn install(api: PluginApi, cleanup: Register) -> Result<ToolExecuteFn, i32> {
    let owner = api.user_data;
    let slot = {
        let mut slots = shared();
        if owner.is_null()
            || slots
                .iter()
                .flatten()
                .any(|item| item.runtime.user_data == owner)
        {
            return Err(1);
        }
        let slot = slots.iter().position(Option::is_none).ok_or(1)?;
        slots[slot] = Some(Arc::new(Registration::new(api)));
        slot
    };
    let rc = cleanup(begin, finish, owner);
    if rc != 0 {
        shared()[slot] = None;
        return Err(rc);
    }
    // The host runs the installed hooks even if a later registrar fails.
    Ok(EXECUTE[slot])
}

pub(crate) const LEGACY_EXECUTE: ToolExecuteFn = execute::<LEGACY_SLOT>;

fn owned_by(registration: &Registration, owner: *mut c_void) -> bool {
    registration.runtime.user_data == owner
}

pub(crate) fn callback(owner: *mut c_void) -> Option<Arc<Registration>> {
    shared()
        .iter()
        .flatten()
        .find(|registration| owned_by(registration, owner))
        .cloned()
}

pub(crate) fn tool<const SLOT: usize>() -> Option<Arc<Registration>> {
    if SLOT == LEGACY_SLOT {
        legacy()
    } else {
        shared()[SLOT].clone()
    }
}

extern "C" fn begin(owner: *mut c_void) {
    if let Some(registration) = callback(owner) {
        registration.stopping.store(true, Ordering::Release);
    }
}

extern "C" fn finish(owner: *mut c_void) -> i32 {
    let mut slots = shared();
    if let Some(slot) = slots.iter_mut().find(|slot| {
        slot.as_ref()
            .is_some_and(|registration| owned_by(registration, owner))
    }) {
        *slot = None;
    }
    0
}
