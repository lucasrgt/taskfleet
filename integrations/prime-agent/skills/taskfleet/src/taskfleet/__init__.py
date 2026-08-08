"""Prime Agent's thin MCP stdio adapter for the Taskfleet binary."""

import asyncio
import json
import os
import shutil
from contextlib import AsyncExitStack

from rlm import McpIntegration


async def _locate(command):
    args = [command, "locate"]
    if os.environ.get("TASKFLEET_CONFIG"):
        args += ["--config", os.environ["TASKFLEET_CONFIG"]]
    process = await asyncio.create_subprocess_exec(
        *args, cwd=os.getcwd(), stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    try:
        stdout, stderr = await asyncio.wait_for(process.communicate(), timeout=10)
    except TimeoutError:
        process.kill()
        await process.wait()
        raise RuntimeError("taskfleet locate timed out")
    if process.returncode:
        raise RuntimeError(f"taskfleet locate failed: {(stderr or stdout).decode()[-2000:]}")
    try:
        location = json.loads(stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError(f"taskfleet locate returned invalid JSON: {error}") from error
    if not location.get("enabled") or not isinstance(location.get("config"), str):
        raise RuntimeError("Taskfleet is not enabled; run /fleet enable external")
    return location


class Taskfleet(McpIntegration):
    """Expose the packaged Taskfleet MCP server as async Python methods."""

    server = "taskfleet"

    async def call_tool(self, tool, arguments=None):
        result = await super().call_tool(tool, arguments)
        if isinstance(result, str):
            try:
                return json.loads(result)
            except json.JSONDecodeError:
                pass
        return result

    async def _open_session(self, stack: AsyncExitStack):
        from mcp import ClientSession, StdioServerParameters
        from mcp.client.stdio import stdio_client

        command = os.environ.get("TASKFLEET_BIN") or shutil.which("taskfleet")
        if not command:
            raise RuntimeError("taskfleet is not on PATH; set TASKFLEET_BIN")
        location = await _locate(command)
        parameters = StdioServerParameters(
            command=command, args=["mcp", "--config", location["config"]], cwd=location["repository"]
        )
        read, write = await stack.enter_async_context(stdio_client(parameters))
        session = await stack.enter_async_context(ClientSession(read, write))
        await session.initialize()
        return session


taskfleet = Taskfleet()
_RESERVED = {"run", "__wrapped__", "__call__"}


def __getattr__(name):
    if name.startswith("_") or name in _RESERVED:
        raise AttributeError(name)
    return getattr(taskfleet, name)
