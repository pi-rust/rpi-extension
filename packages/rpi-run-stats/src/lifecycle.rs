//! Compatibility with the loader-owned shutdown hooks appended to ABI v4.
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
