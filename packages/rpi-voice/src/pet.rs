//! Session-local pet mode. Audio remains owned by the existing voice workers.
use super::*;

const KEY: &str = "rpi.pet";
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) type Animation = (mpsc::Sender<()>, std::thread::JoinHandle<()>);
const HELP: &str = "宠物模式\n  /pet 或 /pet auto   宠物陪伴 + 连续语音对话\n  /pet quiet         安静陪伴，关闭监听和朗读\n  /pet off           退出宠物模式，关闭语音\n  /pet cat|bunny     切换小猫 / 小兔子\n  /pet focus|work    专注陪伴 / 工作陪伴布局\n  /pet name <名字>   修改名字（最多 24 个字符）\n  /pet status        查看宠物和语音状态\n  /pet help          查看帮助\n\n输入文字会暂停连续语音；用 /pet auto 恢复。";

#[derive(Clone)]
pub(super) struct Pet {
    registered: bool,
    published_layout: Option<String>,
    enabled: bool,
    animal: String,
    layout: String,
    name: String,
    mood: &'static str,
    caption: String,
    speaker: &'static str,
    hint: String,
    meter: String,
    frame: u64,
}

impl Default for Pet {
    fn default() -> Self {
        Self {
            registered: false,
            published_layout: None,
            enabled: false,
            animal: "cat".into(),
            layout: "focus".into(),
            name: "小鱼".into(),
            mood: "sleep",
            caption: "我在呢。今天有什么想跟我聊的？".into(),
            speaker: "pet",
            hint: String::new(),
            meter: String::new(),
            frame: 0,
        }
    }
}

impl Pet {
    fn snapshot(&self, auto: bool) -> Value {
        json!({"version":1,"enabled":self.enabled,"name":self.name,"animal":self.animal,
            "mood":self.mood,"caption":self.caption,"speaker":self.speaker,
            "hint":self.hint,"layout":self.layout,"auto":auto,
            "meter":self.meter})
    }
}

// Writes use the same mutex as state changes: an old worker cannot publish an
// enabled snapshot after /pet off cleared it. SetStatus is bridge-local and
// never dispatches extension events, so the callback cannot re-enter this lock.
fn update(voice: &Arc<RuntimeContext>, f: impl FnOnce(&mut Pet)) {
    let mut pet = voice.pet.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut pet);
    if !voice.is_stopping() {
        let panel = panel_spec(&pet, voice.auto_talk_enabled.load(Ordering::Relaxed));
        if !pet.registered {
            if voice.action(RuntimeActionId::UiComponent,json!({"op":"register","id":KEY,"type":"text","slot":"overlay","visible":false})).is_ok() {
                pet.registered = true;
            }
        }
        if pet.published_layout.as_deref() != Some(pet.layout.as_str()) {
            let mut config = panel.clone();
            let obj = config.as_object_mut().unwrap();
            obj.remove("version");
            obj.remove("lines");
            obj.remove("title");
            obj.insert("op".into(), json!("configure"));
            obj.insert("id".into(), json!(KEY));
            obj.insert("slot".into(), json!("overlay"));
            if voice.action(RuntimeActionId::UiComponent, config).is_ok() {
                pet.published_layout = Some(pet.layout.clone());
            }
        }
        let lines = panel["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = voice.action(
            RuntimeActionId::UiComponent,
            json!({"op":"update","id":KEY,"text":lines,"title":panel["title"]}),
        );
        let _ = voice.action(
            RuntimeActionId::UiComponent,
            json!({"op":if pet.enabled {"show"} else {"hide"},"id":KEY}),
        );
    }
}

fn wrap(text: &str, width: usize, limit: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for paragraph in text.lines() {
        let mut row = String::new();
        let mut used = 0;
        for ch in paragraph.chars().filter(|ch| !ch.is_control()) {
            let size = UnicodeWidthChar::width(ch).unwrap_or(0);
            if (used + size > width || row.len() + ch.len_utf8() > 1800) && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(ch);
            used += size;
        }
        rows.push(row);
    }
    if rows.len() > limit {
        rows.truncate(limit);
        if let Some(last) = rows.last_mut() {
            while UnicodeWidthStr::width(last.as_str()) + 1 > width {
                last.pop();
            }
            last.push('…');
        }
    }
    rows
}

