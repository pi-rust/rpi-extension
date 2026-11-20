"""Release comparison must work before the current version has a tag."""
import importlib.util
import pathlib
import shutil
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "release_baseline", pathlib.Path(__file__).with_name("select-release-baseline.py")
)
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)


@unittest.skipUnless(shutil.which("git"), "git required")
class ReleaseBaselineTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="rpi-baseline-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.git("init", "--initial-branch=main")
        self.git("config", "user.name", "Release test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("commit", "--allow-empty", "-m", "release baseline")
        for version in ["0.1.9", "0.1.10", "0.1.73", "0.1.75", "0.1.74-rc.1", "invalid"]:
            self.git("tag", f"rpi-web-v{version}")
        self.git("tag", "rpi-other-v99.0.0")

    def git(self, *args):
        return subprocess.check_output(
            ["git", *args], cwd=self.root, text=True, stderr=subprocess.PIPE
        ).strip()

    def test_existing_current_release_is_used(self):
        self.assertEqual(baseline.select_baseline("rpi-web", "0.1.73", self.root), "rpi-web-v0.1.73")

    def test_unpublished_current_version_uses_previous_release(self):
        # Future, prerelease, malformed and other-package tags must not win.
        self.assertEqual(baseline.select_baseline("rpi-web", "0.1.74", self.root), "rpi-web-v0.1.73")

    def test_versions_are_compared_numerically(self):
        self.assertEqual(baseline.select_baseline("rpi-web", "0.1.10", self.root), "rpi-web-v0.1.10")

    def test_unmerged_tag_is_not_a_release_baseline(self):
        tree = self.git("rev-parse", "HEAD^{tree}")
        unrelated = self.git("commit-tree", tree, "-m", "unrelated release")
        self.git("tag", "rpi-web-v0.1.74", unrelated)
        self.assertEqual(baseline.select_baseline("rpi-web", "0.1.74", self.root), "rpi-web-v0.1.73")

    def test_annotated_release_tag_is_supported(self):
        self.git("tag", "-a", "rpi-web-v0.1.74", "-m", "release")
        self.assertEqual(baseline.select_baseline("rpi-web", "0.1.74", self.root), "rpi-web-v0.1.74")

    def test_missing_baseline_fails_clearly(self):
        with self.assertRaisesRegex(ValueError, "fetch release tags/history"):
            baseline.select_baseline("missing", "0.1.74", self.root)
        with self.assertRaisesRegex(ValueError, "no reachable stable"):
            baseline.select_baseline("rpi-web", "0.1.8", self.root)

    def test_current_version_must_be_stable(self):
        with self.assertRaisesRegex(ValueError, "expected a stable"):
            baseline.select_baseline("rpi-web", "0.1.74-rc.1", self.root)


if __name__ == "__main__":
    unittest.main()
