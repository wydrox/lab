#!/usr/bin/env python3
import json
import os
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("saldeo-login.js")

# Stub `playwright`: the first launch fails (main Helium profile busy), so the script
# falls back to a temporary `lab-helium-*` profile; STUB_MODE picks what happens next.
STUB_PLAYWRIGHT = r"""
const fs = require("fs");
const path = require("path");
let launches = 0;
exports.chromium = {
  async launchPersistentContext(dir) {
    launches += 1;
    if (launches === 1) throw new Error("profile in use");
    fs.writeFileSync(path.join(dir, "Cookies"), "saldeo-session");
    const mode = process.env.STUB_MODE;
    const page = {
      async goto() {
        if (mode === "hang") return new Promise(() => {});
        if (mode === "error") throw new Error("navigation failed");
      },
      async waitForLoadState() {},
    };
    return {
      pages: () => [page],
      async newPage() { return page; },
      once() {},
      async close() {
        fs.writeFileSync(process.env.STUB_CLOSE_MARK, fs.existsSync(dir) ? "dir-present" : "dir-gone");
      },
      async storageState(opts) {
        if (opts && opts.path) fs.writeFileSync(opts.path, "{}");
        const cookies = mode === "ok"
          ? [{ name: "X-SALDEO-XSRF-C-TOKEN", value: "x", domain: "saldeo.brainshare.pl", path: "/", expires: -1, secure: true }]
          : [];
        return { cookies };
      },
      request: {
        async post() {
          return { status: () => 200, text: async () => '{"status":"SUCCESS","data":{"resultCollection":[]}}' };
        },
      },
    };
  },
};
"""


def run_node(js):
    return subprocess.run(["node", "-e", js], capture_output=True, text=True)