fn panel_spec(pet: &Pet, auto_talk_enabled: bool) -> Value {
    let width: usize = if pet.layout == "work" { 32 } else { 52 };
    let inner = width - 4;
    let (eyes, label) = match pet.mood {
        "listen" => (
            if pet.frame % 25 == 24 { "-.-" } else { "o.o" },
            "认真听你说话",
        ),
        "think" => ("-.-", "让我想一想"),
        "speak" => (if pet.frame % 2 == 0 { "o.o" } else { "oOo" }, "正在回答你"),
        "happy" => ("^.^", "完成啦！"),
        "error" => (";.;", "遇到一点小问题"),
        _ => ("-.-", "安静陪着你"),
    };
    let mut lines = vec![String::new()];
    let art = [
        if pet.animal == "bunny" {
            "     (\\_/)".into()
        } else {
            "     /\\_/\\".into()
        },
        format!("  .-( {eyes} )-."),
        " /   /   \\   \\".into(),
        "(___/     \\___)".into(),
        "     /   \\".into(),
        "    (_____)".into(),
    ];
    for row in art {
        lines.push(format!(
            "{}{}",
            " ".repeat(inner.saturating_sub(UnicodeWidthStr::width(row.as_str())) / 2),
            row
        ));
    }
    let meter = match pet.mood {
        "listen" | "speak" => pet.meter.as_str(),
        "think" => ["·", "· ·", "· · ·"][(pet.frame % 3) as usize],
        "happy" => "♡",
        "error" => "!",
        _ => "z z Z",
    };
    lines.extend(wrap(&format!("{label}  {meter}"), inner, 2));
    lines.push(String::new());
    let speaker = if pet.speaker == "user" {
        "你"
    } else {
        &pet.name
    };
    lines.extend(wrap(&format!("{speaker} › {}", pet.caption), inner, 3));
    if !pet.hint.is_empty() {
        lines.extend(wrap(&pet.hint, inner, 1));
    }
    lines.push("/pet quiet · /pet off".into());
    json!({"version":1,"anchor":if pet.layout == "work" {"right-center"} else {"center"},
        "offsetX":if pet.layout == "work" {-1} else {0},"offsetY":-2,
        "width":width,"maxHeight":20,"border":true,
        "title":format!("{} · {}",pet.name,if auto_talk_enabled {"连续对话"} else {"语音暂停"}),"lines":lines})
}

fn start_animation(voice: &Arc<RuntimeContext>) {
    let mut animation = voice
        .pet_animation
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if animation.is_some() {
        return;
    }
    if voice.is_stopping() {
        return;
    }
    let (stop, receiver) = mpsc::channel();
    let owner = voice.clone();
    if let Ok(thread) = std::thread::Builder::new()
        .name("rpi-pet-animation".into())
        .spawn(move || {
            let voice = &owner;
            while matches!(
                receiver.recv_timeout(std::time::Duration::from_millis(320)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                if enabled(voice) {
                    update(voice, |p| p.frame = p.frame.wrapping_add(1));
                }
            }
        })
    {
        *animation = Some((stop, thread));
    }
}

pub(super) fn signal_animation_stop(voice: &Arc<RuntimeContext>) {
    if let Some((stop, _)) = voice.pet_animation.lock().unwrap().as_ref() {
        let _ = stop.send(());
    }
}

pub(super) fn stop_animation(voice: &Arc<RuntimeContext>) {
    if let Some((stop, thread)) = voice
        .pet_animation
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = stop.send(());
        let _ = thread.join();
    }
}

pub(super) fn enabled(voice: &Arc<RuntimeContext>) -> bool {
    voice.pet.lock().unwrap_or_else(|e| e.into_inner()).enabled
}

