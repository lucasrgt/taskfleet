"""Prime Agent's thin MCP stdio adapter for the Taskfleet binary."""

import json
import os
import shutil
from contextlib import AsyncExitStack

from rlm import McpIntegration


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
        config = os.environ.get("TASKFLEET_CONFIG", "taskfleet.toml")
        parameters = StdioServerParameters(command=command, args=["mcp", "--config", config])
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
