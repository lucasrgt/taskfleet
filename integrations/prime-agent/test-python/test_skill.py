import asyncio
import importlib.util
import json
import os
import sys
import types
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch

rlm = types.ModuleType("rlm")
rlm.McpIntegration = type("McpIntegration", (), {})
sys.modules["rlm"] = rlm
path = Path(__file__).parents[1] / "skills/taskfleet/src/taskfleet/__init__.py"
spec = importlib.util.spec_from_file_location("taskfleet_skill", path)
skill = importlib.util.module_from_spec(spec)
spec.loader.exec_module(skill)


class Process:
    def __init__(self, output, code=0):
        self.output, self.returncode = output, code
    async def communicate(self): return self.output, b"failure"
    def kill(self): pass
    async def wait(self): return self.returncode


class LocateTests(unittest.TestCase):
    def test_locator_delegates_config_and_validates_enabled_result(self):
        location = {"mode": "external", "repository": "/repo", "config": "/state/taskfleet.toml", "state": "/state", "enabled": True}
        spawn = AsyncMock(return_value=Process(json.dumps(location).encode()))
        with patch.dict(os.environ, {"TASKFLEET_CONFIG": "/explicit.toml"}, clear=True), patch.object(skill.asyncio, "create_subprocess_exec", spawn):
            self.assertEqual(asyncio.run(skill._locate("/bin/taskfleet")), location)
        args, kwargs = spawn.call_args
        self.assertEqual(args, ("/bin/taskfleet", "locate", "--config", "/explicit.toml"))
        self.assertEqual(kwargs["cwd"], os.getcwd())

    def test_locator_fails_closed_for_disabled_malformed_and_nonzero_results(self):
        cases = [
            (Process(b'{"mode":"external","enabled":false}'), "not enabled"),
            (Process(b"not-json"), "invalid JSON"),
            (Process(b"", 1), "locate failed"),
        ]
        for process, message in cases:
            with self.subTest(message=message), patch.dict(os.environ, {}, clear=True), patch.object(skill.asyncio, "create_subprocess_exec", AsyncMock(return_value=process)):
                with self.assertRaisesRegex(RuntimeError, message): asyncio.run(skill._locate("taskfleet"))


if __name__ == "__main__": unittest.main()
