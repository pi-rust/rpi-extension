//! Loader-owned registration cleanup for the published ABI v4 SDK prefix.
use crate::register;
use rpi_plugin_sdk::PluginApi;
use std::{ffi::c_void, mem::size_of};

pub(crate) type Register = extern "C" fn(
    extern "C" fn(*mut c_void),
    extern "C" fn(*mut c_void) -> i32,
    *mut c_void,
) -> i32;

// The native PluginApi appends streaming-provider and wake-driven-tool
// registrars to the SDK 0.3.20 prefix, followed by the lifecycle registrar.
const LIFECYCLE_OFFSET: usize = size_of::<PluginApi>() + 2 * size_of::<usize>();

#[allow(unsafe_code)]
#[allow(clippy::not_unsafe_ptr_arg_deref)] // Same host contract as export_plugin!.
#[no_mangle]
pub extern "C" fn rpi_plugin_register(raw: *const PluginApi) -> i32 {
    // SAFETY: the host supplies a valid vtable pointer. The SDK checks ABI
    // version and prefix size and contains panics before returning to the host.
    unsafe {
        rpi_plugin_sdk::register_entrypoint(raw, |api| {
            let lifecycle =
                if api.struct_size as usize >= LIFECYCLE_OFFSET + size_of::<Option<Register>>() {
                    // Read from the original allocation, whose advertised size
                    // includes the slot, rather than from the SDK's prefix reference.
                    raw.cast::<u8>()
                        .add(LIFECYCLE_OFFSET)
                        .cast::<Option<Register>>()
                        .read()
                } else {
                    None
                };
            register(api, lifecycle)
        })
    }
}

// The ABI's tool execute callback has no registration data parameter. Give each
// live registration its own execute callback, with a slot reclaimed at finish.
macro_rules! execute_table {
    ($execute:ident) => {{
        [
            $execute::<0>,
            $execute::<1>,
            $execute::<2>,
            $execute::<3>,
            $execute::<4>,
            $execute::<5>,
            $execute::<6>,
            $execute::<7>,
            $execute::<8>,
            $execute::<9>,
            $execute::<10>,
            $execute::<11>,
            $execute::<12>,
            $execute::<13>,
            $execute::<14>,
            $execute::<15>,
            $execute::<16>,
            $execute::<17>,
            $execute::<18>,
            $execute::<19>,
            $execute::<20>,
            $execute::<21>,
            $execute::<22>,
            $execute::<23>,
            $execute::<24>,
            $execute::<25>,
            $execute::<26>,
            $execute::<27>,
            $execute::<28>,
            $execute::<29>,
            $execute::<30>,
            $execute::<31>,
            $execute::<32>,
            $execute::<33>,
            $execute::<34>,
            $execute::<35>,
            $execute::<36>,
            $execute::<37>,
            $execute::<38>,
            $execute::<39>,
            $execute::<40>,
            $execute::<41>,
            $execute::<42>,
            $execute::<43>,
            $execute::<44>,
            $execute::<45>,
            $execute::<46>,
            $execute::<47>,
            $execute::<48>,
            $execute::<49>,
            $execute::<50>,
            $execute::<51>,
            $execute::<52>,
            $execute::<53>,
            $execute::<54>,
            $execute::<55>,
            $execute::<56>,
            $execute::<57>,
            $execute::<58>,
            $execute::<59>,
            $execute::<60>,
            $execute::<61>,
            $execute::<62>,
            $execute::<63>,
            $execute::<64>,
            $execute::<65>,
            $execute::<66>,
            $execute::<67>,
            $execute::<68>,
            $execute::<69>,
            $execute::<70>,
            $execute::<71>,
            $execute::<72>,
            $execute::<73>,
            $execute::<74>,
            $execute::<75>,
            $execute::<76>,
            $execute::<77>,
            $execute::<78>,
            $execute::<79>,
            $execute::<80>,
            $execute::<81>,
            $execute::<82>,
            $execute::<83>,
            $execute::<84>,
            $execute::<85>,
            $execute::<86>,
            $execute::<87>,
            $execute::<88>,
            $execute::<89>,
            $execute::<90>,
            $execute::<91>,
            $execute::<92>,
            $execute::<93>,
            $execute::<94>,
            $execute::<95>,
            $execute::<96>,
            $execute::<97>,
            $execute::<98>,
            $execute::<99>,
            $execute::<100>,
            $execute::<101>,
            $execute::<102>,
            $execute::<103>,
            $execute::<104>,
            $execute::<105>,
            $execute::<106>,
            $execute::<107>,
            $execute::<108>,
            $execute::<109>,
            $execute::<110>,
            $execute::<111>,
            $execute::<112>,
            $execute::<113>,
            $execute::<114>,
            $execute::<115>,
            $execute::<116>,
            $execute::<117>,
            $execute::<118>,
            $execute::<119>,
            $execute::<120>,
            $execute::<121>,
            $execute::<122>,
            $execute::<123>,
            $execute::<124>,
            $execute::<125>,
            $execute::<126>,
            $execute::<127>,
        ]
    }};
}
pub(crate) use execute_table;
