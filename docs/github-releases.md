# GitHub-only extension releases

`pi-rust/rpi-extension` imports the source tree from `pi-rust/rpi-package`. It
is the crates.io publisher for these extensions, so the canonical release entry
is **Version and publish crates** (`release-cargo.yml`). This document covers the
GitHub-only backfill entry (**Publish GitHub extensions**), which needs no
crates.io token and produces the same verified prebuilt assets for already
versioned packages.

## Publish

Open **Actions → Publish GitHub extensions → Run workflow** on `main`.
Set `packages` to `all` (the catalog's prebuilt extensions) or comma-separated
names such as `rpi-todo,rpi-webfetch`. The source is the exact main commit at
workflow dispatch time. `source_ref` may specify an earlier main-history commit
to retry an existing version; unmerged source is rejected.

```sh
gh workflow run release-github.yml --repo pi-rust/rpi-extension --ref main \
  -f packages=all
```

1. Validate catalog names, native library targets and repository metadata.
2. Check **all** selected version tags before mutation. Existing tags must point
   at the exact source commit; tags are never moved. Create missing tags and
   draft package releases (`<package>-v<version>`).
3. Reuse `release-packages.yml` to build Windows x64, Linux GNU x64/ARM64,
   macOS Intel/Apple Silicon and the crate-declared Linux x64 musl targets.
   GNU-only packages are intentionally excluded from musl. Web selections build
   and embed their frontend before compiling native libraries.
4. Verify platforms, source, SDK ABI, archive and library checksums. Upload
   archives first, then each `rpi-extension.json` discovery manifest.
5. Publish drafts only after every supported platform succeeds. A failed build
   leaves drafts unpublished; retry the same source, not a moved version tag.
   Previously published manifests/assets are preserved.

This workflow needs only GitHub's built-in `GITHUB_TOKEN` with Contents write
permission. It does not use `CARGO_REGISTRY_TOKEN`. Do not run it for versions
that **Version and publish crates** is releasing in the same batch: release-plz
owns those tags. The crates.io flow uses `release-plz`, which is why
`release-cargo.yml` runs only on this repository and is gated by the `mode`
input (`dry-run` by default).

To publish changed package code, bump its Cargo version (or the inherited
workspace version), run `python3 .github/scripts/sync-release-catalog.py`, commit
the synchronized catalog and push. Version tags are immutable. No automatic
crates.io version bumping is performed here.

## Install from this repository

Use the explicit repository so crates.io metadata cannot redirect discovery to
upstream. For a known release, pin the exact version:

```sh
rpi install rpi-todo --github pi-rust/rpi-extension --version <published-version>
```

The [catalog](../catalog/packages.json) contains the repository-specific commands
and discovery URLs. Those URLs become available only after the corresponding
release completes. Historical changelog and upstream evidence links are not
rewritten. See [the distribution protocol](prebuilt-distribution.md) for schema
and ABI details.

## Optional services and credentials

- **RustFS/CDN**: absent configuration skips mirroring; GitHub is canonical.
  Optionally set all four `RPI_RUSTFS_ENDPOINT`, `RPI_RUSTFS_BUCKET`,
  `RPI_RUSTFS_ACCESS_KEY`, `RPI_RUSTFS_SECRET_KEY`, plus `RPI_CDN_BASE_URL` if
  desired. Partial RustFS configuration is invalid. Existing upstream CDN
  object keys are shared by the imported scripts; do not reuse an upstream
  bucket without isolating its destination to prevent immutable-object clashes.
- **Antigravity OAuth**: `RPI_ANTIGRAVITY_OAUTH_CLIENT_ID` and
  `RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET` are optional build inputs for
  `rpi-providers`; without them that OAuth provider login is unavailable. These
  are **not** Android signing secrets.
- **Signed Android APKs**: `rpi-web-android-release` is configured with a
  server-side deployment branch rule allowing only branch `main` (not tags).
  Signed manual builds still require seven environment secrets:
  `RPI_WEB_ANDROID_KEYSTORE_BASE64`, `RPI_WEB_ANDROID_STORE_PASSWORD`,
  `RPI_WEB_ANDROID_KEY_ALIAS`, `RPI_WEB_ANDROID_KEY_PASSWORD`,
  `RPI_WEB_ANDROID_CERT_SHA256`, `RPI_WEB_ANDROID_DEFAULT_ORIGIN`,
  `RPI_WEB_ANDROID_DEVELOPMENT_ORIGIN`. GitHub cannot export the source secrets;
  the original keystore must be supplied to preserve signature compatibility.
  No replacement signing key is generated. Android push/PR checks do not need
  signing credentials.

## Validation

Use Python 3.11+ (CI pins 3.12) and Bash 4+ for the musl script fixture:

```sh
python3 -m unittest discover -s .github/scripts -p 'test_*.py' -v
python3 .github/scripts/github-release.py plan --repository pi-rust/rpi-extension
```

`plan` prints selection/source metadata without creating releases or tags.
**Release tooling checks** runs the tests and verifies catalog synchronization
on CI. Source import does not copy old Git history, tags, releases or secrets;
the GitHub-only path needs none of the upstream release-plz history baselines.
