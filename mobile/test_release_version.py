import pathlib
import subprocess
import sys
import tempfile
import unittest

from release_version import release_version


class ReleaseVersionTests(unittest.TestCase):
    def test_stable_release(self):
        self.assertEqual(release_version("android-v1.2.3"), {
            "version_name": "1.2.3",
            "version_code": 10_200_399,
            "prerelease": "false",
        })

    def test_candidates_upgrade_to_stable_then_next_patch(self):
        tags = ["android-v0.1.0-rc.1", "android-v0.1.0-rc.98", "android-v0.1.0",
                "android-v0.1.1-rc.1"]
        versions = [release_version(tag) for tag in tags]
        codes = [version["version_code"] for version in versions]
        self.assertEqual(codes, sorted(set(codes)))
        self.assertEqual(versions[0]["version_name"], "0.1.0-rc.1")
        self.assertEqual(versions[0]["prerelease"], "true")

    def test_component_rollovers_preserve_update_order(self):
        tags = ["android-v0.0.999", "android-v0.1.0-rc.1", "android-v0.99.999",
                "android-v1.0.0-rc.1"]
        codes = [release_version(tag)["version_code"] for tag in tags]
        self.assertEqual(codes, sorted(set(codes)))

    def test_largest_version_stays_within_android_limit(self):
        self.assertLessEqual(release_version("android-v209.99.999")["version_code"], 2_100_000_000)

    def test_invalid_tags_are_rejected(self):
        for tag in ["v1.2.3", "android-v1.2", "android-v01.2.3", "android-v1.02.3",
                    "android-v1.2.03", "android-v1.2.3-beta.1", "android-v1.2.3+build",
                    "android-v1.2.3-rc.0", "android-v1.2.3-rc.01", "android-v1.2.3-rc.99",
                    "android-v1.2.3-rc.100", "android-v210.0.0", "android-v1.100.0",
                    "android-v1.0.1000", "android-v1.2.3\ninjected=true"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release_version(tag)

    def test_cli_produces_workflow_outputs(self):
        result = self.run_cli("android-v0.1.0")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "version_name=0.1.0\nversion_code=100099\nprerelease=false\n")

    def test_invalid_cli_produces_no_workflow_outputs(self):
        for arguments in [[], ["android-v1.2.3", "extra"], ["invalid"]]:
            with self.subTest(arguments=arguments):
                result = self.run_cli(*arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def run_cli(self, *arguments):
        return subprocess.run(
            [sys.executable, str(pathlib.Path(__file__).with_name("release_version.py")), *arguments],
            capture_output=True, text=True, check=False,
        )


class BuildHelpTests(unittest.TestCase):
    def test_help_needs_no_android_configuration_and_leaves_directory_untouched(self):
        script = pathlib.Path(__file__).with_name("build.sh").resolve()
        with tempfile.TemporaryDirectory() as directory:
            outputs = []
            for flag in ["--help", "-h"]:
                result = subprocess.run(
                    ["/bin/bash", str(script), flag],
                    cwd=directory, env={"PATH": "/usr/bin:/bin"},
                    capture_output=True, text=True, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")
                self.assertIn("Usage: bash mobile/build.sh", result.stdout)
                outputs.append(result.stdout)
            self.assertEqual(outputs[0], outputs[1])
            self.assertEqual(list(pathlib.Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