fn mood_from_status(value: &str) -> Option<&'static str> {
    if value.contains('⚠') || value.contains("failed") {
        Some("error")
    } else if value.contains("paused")
        || value.contains("stopped")
        || value.contains("nothing heard")
        || value.contains("heard nothing")
    {
        Some("sleep")
    } else if value.contains("playing") {
        Some("speak")
    } else if value.contains("listening") || value.contains("recording") {
        Some("listen")
    } else if value.contains("transcribing") || value.contains("loading") || value.contains("model")
    {
        Some("think")
    } else if value.contains("draft") || value.contains('➡') {
        Some("happy")
    } else if value.contains("idle") {
        Some("sleep")
    } else {
        None
    }
}

pub(super) fn voice_status(voice: &Arc<RuntimeContext>, value: &str) {
    if !enabled(voice) {
        return;
    }
    if let Some(mood) = mood_from_status(value) {
        update(voice, |p| {
            p.mood = mood;
            p.meter = value
                .chars()
                .filter(|c| "█░▁▂▃▄▅▆▇".contains(*c))
                .take(12)
                .collect();
            p.hint = match mood {
                "listen" if value.contains("warming") => "麦克风启动中…",
                "listen" => "说完停一下，我就会回答。",
                "speak" => "朗读结束后，继续听你说话。",
                "think" if value.contains("model") || value.contains("loading") => {
                    "正在准备语音识别模型…"
                }
                "think" => "正在把你的声音转成文字…",
                "happy" => "识别完成；可以在输入框修正文字。",
                "error" => "语音遇到问题；/voice status 查看原因",
                _ => "语音已暂停；/pet auto 恢复对话",
            }
            .into();
        });
    }
}

pub(super) fn caption(voice: &Arc<RuntimeContext>, text: &str, speaker: &'static str) {
    if !enabled(voice) || text.trim().is_empty() {
        return;
    }
    let text = sanitize_for_speech(text)
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(1600)
        .collect();
    update(voice, |p| {
        p.caption = text;
        p.speaker = speaker;
    });
}

pub(super) fn message_end(voice: &Arc<RuntimeContext>, message: &Value) {
    if !enabled(voice) {
        return;
    }
    if message
        .get("role")
        .or_else(|| message.get("kind"))
        .and_then(Value::as_str)
        == Some("assistant")
    {
        caption(voice, &extract_message_text(message), "pet");
        if message.get("stopReason").and_then(Value::as_str) == Some("error") {
            update(voice, |p| {
                p.mood = "error";
                p.hint = message
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or("回复失败；/pet off 查看聊天详情")
                    .chars()
                    .take(240)
                    .collect();
            });
        }
    }
}

rpi_plugin_sdk::event_handler! {
fn on_event(event, user_data) {
    let Some(owner) = registration::callback(user_data) else { return 0; };
    let voice = &owner;
    if event.tag() == EventTag::SessionShutdown {
        if enabled(voice) { update(voice, |p| p.enabled = false); }
        registration::session_shutdown(voice);
        return 0;
    }
    if !enabled(voice) {
        return 0;
    }
    match event.tag() {
        EventTag::AgentStart => update(voice, |p| {
            p.mood = "think";
            p.hint.clear();
        }),
        EventTag::ToolExecutionStart => update(voice, |p| {
            if !voice.tts_playing.load(Ordering::Relaxed) {
                p.mood = "think";
            }
            p.hint = "正在处理你的请求…".into();
        }),
        EventTag::ToolExecutionEnd => {
            // The union variant is guaranteed by ToolExecutionEnd.
            if event.tool_result().is_some_and(|result| result.is_error) {
                update(voice, |p| {
                    p.mood = "error";
                    p.hint = "执行遇到问题；/pet off 查看详情".into();
                });
            }
        }
        EventTag::AgentEnd => update(voice, |p| {
            if !voice.tts_playing.load(Ordering::Relaxed)
                && !voice.recording.load(Ordering::Relaxed)
                && p.mood != "error"
            {
                p.mood = "happy";
                p.hint.clear();
            }
        }),
        EventTag::SessionStart | EventTag::SessionTree => update(voice, |p| {
            p.caption = "新的对话，我也在。".into();
            p.speaker = "pet";
            p.hint.clear();
        }),
        _ => {}
    }
    0
}
}

