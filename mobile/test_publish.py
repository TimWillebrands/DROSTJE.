import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest


GITHUB_CLI = '''import json
import os
import pathlib
import sys

state_path = pathlib.Path("release.json")
state = json.loads(state_path.read_text())
args = sys.argv[1:]
if args[0] == "api":
    if os.environ.get("LOOKUP_FAILS"):
        sys.exit(4)
    if state["exists"]:
        print(123)
    sys.exit(0)
operation = args[1]
if operation == "create" and state["exists"]:
    sys.exit("Release already exists")
if operation == "upload" and not state["exists"]:
    sys.exit("Release does not exist")
files = [pathlib.Path(arg) for arg in args[3:] if pathlib.Path(arg).is_file()]
if any(file.name in state["assets"] for file in files):
    sys.exit("Asset already exists")
state["assets"].update({file.name: file.read_text() for file in files})
state["exists"] = True
if operation == "create":
    state["notes"] = "Generated release notes"
    state["prerelease"] = "--prerelease" in args
state_path.write_text(json.dumps(state))
'''


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.state = {"exists": False, "notes": "Existing release notes", "assets": {}}
        (self.root / "bin").mkdir()
        cli = self.root / "bin/gh"
        cli.write_text(f"#!{sys.executable}\n" + GITHUB_CLI)
        cli.chmod(0o755)
        (self.root / "dist").mkdir()
        (self.root / "dist/app.apk").write_text("signed APK fixture")
        (self.root / "dist/SHA256SUMS").write_text("checksum fixture")

    def publish(self, **extra_env):
        state_path = self.root / "release.json"
        state_path.write_text(json.dumps(self.state))
        env = dict(os.environ, PATH=f"{self.root / 'bin'}:/usr/bin:/bin",
                   GITHUB_REF_NAME="android-v0.1.0", GITHUB_REPOSITORY="owner/repo",
                   VERSION_NAME="0.1.0", PRERELEASE="false")
        env.update(extra_env)
        result = subprocess.run(
            ["/bin/bash", str(pathlib.Path(__file__).with_name("publish.sh").resolve())],
            cwd=self.root, env=env, capture_output=True, text=True, check=False,
        )
        return result, json.loads(state_path.read_text())

    def test_existing_empty_release_receives_assets_and_keeps_notes(self):
        self.state["exists"] = True
        result, state = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(state["notes"], self.state["notes"])
        self.assertEqual(state["assets"], {"app.apk": "signed APK fixture", "SHA256SUMS": "checksum fixture"})

    def test_new_release_is_created_with_assets(self):
        result, state = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(state["exists"])
        self.assertEqual(state["notes"], "Generated release notes")
        self.assertEqual(len(state["assets"]), 2)
        self.assertFalse(state["prerelease"])

    def test_failed_lookup_does_not_publish(self):
        result, state = self.publish(LOOKUP_FAILS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state, self.state)

    def test_new_release_candidate_is_a_prerelease(self):
        result, state = self.publish(PRERELEASE="true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(state["prerelease"])

    def test_existing_assets_are_never_replaced(self):
        self.state.update(exists=True, assets={"app.apk": "original signed APK"})
        result, state = self.publish()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state, self.state)


if __name__ == "__main__":
    unittest.main()
