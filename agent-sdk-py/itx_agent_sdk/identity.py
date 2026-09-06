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
import stat
from pathlib import Path

from .envelope import Agent

# The only mode this file is ever meant to have: readable and writable by
# its owner and nobody else.
_KEY_FILE_MODE = 0o600


def load_or_create_agent(key_file: str) -> Agent:
    """Loads the agent identity at ``key_file`` if it exists, else
    generates a fresh one and writes it there. The file holds nothing but
    the raw 32-byte private key scalar, hex-encoded, on a single line --
    see the module docstring for why that's sufficient.

    Raises ``ValueError`` if the file exists but does not hold a usable
    key, and ``OSError`` if it cannot be read or written. Those are the
    two error types ``cli.main`` renders as the ``{"error": ...}`` JSON
    every caller of this SDK parses, which is why the several shapes of
    "that isn't a key" `ecdsa` can raise are funnelled into one of them
    here.
    """
    path = Path(key_file).expanduser()
    if path.exists():
        private_key_hex = path.read_text(encoding="utf-8").strip()
        try:
            agent = Agent.from_private_key_hex(private_key_hex)
        except Exception:
            # Deliberately broad, and deliberately not reporting the cause.
            # `ecdsa` signals a bad scalar with `MalformedPointError`,
            # which subclasses `AssertionError` rather than `ValueError`,
            # so listing exception types here has already been wrong once;
            # and the message must not carry any part of the file's
            # contents, because on the "file is truncated" path those
            # contents are still most of a live private key.
            raise ValueError(
                f"{path} does not contain a valid agent private key "
                "(expected one line of 64 hex characters). Move it aside to have a fresh "
                "identity generated, but only if you are sure you do not need the old one -- "
                "the key file is the whole identity."
            ) from None
        _tighten_mode(path)
        return agent

    agent = Agent.generate()
    # 0700 on the directory as well as 0600 on the file: `~/.itx` holds
    # nothing but key material, and a 0755 directory advertises which
    # identities exist on the machine even when the keys themselves are
    # unreadable.
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Created with its final mode by `os.open`, not chmod'ed afterwards.
    # `write_text` then `chmod` leaves the key on disk world-readable for
    # the width of that gap under any ordinary umask, and `O_EXCL` also
    # refuses to write over a key that appeared since the `exists()` check
    # above rather than clobbering it.
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, _KEY_FILE_MODE)
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(agent.private_key_hex)
    return agent


def _tighten_mode(path: Path) -> None:
    """Narrows an existing key file back to ``0600`` if anyone else can
    read it.

    Repairing rather than refusing: by the time we notice, the key has
    already been exposed, and refusing to start would strand a running
    agent without making it any less exposed. Narrowing is idempotent and
    strictly an improvement. Best-effort -- ``chmod`` is a no-op or an
    error on filesystems that don't carry Unix modes.
    """
    try:
        current = stat.S_IMODE(path.stat().st_mode)
        if current & ~_KEY_FILE_MODE:
            os.chmod(path, _KEY_FILE_MODE)
    except OSError:
        pass
