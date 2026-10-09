#!/usr/bin/env python3
"""lab-automation.sh against a stub `lab` binary and a stub `osascript` in a temp HOME."""
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("lab-automation.sh")

STUB_LAB = r"""#!/usr/bin/env bash
printf 'LAB_NONINTERACTIVE=%s %s\n' "${LAB_NONINTERACTIVE-unset}" "$*" >> "$STUB_CALLS"
case "${STUB_MODE:-ok}" in
  expired)
    if [[ "$1" == sync ]]; then
      echo "Error: Saldeo session expired (LAB_NONINTERACTIVE=1); run lab onboard" >&2
      exit 1
    fi
    ;;
  fail)
    echo "Error: something else broke" >&2
    exit 3
    ;;
esac
# Like the binary: write the --output file (its mode shows the inherited umask).
prev=""
for arg in "$@"; do
  if [[ "$prev" == --output ]]; then
    printf '{"summary":{"uploaded_count":0,"failed_count":0}}\n' > "$arg"
  fi
  prev="$arg"
done
echo "stub $1 ok"
exit 0
"""

STUB_OSASCRIPT = r"""#!/usr/bin/env bash
# notify() passes: -e ... -e ... -e ... MESSAGE TITLE
args=("$@")
n=${#args[@]}
printf '%s|%s\n' "${args[n-1]}" "${args[n-2]}" >> "$NOTIFY_LOG"
"""


def write_exec(path, text):
    path.write_text(text)
    path.chmod(0o755)


class LabAutomationTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        root = Path(self._tmp.name)
        self.home = root / "home"
        self.home.mkdir()
        self.bin = root / "bin"
        self.bin.mkdir()
        self.log_dir = self.home / "Library" / "Logs" / "lab"
        self.calls = root / "calls.txt"
        self.notify_log = root / "notify.txt"
        write_exec(self.bin / "lab", STUB_LAB)
        write_exec(self.bin / "osascript", STUB_OSASCRIPT)
        self.env = {
            "PATH": f"{self.bin}:{os.environ.get('PATH', '/usr/bin:/bin')}",
            "HOME": str(self.home),
            "LAB_ROOT": str(root),
            "LAB_BIN": str(self.bin / "lab"),
            "LAB_YEAR": "2026",
            "LAB_LOCK_DIR": str(root / "lock" / "automation.lock"),
            "STUB_CALLS": str(self.calls),
            "NOTIFY_LOG": str(self.notify_log),
        }

    def tearDown(self):
        self._tmp.cleanup()

    def run_script(self, **extra):
        env = dict(self.env, **extra)
        # stdout is a pipe here, as under launchd.
        return subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True, timeout=60)

    def notifications(self):
        return self.notify_log.read_text().splitlines() if self.notify_log.exists() else []

    def test_success_writes_log_only_with_private_modes(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "", "log lines must not be duplicated to launchd stdout")
        log = (self.log_dir / "automation.log").read_text()
        self.assertIn("stub sync ok", log)
        self.assertIn("Done.", log)
        self.assertEqual(stat.S_IMODE(self.log_dir.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((self.log_dir / "automation.log").stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE((self.log_dir / "reconcile-2026.json").stat().st_mode) & 0o077, 0)
        self.assertTrue(self.notifications()[-1].startswith("LAB automation done|"))
        self.assertTrue(all(line.startswith("LAB_NONINTERACTIVE=1 ") for line in self.calls.read_text().splitlines()))

    def test_existing_world_readable_log_is_tightened(self):
        self.log_dir.mkdir(parents=True)
        self.log_dir.chmod(0o755)
        log = self.log_dir / "automation.log"
        log.write_text("old\n")
        log.chmod(0o644)
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(stat.S_IMODE(self.log_dir.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(log.stat().st_mode), 0o600)

    def test_session_expired_sends_login_notification(self):
        result = self.run_script(STUB_MODE="expired")
        self.assertEqual(result.returncode, 1, result.stderr)
        notes = self.notifications()
        self.assertEqual(len(notes), 1, notes)
        self.assertTrue(notes[0].startswith("LAB: Saldeo login needed|"), notes)
        self.assertIn("lab onboard", notes[0])
        self.assertIn("Saldeo session expired", (self.log_dir / "automation.log").read_text())

    def test_other_failure_sends_generic_notification(self):
        result = self.run_script(STUB_MODE="fail")
        self.assertEqual(result.returncode, 3, result.stderr)
        notes = self.notifications()
        self.assertEqual(len(notes), 1, notes)
        self.assertTrue(notes[0].startswith("LAB automation failed|"), notes)

    def test_explicit_noninteractive_setting_wins(self):
        result = self.run_script(LAB_NONINTERACTIVE="0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(line.startswith("LAB_NONINTERACTIVE=0 ") for line in self.calls.read_text().splitlines()))

    def rotate(self, size, old_files):
        self.log_dir.mkdir(parents=True)
        log = self.log_dir / "automation.log"
        log.write_bytes(b"x" * size)
        for i in old_files:
            Path(f"{log}.{i}").write_text(f"old{i}\n")
        script = f"""
set -euo pipefail
source {SCRIPT}
rotate_log
"""
        env = dict(self.env, LAB_LOG_DIR=str(self.log_dir))
        result = subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return log

    def test_rotation_over_limit_keeps_three(self):
        log = self.rotate(5 * 1024 * 1024 + 1, [1, 2, 3])
        self.assertEqual(Path(f"{log}.1").stat().st_size, 5 * 1024 * 1024 + 1)
        self.assertEqual(Path(f"{log}.2").read_text(), "old1\n")
        self.assertEqual(Path(f"{log}.3").read_text(), "old2\n")
        self.assertFalse(Path(f"{log}.4").exists())
        self.assertIn("Rotated automation.log", log.read_text())
        self.assertEqual(stat.S_IMODE(log.stat().st_mode), 0o600)

    def test_no_rotation_at_limit(self):
        log = self.rotate(5 * 1024 * 1024, [])
        self.assertEqual(log.stat().st_size, 5 * 1024 * 1024)
        self.assertFalse(Path(f"{log}.1").exists())


if __name__ == "__main__":
    unittest.main()
