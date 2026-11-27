use super::*;
use rpi_plugin_sdk::{
    CommandHandlerFn, StablePluginEvent, ToolDestroyFn, ToolExecuteFn, ToolPollFn,
};
use std::{cell::RefCell, collections::HashMap};

#[derive(Default)]
struct Callbacks {
    cleanup: Option<Cleanup>,
    command: Option<CommandHandlerFn>,
    events: Vec<(EventTag, EventHandlerFn, *mut c_void)>,
    tool: Option<(ToolExecuteFn, ToolPollFn, ToolDestroyFn)>,
    reject_cleanup: bool,
    reject_tool: bool,
}

struct Cleanup {
    begin: extern "C" fn(*mut c_void),
    finish: extern "C" fn(*mut c_void) -> i32,
    owner: *mut c_void,
}

#[derive(Default)]
struct Branch {
    entry: Option<Value>,
    fail_restore: bool,
}

thread_local! {
    static CALLBACKS: RefCell<Callbacks> = RefCell::default();
    static BRANCHES: RefCell<HashMap<usize, Branch>> = RefCell::default();
}

extern "C" fn capture_cleanup(
    begin: extern "C" fn(*mut c_void),
    finish: extern "C" fn(*mut c_void) -> i32,
    owner: *mut c_void,
) -> i32 {
    CALLBACKS.with_borrow_mut(|callbacks| {
        if callbacks.reject_cleanup {
            return 2;
        }
        callbacks.cleanup = Some(Cleanup {
            begin,
            finish,
            owner,
        });
        0
    })
}

extern "C" fn capture_command(_: StbStringRef, _: StbStringRef, handler: CommandHandlerFn) -> i32 {
    CALLBACKS.with_borrow_mut(|callbacks| callbacks.command = Some(handler));
    0
}

extern "C" fn capture_event(tag: EventTag, handler: EventHandlerFn, owner: *mut c_void) -> i32 {
    CALLBACKS.with_borrow_mut(|callbacks| callbacks.events.push((tag, handler, owner)));
    0
}

extern "C" fn capture_tool(
    _: *const StableToolSchema,
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    _: rpi_plugin_sdk::ToolCancelFn,
    destroy: ToolDestroyFn,
    _: FreeStringFn,
) -> i32 {
    CALLBACKS.with_borrow_mut(|callbacks| {
        if callbacks.reject_tool {
            return 3;
        }
        callbacks.tool = Some((execute, poll, destroy));
        0
    })
}

