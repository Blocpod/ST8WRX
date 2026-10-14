"""Exercise worker supervision with a fake binary; no wallet or database access."""

import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest


SCRIPT = Path(__file__).resolve().with_name("run-anchor-worker.sh")


class AnchorWorkerTests(unittest.TestCase):
    def test_configuration_is_required(self):
        for queue, extra in [
            ("invalid", {}),
            ("run-once", {}),
            ("run-compute-once", {"DATABASE_URL": "unused"}),
            ("run-once", {"DATABASE_URL": "unused", "ST8_WALLET_URL": "unused",
                          "ST8_ANCHOR_POLL_SECONDS": "0"}),
            ("run-once", {"DATABASE_URL": "unused", "ST8_WALLET_URL": "unused",
                          "ST8_ANCHOR_POLL_SECONDS": "00"}),
        ]:
            with self.subTest(queue=queue, extra=extra):
                result = subprocess.run(["sh", str(SCRIPT), queue],
                                        env={"PATH": os.environ["PATH"], **extra},
                                        capture_output=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)

    def test_both_queues_retry_failed_attempts_and_stop(self):
        for queue in ["run-once", "run-compute-once"]:
            with self.subTest(queue=queue), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                calls = root / "calls"
                binary = root / "st8-anchor-worker"
                binary.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\nexit 1\n')
                binary.chmod(0o755)
                env = {**os.environ, "PATH": directory + os.pathsep + os.environ["PATH"],
                       "DATABASE_URL": "unused", "ST8_WALLET_URL": "unused",
                       "ST8_ANCHOR_POLL_SECONDS": "1", "CALLS": str(calls)}
                process = subprocess.Popen(["sh", str(SCRIPT), queue], env=env,
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        if calls.exists() and len(calls.read_text().splitlines()) >= 2:
                            break
                        time.sleep(0.05)
                    self.assertGreaterEqual(len(calls.read_text().splitlines()), 2)
                    self.assertEqual(set(calls.read_text().splitlines()),
                                     {queue + " --require-mined true"})
                    process.terminate()
                    self.assertEqual(process.wait(timeout=3), 0)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait()

    def test_stop_terminates_active_attempt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pid_file = root / "pid"
            binary = root / "st8-anchor-worker"
            binary.write_text('#!/bin/sh\nprintf "%s" "$$" > "$CHILD_PID"\nexec sleep 30\n')
            binary.chmod(0o755)
            env = {**os.environ, "PATH": directory + os.pathsep + os.environ["PATH"],
                   "DATABASE_URL": "unused", "ST8_WALLET_URL": "unused",
                   "CHILD_PID": str(pid_file)}
            process = subprocess.Popen(["sh", str(SCRIPT), "run-once"], env=env)
            try:
                deadline = time.monotonic() + 5
                while not pid_file.exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                child = int(pid_file.read_text())
                process.terminate()
                self.assertEqual(process.wait(timeout=3), 0)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child, 0)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=3)


if __name__ == "__main__":
    unittest.main()
