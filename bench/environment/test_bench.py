"""Tests for evidence classification and VM ownership, without contacting exe.dev."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import host
import run


class BenchTests(unittest.TestCase):
    def test_failed_prerequisite_is_unverified_not_pass(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, "STATE", Path(tmp)):
            previous = host.RESULTS.copy()
            try:
                host.RESULTS["toolchains"] = host.Result("toolchains", "installed", host.Status.FAIL, "download failed")
                called = []
                host.case("rust-build", lambda: called.append(True), ["toolchains"])
                self.assertEqual(host.RESULTS["rust-build"].status, host.Status.UNVERIFIED)
                self.assertEqual(called, [])
                report = json.loads((Path(tmp) / "report.json").read_text())
                self.assertGreater(report["summary"]["unverified"], 0)
            finally:
                host.RESULTS.clear()
                host.RESULTS.update(previous)

    def test_wrong_outcome_is_failure(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, "STATE", Path(tmp)):
            previous = host.RESULTS.copy()
            try:
                def unexpected_access():
                    raise AssertionError("host reachable")
                host.case("root-boundary", unexpected_access)
                self.assertEqual(host.RESULTS["root-boundary"].status, host.Status.FAIL)
            finally:
                host.RESULTS.clear()
                host.RESULTS.update(previous)

    def test_vm_cleanup_requires_same_tag_and_incarnation(self):
        receipt = {"vm": "kuggis-bench-abc", "created_at": "first", "tag": "bench-abc"}
        good = {"vm_name": receipt["vm"], "created_at": "first", "tags": ["bench-abc"]}
        with patch.object(run, "edge", return_value=json.dumps({"vms": [good]}).encode()):
            self.assertEqual(run.owned(receipt), good)
        for entry in [dict(good, created_at="replacement"), dict(good, tags=[]), dict(good, vm_name="kuggis")]:
            with patch.object(run, "edge", return_value=json.dumps({"vms": [entry]}).encode()):
                with self.assertRaises(RuntimeError):
                    run.owned(receipt)


if __name__ == "__main__":
    unittest.main()