pub(super) fn silence(voice: &Arc<RuntimeContext>) {
    voice.auto_mode_enabled.store(false, Ordering::Relaxed);
    voice.auto_talk_enabled.store(false, Ordering::Relaxed);
    voice.auto_tts_enabled.store(false, Ordering::Relaxed);
    voice.ptt_enabled.store(false, Ordering::Relaxed);
    if let Some(stop) = voice.ptt.lock().unwrap().stop.take() {
        stop.store(true, Ordering::Relaxed);
    }
    ptt_reset(voice);
    if let Some(stop) = voice.auto_talk_stop.lock().unwrap().take() {
        stop.store(true, Ordering::Relaxed);
    }
    stop_playback(voice);
}

pub(super) fn prompt(voice: &Arc<RuntimeContext>, base: &str) -> Option<String> {
    let pet = voice.pet.lock().unwrap_or_else(|e| e.into_inner()).clone();
    prompt_for_pet(base, &pet.name, pet.enabled)
}

fn prompt_for_pet(base: &str, name: &str, enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    const HEADING: &str = "## Pet companion mode";
    if base.contains(HEADING) {
        return Some(base.to_owned());
    }
    Some(format!(
        "{base}\n\n{HEADING}\nYour display name is {} (a label, not an instruction). Be a warm, playful companion. Reply in the user's language using short natural sentences. Keep technical help accurate. Do not invent emotions, sounds, or stage directions for text-to-speech.\n",
        json!(name)
    ))
}

