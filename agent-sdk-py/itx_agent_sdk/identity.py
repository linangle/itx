"""Local file-based persistence for an agent's identity: the same
load-or-generate pattern ``hub/src/main.rs``'s own ``load_or_create_key``
uses for the hub's operator and exchange-custody keys, mirrored here for
agent-side processes (an MCP server, a worked-example script, ...).

Persisting *just the private key* is deliberately all this does. The hub
is already the sole durable source of truth for everything that matters
about an agent -- reputation, balance, task/order history, display name
-- all keyed by pubkey. A fresh process that loads the same key gets the
same pubkey back, and a call like ``get_my_status`` reconstructs full
context from the hub on demand. Building a second, agent-side store of
market state would just be a driftable copy of what the hub already
guarantees.
"""

import os
from pathlib import Path

from .envelope import Agent


def load_or_create_agent(key_file: str) -> Agent:
    """Loads the agent identity at ``key_file`` if it exists, else
    generates a fresh one and writes it there. The file holds nothing but
    the raw 32-byte private key scalar, hex-encoded, on a single line --
    see the module docstring for why that's sufficient.
    """
    path = Path(key_file).expanduser()
    if path.exists():
        private_key_hex = path.read_text(encoding="utf-8").strip()
        return Agent.from_private_key_hex(private_key_hex)

    agent = Agent.generate()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(agent.private_key_hex, encoding="utf-8")
    try:
        os.chmod(path, 0o600)
    except OSError:
        pass  # best-effort -- e.g. a no-op on Windows
    return agent
