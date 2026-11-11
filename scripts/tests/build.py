"""Exercise the build command's public command-line validation."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


BUILD = Path(__file__).resolve().parents[1] / "build"


class BuildCommandTests(unittest.TestCase):
    def test_help_from_another_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([str(BUILD), "--help"], cwd=directory,
                                    capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn("Usage: nix develop -c scripts/build [all|installer|workbench|check]",
                      result.stdout)

    def test_unknown_target(self):
        result = subprocess.run([str(BUILD), "unknown"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("Unknown build target: unknown\n", result.stderr)

    def test_installer_certificate_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            for certificate in (None, f"{directory}/missing.pfx"):
                env = dict(os.environ)
                env.pop("VENERA_KMD_SIGN_PFX", None)
                if certificate is not None:
                    env["VENERA_KMD_SIGN_PFX"] = certificate
                for arguments in ([], ["installer"], ["all"]):
                    with self.subTest(certificate=certificate, arguments=arguments):
                        result = subprocess.run([str(BUILD), *arguments], cwd=directory,
                                                env=env, capture_output=True, text=True)
                        self.assertEqual(result.returncode, 1)
                        self.assertEqual(result.stderr,
                                         "Set VENERA_KMD_SIGN_PFX to an existing test-signing PFX file.\n")
                        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