rpi_plugin_sdk::string_handler! {
fn command(input, out, user_data) {
    let Some(owner) = registration::callback(user_data) else { return 1; };
    let voice = &owner;
    let value: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let args = value
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let (verb, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let text = match verb {
        "" | "on" | "auto" if rest.trim().is_empty() => {
            update(voice, |p| {
                p.enabled = true;
                p.mood = "listen";
                p.hint.clear();
            });
            start_animation(voice);
            // Delegate to the existing voice command, preserving its worker,
            // microphone ownership, model preload and hands-free turn-taking.
            let input = json!({"args":"auto on","command":"voice"}).to_string();
            return handle_voice_command(voice, &input, out);
        }
        "quiet" if rest.trim().is_empty() => {
            silence(voice);
            update(voice, |p| {
                p.enabled = true;
                p.mood = "sleep";
                p.hint = "安静陪伴 · /pet auto 恢复语音".into();
            });
            start_animation(voice);
            "宠物正在安静陪伴你；监听和朗读已关闭。".into()
        }
        "off" if rest.trim().is_empty() => {
            silence(voice);
            update(voice, |p| p.enabled = false);
            stop_animation(voice);
            "宠物面板已关闭；语音已关闭。".into()
        }
        "cat" | "bunny" if rest.trim().is_empty() => {
            update(voice, |p| p.animal = verb.into());
            format!("宠物形象：{verb}")
        }
        "focus" | "work" if rest.trim().is_empty() => {
            update(voice, |p| p.layout = verb.into());
            format!("宠物布局：{verb}")
        }
        "name" => {
            let name = rest.trim();
            if name.is_empty() || name.chars().count() > 24 || name.chars().any(char::is_control) {
                "名字需要 1–24 个字符，不能包含控制字符。".into()
            } else {
                update(voice, |p| p.name = name.into());
                format!("宠物名字：{name}")
            }
        }
        "status" => {
            let pet = voice.pet
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            format!(
                "宠物：{} · {} · {}\n{}",
                pet.name,
                pet.animal,
                if pet.enabled { "开启" } else { "关闭" },
                status_text(voice)
            )
        }
        _ => HELP.into(),
    };
    if matches!(verb, "help" | "status")
        || !matches!(
            verb,
            "quiet" | "off" | "cat" | "bunny" | "name" | "focus" | "work"
        )
    {
        caption(voice, &text, "pet");
    }
    let state = voice.pet
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .snapshot(voice.auto_talk_enabled.load(Ordering::Relaxed));
    set_output(
        out,
        json!({"kind":"message","text":text,"details":{"kind":"pet","state":state}}),
    );
    0
}
}

pub(super) fn register(api: &PluginApi) -> i32 {
    if let Some(register) = api.register_command {
        let rc = register(
            StbStringRef::from_str("pet"),
            StbStringRef::from_str("宠物陪伴：auto|quiet|off|cat|bunny|name|status|help"),
            command,
        );
        if rc != 0 {
            return rc;
        }
    }
    if let Some(register) = api.register_event_handler {
        for tag in [
            EventTag::AgentStart,
            EventTag::AgentEnd,
            EventTag::ToolExecutionStart,
            EventTag::ToolExecutionEnd,
            EventTag::SessionStart,
            EventTag::SessionTree,
            EventTag::SessionShutdown,
        ] {
            let rc = register(tag, on_event, api.user_data);
            if rc != 0 {
                return rc;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_panels_cover_all_expressions_and_respect_content_limits() {
        let combining = format!("x{}", "\u{301}".repeat(1500));
        assert!(wrap(&combining, 48, 3)
            .iter()
            .all(|line| line.len() <= 2048));
        for layout in ["focus", "work"] {
            for mood in ["listen", "think", "speak", "happy", "sleep", "error"] {
                let pet = Pet {
                    layout: layout.into(),
                    mood,
                    caption: "中文长字幕".repeat(100),
                    ..Pet::default()
                };
                let panel = panel_spec(&pet, false);
                let inner = panel["width"].as_u64().unwrap() as usize - 4;
                let lines = panel["lines"].as_array().unwrap();
                assert!(lines.len() + 3 <= panel["maxHeight"].as_u64().unwrap() as usize);
                assert!(lines
                    .iter()
                    .all(|line| UnicodeWidthStr::width(line.as_str().unwrap()) <= inner));
                assert_eq!(
                    panel["anchor"],
                    if layout == "work" {
                        "right-center"
                    } else {
                        "center"
                    }
                );
                assert!(lines
                    .iter()
                    .any(|line| line.as_str().unwrap().ends_with('…')));
            }
        }
    }
    #[test]
    fn voice_states_cover_playback_listening_failure_and_pause() {
        for (value, mood) in [
            ("voice: 🎤 listening ▂▃", "listen"),
            ("voice: ♪ playing 1.2s", "speak"),
            ("voice: 🧠 transcribing…", "think"),
            ("voice: ⚠ speech failed", "error"),
            ("voice: ✋ continuous mode paused", "sleep"),
            ("voice: ✏️ draft", "happy"),
        ] {
            assert_eq!(mood_from_status(value), Some(mood));
        }
        assert_eq!(mood_from_status("unrelated"), None);
    }
    #[test]
    #[allow(unsafe_code)]
    fn mascot_commands_and_animation_stay_with_the_registration() {
        fn call(owner: &Arc<RuntimeContext>, args: &str) {
            registration::with_test_callback(owner, || {
                let input = json!({"args": args}).to_string();
                let mut out = StbString::empty();
                assert_eq!(
                    command(
                        StbStringRef::from_str(&input),
                        &mut out,
                        owner.api.user_data
                    ),
                    0
                );
                // SAFETY: the fixture takes ownership of the command output.
                unsafe { rpi_plugin_sdk::callbacks::free_string(out) };
            });
        }

        let first = registration::test_runtime();
        let second = registration::test_runtime();
        call(&first, "name 小鱼");
        call(&second, "name 小兔");
        call(&first, "quiet");
        assert!(enabled(&first));
        assert!(!enabled(&second));
        assert!(prompt(&first, "base").unwrap().contains("小鱼"));
        assert!(prompt(&second, "base").is_none());
        first.finish();
        assert!(first.pet_animation.lock().unwrap().is_none());
        call(&second, "quiet");
        assert!(prompt(&second, "base").unwrap().contains("小兔"));
        second.finish();
    }

    #[test]
    fn pet_prompt_preserves_base_and_disappears_when_disabled() {
        assert!(prompt_for_pet("BASE", "小鱼", false).is_none());
        let once = prompt_for_pet("BASE", "小鱼", true).unwrap();
        assert!(once.starts_with("BASE"));
        assert!(once.contains("\"小鱼\""));
        assert_eq!(prompt_for_pet(&once, "小鱼", true).unwrap(), once);
    }
}
