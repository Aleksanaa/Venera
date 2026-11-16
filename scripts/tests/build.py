"""Exercise the build command's public command-line validation."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


BUILD = Path(__file__).resolve().parents[1] / "build"


class BuildCommandTests(unittest.TestCase):
    def test_installer_packages_socket_driver_and_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = BUILD.parent.parent
            for name in ("scripts/build", "scripts/fetch-viosock.sh",
                         "docs/third-party/viosock-LICENSE.txt",
                         "docs/third-party/viosock-lib-LICENSE.txt",
                         "docs/third-party/nlohmann-json-LICENSE.txt"):
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source / name, destination)
            for name in ("cmake/fetch-winkits.sh", "icd/build.sh"):
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_text("#!/bin/sh\nexit 0\n")
                destination.chmod(0o755)
            bindir = root / "bin"
            bindir.mkdir()
            commands = {
                "cmake": '''#!/bin/sh
mkdir -p build/win/setup
printf '1.2.3' > build/win/setup/version.txt
printf 'installer' > build/win/setup/venera-setup.exe
''',
                "xorriso": '''#!/usr/bin/env python3
import pathlib, sys
args = sys.argv[1:]
if '-extract' in args:
    for i, arg in enumerate(args):
        if arg == '-extract':
            path = pathlib.Path(args[i + 2])
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('upstream ' + pathlib.Path(args[i + 1]).name)
else:
    output = pathlib.Path(args[args.index('-o') + 1])
    output.write_text('\\n'.join(args[args.index('-o') + 2:]))
''',
                "curl": "#!/bin/sh\nexit 0\n",
                "openssl": "#!/bin/sh\nexit 0\n",
                "osslsigncode": "#!/bin/sh\nexit 0\n",
            }
            for name, contents in commands.items():
                command = bindir / name
                command.write_text(contents)
                command.chmod(0o755)
            cache = root / "build/viosock-cache"
            cache.mkdir(parents=True)
            with (cache / "virtio-win-0.1.302.iso").open("wb") as archive:
                archive.truncate(877373440)
            (cache / "microsoft-root-2010.crt").write_text("certificate")
            certificate = root / "signing.pfx"
            certificate.touch()
            env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}",
                       VENERA_KMD_SIGN_PFX=str(certificate))
            result = subprocess.run([str(root / "scripts/build"), "installer"],
                                    cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            package = root / "build/viosock/package"
            self.assertEqual(sorted(p.name for p in package.iterdir()), sorted([
                "viosock.inf", "viosock.cat", "viosock.sys", "viosocklib_x64.dll",
                "viosocklib_x86.dll", "viosockwspsvc.exe", "LICENSE.txt",
                "viosock-LICENSE.txt", "viosock-lib-LICENSE.txt"]))
            release = root / "build/release"
            self.assertEqual((release / "virtio-win-LICENSE.txt").read_text(),
                             "upstream virtio-win_license.txt")
            for notice in ("viosock-LICENSE.txt", "viosock-lib-LICENSE.txt",
                           "nlohmann-json-LICENSE.txt"):
                self.assertEqual((release / notice).read_bytes(),
                                 (source / "docs/third-party" / notice).read_bytes())
            self.assertEqual((release / "venera-setup-1.2.3.iso").read_text(),
                             "venera-setup-1.2.3.exe\nvirtio-win-LICENSE.txt\n"
                             "viosock-LICENSE.txt\nviosock-lib-LICENSE.txt\n"
                             "nlohmann-json-LICENSE.txt")

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
