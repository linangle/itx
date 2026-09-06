"""Where the two console scripts (``itx-agent`` and
``itx-agent-mcp-server``) get their settings from, so the CLI, the MCP
server and any cron heartbeat built on them all agree on one story:

    explicit flag  >  environment variable  >  built-in default

The environment variables exist because an MCP client (Claude Desktop,
Cursor, ...) or an OpenClaw skill configures a server through its
environment, not through argv, and because a heartbeat running under
cron from an arbitrary working directory must not silently mint a fresh
identity by resolving a *relative* key path against the wrong directory.
That is also why the default key file lives under the home directory
rather than the current one.
"""

import os
from pathlib import Path
from typing import Optional

ENV_HUB_URL = "ITX_HUB_URL"
ENV_KEY_FILE = "ITX_AGENT_KEY_FILE"

DEFAULT_HUB_URL = "http://127.0.0.1:9100"
DEFAULT_KEY_FILE = "~/.itx/agent.key"


def resolve_hub_url(explicit: Optional[str] = None) -> str:
    """The hub base URL: ``explicit`` if given, else ``$ITX_HUB_URL``,
    else the local-development default."""
    if explicit:
        return explicit
    return os.environ.get(ENV_HUB_URL) or DEFAULT_HUB_URL


def resolve_key_file(explicit: Optional[str] = None) -> str:
    """The agent's private-key file path: ``explicit`` if given, else
    ``$ITX_AGENT_KEY_FILE``, else ``~/.itx/agent.key`` -- with ``~``
    expanded, so the same value works in a shell, an MCP client's JSON
    config and a crontab line."""
    raw = explicit or os.environ.get(ENV_KEY_FILE) or DEFAULT_KEY_FILE
    return str(Path(raw).expanduser())
