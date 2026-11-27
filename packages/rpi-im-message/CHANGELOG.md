# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Add `allowDirectChats` as an opt-in exemption from `allowChats` for confirmed
  Feishu `p2p` and WeCom `single` conversations.

### Fixed

- Exempt confirmed Feishu `p2p` conversations from `mentionRequired` so direct
  messages can arrive without a mention.

## [0.1.16](https://github.com/pi-rust/rpi-extension/compare/rpi-im-message-v0.1.15...rpi-im-message-v0.1.16) - 2026-11-25

### Added

- *(im-message)* add WeCom intelligent bot long connections ([#63](https://github.com/pi-rust/rpi-extension/pull/63))

### Added

- Support WeCom (企业微信) intelligent-bot long connections alongside Feishu/Lark,
  including Bot ID/Secret authentication, heartbeat/reconnect handling, passive
  Markdown/template-card replies, passive-reply fallback to active push,
  Markdown chunking, exponential reconnect backoff, duplicate Bot ID start
  protection, `msgid` de-duplication, group mention handling for commands,
  inbound text/voice/mixed extraction, per-provider session IDs and `wecom`
  profile validation/tests.

### Fixed

- Aggregate all WeCom Markdown chunk acknowledgements (including passive-reply
  fallbacks) before reporting delivery success, reject new sends at capacity
  without evicting pending deliveries, and ignore malformed acknowledgements.
- Reserve WeCom Bot IDs atomically before startup and retain the reservation
  through worker shutdown; request cleanup when startup times out.
- Bound WeCom writes with a shared multi-frame deadline, abandon poisoned
  transports and fail pending callers on write errors without automatic resend.
  Allow Stop during connection/authentication and handle worker exit with Stop
  queued. Only matching outstanding heartbeat acknowledgements reset liveness.
- Keep Feishu stream polling active during bounded REST requests; validate
  explicit API success codes, de-duplicate callbacks before side effects and
  ignore malformed events without poisoning connection state. Only text
  messages with content beyond mentions trigger Feishu automatic generation.
- Synchronize receive-state notifications with the queue wait, wake stopped
  receivers promptly after draining messages, and validate send timeouts before
  enqueueing either provider's externally visible send command.
- Reject non-object tool parameters and malformed action/profile/conversation
  types instead of silently defaulting to `start` or the configured destination;
  trim identifier parameters and configuration IDs. WeCom callbacks now require
  `msgid` and a group `chatid`; mention stripping no longer prefix-matches a
  longer mention key. Remove the unsupported Feishu `MEDIA:` prompt directive.
- Bound worker runtime shutdown so a blocked auto-reply task cannot make `stop`
  wait for the full model/process timeout; leaked blocking work may outlive the
  server but no longer delays shutdown.

## [0.1.15](https://github.com/pi-rust/rpi-package/compare/rpi-im-message-v0.1.14...rpi-im-message-v0.1.15) - 2026-11-06

### Fixed

- *(release)* publish musl builds for non-voice extensions ([#222](https://github.com/pi-rust/rpi-package/pull/222))

## [0.1.14](https://github.com/pi-rust/rpi-package/compare/rpi-im-message-v0.1.13...rpi-im-message-v0.1.14) - 2026-11-06

### Other

- honor per-crate musl support matrix ([#215](https://github.com/pi-rust/rpi-package/pull/215))

## [0.1.13](https://github.com/pi-rust/rpi-package/compare/rpi-im-message-v0.1.12...rpi-im-message-v0.1.13) - 2026-10-31

### Fixed

- *(tools)* clarify guidance and complete parameter schemas ([#135](https://github.com/pi-rust/rpi-package/pull/135))

## [0.1.12](https://github.com/pi-rust/rpi-package/compare/rpi-im-message-v0.1.11...rpi-im-message-v0.1.12) - 2026-10-14

### Other

- update Cargo.toml dependencies

## [0.1.11](https://github.com/pi-rust/rpi-package/compare/rpi-im-message-v0.1.10...rpi-im-message-v0.1.11) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
