use super::*;
use crate::lifecycle::{self, Register};
use rpi_plugin_sdk::{callbacks::Step, PluginApi, StableToolSchema};
use serde_json::json;
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Loader {
    hooks: Option<Hooks>,
    tool_count: usize,
    reject_tool: Option<usize>,
}

type Hooks = (
    extern "C" fn(*mut c_void),
    extern "C" fn(*mut c_void) -> i32,
    usize,
);
thread_local! {
    static LOADER: RefCell<Loader> = RefCell::default();
}
static INVOCATION: AtomicUsize = AtomicUsize::new(0);
static WORKER_EXITED: AtomicBool = AtomicBool::new(false);

extern "C" fn lifecycle(
    begin: extern "C" fn(*mut c_void),
    finish: extern "C" fn(*mut c_void) -> i32,
    data: *mut c_void,
) -> i32 {
    LOADER.with_borrow_mut(|loader| loader.hooks = Some((begin, finish, data as usize)));
    0
}

#[allow(unsafe_code)]
extern "C" fn register_tool(
    schema: *const StableToolSchema,
    _: ToolExecuteFn,
    _: ToolPollFn,
    _: ToolCancelFn,
    _: ToolDestroyFn,
    free: FreeStringFn,
) -> i32 {
    // SAFETY: the plugin provides a live schema and transfers its strings.
    let schema = unsafe { &*schema };
    free(schema.name);
    free(schema.description);
    free(schema.parameters);
    LOADER.with_borrow_mut(|loader| {
        loader.tool_count += 1;
        if loader.reject_tool == Some(loader.tool_count) {
            17
        } else {
            0
        }
    })
}

extern "C" fn runtime_action(_: u32, _: StbStringRef, _: *mut StbString, data: *mut c_void) -> i32 {
    INVOCATION.store(data as usize, Ordering::Release);
    1
}

#[repr(C)]
struct HostApi {
    prefix: PluginApi,
    appended: [usize; 2],
    lifecycle: Option<Register>,
}

fn load(owner: usize, reject_tool: Option<usize>) -> (i32, Hooks) {
    LOADER.with_borrow_mut(|loader| {
        *loader = Loader {
            reject_tool,
            ..Loader::default()
        }
    });
    let api = HostApi {
        prefix: PluginApi {
            abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: std::mem::size_of::<HostApi>() as u32,
            free_string: plugin_free_string,
            register_tool: Some(register_tool),
            register_command: None,
            register_shortcut: None,
            register_flag: None,
            register_provider: None,
            register_message_renderer: None,
            register_markdown_transformer: None,
            register_entry_renderer: None,
            register_event_handler: None,
            register_resources_discover: None,
            runtime_action,
            dispatch_event: None,
            user_data: owner as *mut c_void,
            declare: None,
            register_before_agent_start: None,
            register_oauth_provider: None,
        },
        appended: [0; 2],
        lifecycle: Some(lifecycle),
    };
    let status = lifecycle::rpi_plugin_register(std::ptr::addr_of!(api).cast());
    (
        status,
        LOADER.with_borrow_mut(|loader| loader.hooks.take().unwrap()),
    )
}

fn run_plan(slot: usize, params: Value) -> String {
    let callbacks = callbacks_for(slot);
    let handle = (callbacks.executes[1])(
        StbStringRef::from_str("test-plan"),
        StbString::from_string(params.to_string()),
        Some(plugin_free_string),
    );
    assert!(!handle.is_null());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let step =
            rpi_plugin_sdk::take_step_result!((callbacks.poll)(handle, None, std::ptr::null_mut()));
        match step {
            Step::Pending(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Step::Done(result) => {
                (callbacks.destroy)(handle);
                return result;
            }
            step => {
                (callbacks.destroy)(handle);
                panic!("plan callback failed: {step:?}");
            }
        }
    }
}

fn waiting_worker(
    registration: &Registration,
    params: &Value,
    control: &Control,
    _: &Progress,
) -> Result<String, String> {
    crate::host::parent_selection(registration.host.as_ref(), params);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !control.is_cancelled() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    WORKER_EXITED.store(true, Ordering::Release);
    control.check()?;
    Err("worker was not cancelled".into())
}

#[test]
fn registrations_isolate_graphs_and_failed_reload_cleanup() {
    let (status, (begin_a, finish_a, data_a)) = load(100, None);
    assert_eq!(status, 0);
    let (status, (begin_b, finish_b, data_b)) = load(200, None);
    assert_eq!(status, 0);
    let a = data_a - 1;
    let b = data_b - 1;
    let set = |id| json!({"action":"set", "nodes":[{"id":id,"task":"test"}]});
    assert!(run_plan(a, set("alpha")).contains("alpha"));
    assert!(run_plan(b, set("bravo")).contains("bravo"));

    let worker = execute(
        a,
        StbString::from_string(json!({"__rpi":{"runtimeHandle":314}}).to_string()),
        Some(plugin_free_string),
        waiting_worker,
    );
    assert!(!worker.is_null());
    let deadline = Instant::now() + Duration::from_secs(3);
    while INVOCATION.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(INVOCATION.load(Ordering::Acquire), 314);
    (callbacks_for(b).cancel)(worker);
    assert!(!runtime::get(a).unwrap().workers.lock()[&(worker as usize)]
        .control
        .is_cancelled());

    let (status, (begin_failed, finish_failed, failed)) = load(100, Some(3));
    assert_eq!(status, 17);
    assert_ne!(failed, data_a);
    begin_failed(failed as *mut c_void);
    assert_eq!(finish_failed(failed as *mut c_void), 0);
    assert!(runtime::get(failed - 1).is_none());
    assert!(!runtime::get(a).unwrap().workers.lock()[&(worker as usize)]
        .control
        .is_cancelled());
    assert!(run_plan(a, json!({})).contains("alpha"));
    assert!(run_plan(b, json!({})).contains("bravo"));

    begin_a(data_a as *mut c_void);
    assert!(runtime::get(a).unwrap().workers.lock()[&(worker as usize)]
        .control
        .is_cancelled());
    assert_eq!(finish_a(data_a as *mut c_void), 0);
    assert!(WORKER_EXITED.load(Ordering::Acquire));
    assert!(runtime::get(a).is_none());
    assert!(run_plan(b, json!({})).contains("bravo"));
    begin_b(data_b as *mut c_void);
    assert_eq!(finish_b(data_b as *mut c_void), 0);
}