class SaldeoLoginTests(unittest.TestCase):
    def test_read_login_file_and_unlink(self):
        script = Path(__file__).with_name("saldeo-login.js")
        with tempfile.TemporaryDirectory() as tmp:
            login = Path(tmp) / "login.json"
            login.write_text(json.dumps({"username": "jan", "password": "tajne-haslo"}))
            os.chmod(login, 0o600)
            js = f"""
const {{ readLoginFile }} = require({json.dumps(str(script))});
const data = readLoginFile({json.dumps(str(login))});
if (data.username !== "jan" || data.password !== "tajne-haslo") process.exit(2);
"""
            result = subprocess.run(["node", "-e", js], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(login.exists())

    def test_script_does_not_use_argv_for_password(self):
        text = Path(__file__).with_name("saldeo-login.js").read_text()
        self.assertIn("LAB_SALDEO_LOGIN_FILE", text)
        self.assertNotIn("process.argv[", text)
        self.assertNotIn("SALDEO_PASSWORD", text)

    def test_password_login_waits_for_spa_and_is_headed_by_default(self):
        text = Path(__file__).with_name("saldeo-login.js").read_text()
        self.assertIn("waitAndFillLogin", text)
        self.assertIn("SALDEO_AUTH_HEADLESS === \"1\"", text)
        self.assertNotIn("Boolean(login)", text)
        self.assertIn("waitForLoadState", text)


class SaldeoLoginPureTests(unittest.TestCase):
    def node_json(self, expr):
        js = f"""
const m = require({json.dumps(str(SCRIPT))});
process.stdout.write(JSON.stringify({expr}));
"""
        result = run_node(js)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_cookies_for_url_follow_domain_path_secure_rules(self):
        cookies = [
            {"name": "host", "value": "1", "domain": "saldeo.brainshare.pl", "path": "/", "secure": True, "expires": -1},
            {"name": "parent", "value": "1", "domain": ".brainshare.pl", "path": "/", "secure": False, "expires": -1},
            {"name": "rest", "value": "1", "domain": "saldeo.brainshare.pl", "path": "/rest", "secure": False, "expires": -1},
            {"name": "other", "value": "1", "domain": ".google.com", "path": "/", "secure": False, "expires": -1},
            {"name": "old", "value": "1", "domain": "saldeo.brainshare.pl", "path": "/", "secure": False, "expires": 10},
        ]
        names = lambda url: self.node_json(
            f"m.cookiesForUrl({json.dumps(cookies)}, {json.dumps(url)}, 1000).map((c) => c.name)"
        )
        self.assertEqual(names("https://saldeo.brainshare.pl/rest/client/document/list/search"), ["host", "parent", "rest"])
        self.assertEqual(names("https://cdn.brainshare.pl/f.pdf"), ["parent"])
        self.assertEqual(names("https://files.example.net/f.pdf"), [])
        self.assertEqual(names("http://saldeo.brainshare.pl/restore"), ["parent"])

    def test_session_response_valid(self):
        cases = {
            "ok": [200, '{"status":"SUCCESS","data":{"resultCollection":[]}}'],
            "no_status": [200, '{"data":{"resultCollection":[]}}'],
            "error_status": [200, '{"status":"ERROR","data":{"resultCollection":[]}}'],
            "html": [200, "<html><form>login</form></html>"],
            "unauthorized": [401, '{"status":"SUCCESS","data":{"resultCollection":[]}}'],
        }
        got = self.node_json(
            "Object.fromEntries(Object.entries(" + json.dumps(cases) + ").map(([k, v]) => [k, m.sessionResponseValid(v[0], v[1])]))"
        )
        self.assertEqual(got, {"ok": True, "no_status": True, "error_status": False, "html": False, "unauthorized": False})

    def test_sweep_removes_only_stale_lab_profiles(self):
        with tempfile.TemporaryDirectory() as tmp:
            stale = Path(tmp) / "lab-helium-old"
            fresh = Path(tmp) / "lab-helium-new"
            unrelated = Path(tmp) / "other-old"
            for d in (stale, fresh, unrelated):
                d.mkdir()
            old = time.time() - 7 * 3600
            os.utime(stale, (old, old))
            os.utime(unrelated, (old, old))
            removed = self.node_json(f"m.sweepStaleTempProfiles({json.dumps(tmp)})")
            self.assertEqual(removed, [str(stale)])
            self.assertFalse(stale.exists())
            self.assertTrue(fresh.exists())
            self.assertTrue(unrelated.exists())


class SaldeoLoginTempProfileTests(unittest.TestCase):
    """Runs the script as `lab` does, with a stub playwright, and checks that the
    temporary profile is gone on every exit path."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        root = Path(self._tmp.name)
        self.tmpdir = root / "tmp"
        self.tmpdir.mkdir()
        self.home = root / "home"
        self.home.mkdir()
        stub = root / "node_modules" / "playwright"
        stub.mkdir(parents=True)
        (stub / "index.js").write_text(STUB_PLAYWRIGHT)
        self.close_mark = root / "closed"
        self.out = root / "state.json"
        self.env = {
            "PATH": os.environ.get("PATH", ""),
            "HOME": str(self.home),
            "TMPDIR": str(self.tmpdir),
            "NODE_PATH": str(root / "node_modules"),
            "LAB_SALDEO_STORAGE_STATE": str(self.out),
            "HELIUM_EXECUTABLE": "/nonexistent/helium",
            "STUB_CLOSE_MARK": str(self.close_mark),
        }

    def tearDown(self):
        self._tmp.cleanup()

    def profiles(self):
        return sorted(p.name for p in self.tmpdir.iterdir() if p.name.startswith("lab-helium-"))

    def run_script(self, mode, timeout_ms="5000", timeout=30):
        env = dict(self.env, STUB_MODE=mode, SALDEO_AUTH_TIMEOUT_MS=timeout_ms)
        return subprocess.run(["node", str(SCRIPT)], env=env, capture_output=True, text=True, timeout=timeout)

    def test_success_removes_temp_profile(self):
        result = self.run_script("ok")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("lab-helium-", result.stderr)
        self.assertTrue(self.out.exists())
        self.assertEqual(self.close_mark.read_text(), "dir-present")
        self.assertEqual(self.profiles(), [])

    def test_error_removes_temp_profile(self):
        result = self.run_script("error")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("navigation failed", result.stderr)
        self.assertEqual(self.profiles(), [])

    def test_no_session_until_deadline_removes_temp_profile(self):
        result = self.run_script("nosession", timeout_ms="100")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.profiles(), [])

    def test_watchdog_timeout_removes_temp_profile(self):
        # Navigation hangs; the watchdog fires at timeout + 10 s, before `lab` would SIGKILL.
        start = time.monotonic()
        result = self.run_script("hang", timeout_ms="100")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertLess(time.monotonic() - start, 20)
        self.assertEqual(self.close_mark.read_text(), "dir-present")
        self.assertEqual(self.profiles(), [])

    def test_signal_removes_temp_profile(self):
        for sig, code in ((signal.SIGTERM, 143), (signal.SIGINT, 130), (signal.SIGHUP, 129)):
            env = dict(self.env, STUB_MODE="hang", SALDEO_AUTH_TIMEOUT_MS="60000")
            proc = subprocess.Popen(["node", str(SCRIPT)], env=env, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 10
                while not self.profiles() and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(self.profiles(), "temporary profile was not created")
                time.sleep(0.2)
                proc.send_signal(sig)
                _, stderr = proc.communicate(timeout=15)
            finally:
                if proc.poll() is None:
                    proc.kill()
            self.assertEqual(proc.returncode, code, stderr)
            self.assertEqual(self.profiles(), [], sig)


if __name__ == "__main__":
    unittest.main()
