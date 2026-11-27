//! Mock host registrars for extension tests using the published plugin SDK.

use rpi_plugin_sdk::{
    callbacks::OwnedString, guard_or, FreeStringFn, StableToolSchema, ToolCancelFn, ToolDestroyFn,
    ToolExecuteFn, ToolPollFn,
};

pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: String,
}

#[derive(Clone, Copy)]
pub struct ToolCallbacks {
    pub execute: ToolExecuteFn,
    pub poll: ToolPollFn,
    pub cancel: ToolCancelFn,
    pub destroy: ToolDestroyFn,
}

pub trait ToolRegistration {
    fn register_tool(schema: ToolSchema, callbacks: ToolCallbacks) -> i32;
}

type RegisterToolFn = extern "C" fn(
    *const StableToolSchema,
    ToolExecuteFn,
    ToolPollFn,
    ToolCancelFn,
    ToolDestroyFn,
    FreeStringFn,
) -> i32;

/// Return a mock host's tool callback for a plugin SDK registration table.
/// The plugin SDK must supply a live schema and transfer all three strings.
pub fn tool_registrar<R: ToolRegistration>() -> RegisterToolFn {
    register_tool::<R>
}

extern "C" fn register_tool<R: ToolRegistration>(
    raw: *const StableToolSchema,
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    cancel: ToolCancelFn,
    destroy: ToolDestroyFn,
    free: FreeStringFn,
) -> i32 {
    guard_or(1, || {
        // SAFETY: a non-null schema is live for the duration of the ABI call.
        let Some(raw) = (unsafe { raw.as_ref() }) else {
            return 1;
        };
        let schema = {
            // SAFETY: the ABI transfers each field's buffer once and supplies
            // its producer's deallocator. Adopt every field before decoding.
            let (name, description, parameters) = unsafe {
                (
                    OwnedString::from_raw(raw.name, Some(free)),
                    OwnedString::from_raw(raw.description, Some(free)),
                    OwnedString::from_raw(raw.parameters, Some(free)),
                )
            };
            ToolSchema {
                name: name.to_string_lossy(),
                description: description.to_string_lossy(),
                parameters: parameters.to_string_lossy(),
            }
        };
        R::register_tool(
            schema,
            ToolCallbacks {
                execute,
                poll,
                cancel,
                destroy,
            },
        )
    })
}
