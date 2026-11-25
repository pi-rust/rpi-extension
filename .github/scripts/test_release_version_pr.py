"""Exercise the version PR script using a local remote and a fake GitHub API."""

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


def git(directory, *args):
    return subprocess.check_output(["git", "-C", str(directory), *args], encoding="utf-8", stderr=subprocess.PIPE).strip()


def shell_path(path):
    value = pathlib.Path(path).resolve().as_posix()
    return "/" + value[0].lower() + value[2:] if os.name == "nt" else value


# Records only the required-check poll, so the script's merge path is exercised
# exactly like the release run: branch protection rejects merges until the
# `require-issue` context reports SUCCESS.
FAKE_GH = """#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
  'pr create') echo 'https://github.com/example/repo/pull/1' ;;
  'pr merge')
    if [[ "${CHECK_STATE:-SUCCESS}" != "SUCCESS" ]]; then
      echo "merge attempted before the required check passed" >&2
      exit 1
    fi
    tree="$(git rev-parse 'HEAD^{tree}')"
    parent="$(git ls-remote origin refs/heads/main | cut -f1)"
    merged="$(printf 'squashed version PR\\n' | git commit-tree "$tree" -p "$parent")"
    git push origin "$merged:refs/heads/main" >&2
    printf '%s\\n' "$merged" > "$FAKE_MERGED"
    ;;
  'pr view')
    case "$*" in
      *statusCheckRollup*)
        polls=0
        [[ -f "$FAKE_POLLS" ]] && polls="$(cat "$FAKE_POLLS")"
        polls=$((polls + 1))
        printf '%s\\n' "$polls" > "$FAKE_POLLS"
        # First poll reports a queued check with no conclusion yet.
        if ((polls == 1)); then
          printf '%s\\n' "${CHECK_STATE_PENDING:-}"
        else
          printf '%s\\n' "${CHECK_STATE:-SUCCESS}"
        fi
        ;;
      *) cat "$FAKE_MERGED" ;;
    esac
    ;;
  *) exit 99 ;;
esac
"""


@unittest.skipUnless(shutil.which("git") and shutil.which("bash"), "git and bash required")
class ReleaseVersionPrTests(unittest.TestCase):
    def run_script(self, root, check_state="SUCCESS"):
        repository = root / "checkout"
        repository.mkdir()
        git(repository, "init", "--initial-branch=main")
        git(repository, "config", "user.name", "Release test")
        git(repository, "config", "user.email", "test@example.invalid")
        (repository / "packages/demo").mkdir(parents=True)
        (repository / "catalog").mkdir()
        (repository / "Cargo.toml").write_text('[workspace.package]\nversion = "1.0.0"\n')
        (repository / "Cargo.lock").write_text("# fixture\n")
        (repository / "catalog/packages.json").write_text("[]\n")
        (repository / "packages/demo/Cargo.toml").write_text('[package]\nname = "demo"\n')
        git(repository, "add", ".")
        git(repository, "commit", "-m", "initial main")
        remote = root / "remote.git"
        subprocess.run(["git", "clone", "--bare", str(repository), str(remote)], check=True, capture_output=True)
        git(repository, "remote", "add", "origin", str(remote))
        original_main = git(repository, "rev-parse", "HEAD")
        (repository / "Cargo.toml").write_text('[workspace.package]\nversion = "1.0.1"\n')

        binary = root / "bin"
        binary.mkdir()
        gh = binary / "gh"
        gh.write_bytes(FAKE_GH.encode("utf-8"))
        gh.chmod(0o755)
        output = root / "outputs.txt"
        script = pathlib.Path(__file__).with_name("release-version-pr.sh")
        environment = dict(os.environ, GITHUB_RUN_ID="123", GITHUB_RUN_ATTEMPT="1",
                           GITHUB_OUTPUT=shell_path(output), RUNNER_TEMP=shell_path(root),
                           FAKE_MERGED=shell_path(root / "merged.txt"),
                           FAKE_POLLS=shell_path(root / "polls.txt"),
                           CHECK_STATE=check_state,
                           RELEASE_CHECK_TIMEOUT_SECONDS="60", RELEASE_CHECK_INTERVAL_SECONDS="1",
                           FIXTURE_BIN=shell_path(binary), RELEASE_SCRIPT=shell_path(script))
        result = subprocess.run(["bash", "-c", 'export PATH="$FIXTURE_BIN:$PATH"; bash "$RELEASE_SCRIPT"'],
                                cwd=repository, env=environment, capture_output=True, text=True)
        return repository, remote, output, original_main, result

    def test_merges_metadata_through_a_pr_and_checks_out_the_merged_commit(self):
        with tempfile.TemporaryDirectory(prefix="rpi-release-pr-test-") as temporary:
            root = pathlib.Path(temporary)
            repository, remote, output, original_main, result = self.run_script(root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            # The queued poll must precede the merge rather than the merge racing it.
            self.assertEqual((root / "polls.txt").read_text().strip(), "2")
            merged = git(repository, "rev-parse", "HEAD")
            self.assertNotEqual(merged, original_main)
            self.assertEqual(output.read_text().strip(), f"source_ref={merged}")
            self.assertEqual(git(remote, "rev-parse", "refs/heads/main"), merged)
            self.assertEqual(git(repository, "status", "--porcelain"), "")
            self.assertEqual(git(remote, "branch", "--list", "codex/release-*"), "")

    def test_a_failed_required_check_fails_the_run_without_merging(self):
        with tempfile.TemporaryDirectory(prefix="rpi-release-pr-failing-") as temporary:
            root = pathlib.Path(temporary)
            repository, remote, _, original_main, result = self.run_script(root, check_state="FAILURE")
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("require-issue ended as FAILURE", result.stdout + result.stderr)
            self.assertFalse((root / "outputs.txt").exists())
            self.assertEqual(git(remote, "rev-parse", "refs/heads/main"), original_main)
            # The version PR stays open for a human merge or a clean rerun.
            self.assertEqual(git(remote, "branch", "--list", "codex/release-*"), "codex/release-123-1")


if __name__ == "__main__":
    unittest.main()