rpi_plugin_sdk::string_handler! {
fn runtime(action, args, out, owner) {
    BRANCHES.with_borrow_mut(|branches| {
        let branch = branches.get_mut(&(owner as usize)).expect("unknown runtime owner");
        match RuntimeActionId::try_from(action).unwrap() {
            RuntimeActionId::GetSessionBranch => {
                if branch.fail_restore {
                    out.write("branch unavailable".into());
                    return 1;
                }
                let entries: Vec<_> = branch.entry.iter().collect();
                out.write(json!({"filtered":true,"entries":entries,"sessionId":"duplicate"}).to_string());
            }
            RuntimeActionId::AppendEntry => {
                let args: Value = serde_json::from_str(args).unwrap();
                branch.entry = Some(json!({"type":"custom","customType":args["customType"],"data":args["data"]}));
                out.write(r#"{"entryId":"saved"}"#.into());
            }
            other => panic!("unexpected action: {other:?}"),
        }
        0
    })
}
}

fn register_preview(owner: usize, reject_cleanup: bool, reject_tool: bool) -> (i32, Callbacks) {
    CALLBACKS.with_borrow_mut(|callbacks| {
        callbacks.reject_cleanup = reject_cleanup;
        callbacks.reject_tool = reject_tool;
    });
    let mut api = mock_api(runtime);
    api.user_data = owner as *mut c_void;
    api.register_event_handler = Some(capture_event);
    api.register_command = Some(capture_command);
    api.register_tool = Some(capture_tool);
    api.register_plugin_lifecycle = Some(capture_cleanup);
    let status = rpi_plugin_register(&api);
    (status, CALLBACKS.with_borrow_mut(std::mem::take))
}

impl Callbacks {
    fn begin(&self) {
        let cleanup = self.cleanup.as_ref().unwrap();
        (cleanup.begin)(cleanup.owner);
    }

    fn finish(&self) {
        let cleanup = self.cleanup.as_ref().unwrap();
        assert_eq!((cleanup.finish)(cleanup.owner), 0);
    }

    fn reload(&self, tag: EventTag) -> i32 {
        let &(_, handler, owner) = self.events.iter().find(|event| event.0 == tag).unwrap();
        handler(StablePluginEvent::empty(tag), owner)
    }

    fn show(&self, owner: *mut c_void) -> Value {
        let mut out = StbString::empty();
        assert_eq!(
            (self.command.unwrap())(StbStringRef::from_str(""), &mut out, owner),
            0
        );
        let text = rpi_plugin_sdk::stb_take!(out, Some(free_string));
        serde_json::from_str(&text).unwrap()
    }

    fn execute(&self, mut params: Value) -> StepHandle {
        params["__rpi"] = json!({"sessionId":"duplicate"});
        (self.tool.unwrap().0)(
            StbStringRef::from_str("same-call-id"),
            StbString::from_string(params.to_string()),
            Some(free_string),
        )
    }

    fn settle(&self, handle: StepHandle) -> Result<Value, String> {
        let (_, poll, destroy) = self.tool.unwrap();
        let result = poll(handle, None, std::ptr::null_mut());
        destroy(handle);
        match rpi_plugin_sdk::take_step!(result) {
            rpi_plugin_sdk::callbacks::Step::Done(text) => {
                Ok(serde_json::from_str::<Value>(&text).unwrap()["details"].clone())
            }
            rpi_plugin_sdk::callbacks::Step::Err(error) => Err(error),
            rpi_plugin_sdk::callbacks::Step::Pending(_) => panic!("todo must settle synchronously"),
        }
    }

    fn invoke(&self, params: Value) -> Value {
        self.settle(self.execute(params)).unwrap()
    }
}

#[test]
fn shared_registration_isolates_duplicate_session_ids_through_teardown() {
    let (status, rejected) = register_preview(1, true, false);
    assert_eq!(status, 2);
    assert!(rejected.events.is_empty() && rejected.tool.is_none());
    assert!(lifecycle::callback(1 as *mut c_void).is_none());

    let (status, partial) = register_preview(1, false, true);
    assert_eq!(status, 3);
    partial.begin();
    partial.finish();
    assert!(lifecycle::callback(1 as *mut c_void).is_none());

    BRANCHES.with_borrow_mut(|branches| {
        *branches = (1..=3).map(|owner| (owner, Branch::default())).collect();
    });
    let (status, first) = register_preview(1, false, false);
    assert_eq!(status, 0);
    let (status, second) = register_preview(2, false, false);
    assert_eq!(status, 0);
    let (status, duplicate) = register_preview(1, false, false);
    assert_ne!(status, 0);
    assert!(duplicate.cleanup.is_none() && duplicate.tool.is_none());

    let first_owner = first.cleanup.as_ref().unwrap().owner;
    let second_owner = second.cleanup.as_ref().unwrap().owner;
    assert_eq!(first.reload(EventTag::SessionStart), 0);
    assert_eq!(second.reload(EventTag::SessionStart), 0);
    let first_add = first.execute(json!({"action":"add","text":"first only"}));
    let second_add = second.execute(json!({"action":"add","text":"second only"}));
    for (callbacks, handle, text) in [
        (&second, second_add, "second only"),
        (&first, first_add, "first only"),
    ] {
        let result = callbacks.settle(handle).unwrap();
        assert_eq!(
            result["todos"],
            json!([{"id":0,"text":text,"done":false,"status":"pending"}])
        );
        assert_eq!(result["nextId"], 1);
    }
    assert!(first.show(first_owner)["text"]
        .as_str()
        .unwrap()
        .ends_with("first only"));
    assert!(second.show(second_owner)["text"]
        .as_str()
        .unwrap()
        .ends_with("second only"));
    assert!(second.show(std::ptr::null_mut())["error"].is_string());

    BRANCHES.with_borrow_mut(|branches| branches.get_mut(&1).unwrap().fail_restore = true);
    assert_eq!(first.reload(EventTag::SessionTree), 1);
    assert!(first.show(first_owner)["error"]
        .as_str()
        .unwrap()
        .contains("branch unavailable"));
    assert_eq!(second.invoke(json!({"action":"list"}))["nextId"], 1);

    let pending = first.execute(json!({"action":"clear"}));
    let first_registration = Arc::downgrade(&lifecycle::callback(first_owner).unwrap());
    first.begin();
    assert!(first.settle(pending).unwrap_err().contains("shutting down"));
    assert_eq!(
        second.invoke(json!({"action":"add","text":"still live"}))["nextId"],
        2
    );
    first.finish();
    assert!(first_registration.upgrade().is_none());
    assert!(first.show(first_owner)["error"]
        .as_str()
        .unwrap()
        .contains("runtime is unavailable"));

    let (status, third) = register_preview(3, false, false);
    assert_eq!(status, 0);
    assert_eq!(
        third.tool.unwrap().0 as usize,
        first.tool.unwrap().0 as usize
    );
    assert_eq!(third.invoke(json!({"action":"list"}))["nextId"], 0);
    assert_eq!(second.invoke(json!({"action":"list"}))["nextId"], 2);
    second.begin();
    third.begin();
    third.finish();
    second.finish();
    assert!(lifecycle::callback(second_owner).is_none());
}
