# Package catalog metadata

[`packages.json`](packages.json) is the extension discovery catalog. Each entry
keeps its stable Cargo package ID in `name` and includes bilingual card metadata:

```json
{
  "name": "rpi-acp",
  "nameZh": "rpi-acp",
  "nameEn": "rpi-acp",
  "descriptionZh": "通过 ACP 将 rpi 接入 Zed 等客户端，支持流式回复、工具交互和会话管理。",
  "descriptionEn": "Use rpi in Zed and other ACP clients, with streamed replies, tool interactions and session management.",
  "description": "Use rpi in Zed and other ACP clients, with streamed replies, tool interactions and session management."
}
```

- `name` is the machine-readable package ID; localization must not change it.
- `nameZh` / `nameEn` are display names. They currently retain the recognizable
  package ID in both languages, as in the example above.
- `descriptionZh` / `descriptionEn` are concise Chinese and English descriptions
  of implemented behavior, reviewed against each package's README.
- `description` mirrors `descriptionEn` for consumers that still read the legacy
  field. Language-aware consumers can select the localized fields, with `name`
  and `description` as compatibility fallbacks.

When adding an entry or changing its capabilities, maintain both descriptions.
Do not advertise planned behavior, sandboxing, offline operation or automatic
execution unless the package actually provides it.

`python3 .github/scripts/sync-release-catalog.py` updates release metadata but
preserves these curated names and descriptions. Run the regression checks with:

```sh
python3 -m unittest discover -s .github/scripts -p test_sync_release_catalog.py -v
```

The tests require non-empty bilingual metadata on every catalog entry and verify
that synchronization preserves it across repeated runs. This schema does not
change install commands, versions, release URLs or frontend rendering logic.
