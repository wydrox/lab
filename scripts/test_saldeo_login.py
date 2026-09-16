#!/usr/bin/env python3
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


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


if __name__ == "__main__":
    unittest.main()
