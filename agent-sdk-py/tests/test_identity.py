"""Unit tests for `identity.load_or_create_agent` -- the file-based
load-or-generate pattern MCP servers and worked-example scripts use so
an agent's pubkey (and, via the hub, its reputation/balance) survives a
process restart.
"""

import os
import stat

import pytest

from itx_agent_sdk.identity import load_or_create_agent


def test_generates_and_persists_a_new_identity_when_file_is_absent(tmp_path):
    key_file = tmp_path / "agent.key"
    assert not key_file.exists()

    agent = load_or_create_agent(str(key_file))

    assert key_file.exists()
    assert key_file.read_text(encoding="utf-8").strip() == agent.private_key_hex


def test_loads_the_same_identity_on_a_second_call(tmp_path):
    key_file = tmp_path / "agent.key"

    first = load_or_create_agent(str(key_file))
    second = load_or_create_agent(str(key_file))

    assert first.pubkey_hex == second.pubkey_hex
    assert first.private_key_hex == second.private_key_hex


def test_creates_missing_parent_directories(tmp_path):
    key_file = tmp_path / "nested" / "dir" / "agent.key"

    agent = load_or_create_agent(str(key_file))

    assert key_file.exists()
    assert load_or_create_agent(str(key_file)).pubkey_hex == agent.pubkey_hex


def test_the_key_file_is_created_0600_and_its_directory_0700(tmp_path):
    """Mode is set by `os.open` at creation, not chmod'ed afterwards --
    otherwise the key sits on disk world-readable for the width of that
    gap under any ordinary umask.
    """
    key_file = tmp_path / "keys" / "agent.key"
    previous_umask = os.umask(0o022)
    try:
        load_or_create_agent(str(key_file))
    finally:
        os.umask(previous_umask)

    assert stat.S_IMODE(key_file.stat().st_mode) == 0o600
    assert stat.S_IMODE(key_file.parent.stat().st_mode) == 0o700


def test_an_existing_key_file_anyone_can_read_is_narrowed_on_load(tmp_path):
    key_file = tmp_path / "agent.key"
    first = load_or_create_agent(str(key_file))
    os.chmod(key_file, 0o644)

    second = load_or_create_agent(str(key_file))

    assert second.pubkey_hex == first.pubkey_hex, "narrowing the mode must not disturb the identity"
    assert stat.S_IMODE(key_file.stat().st_mode) == 0o600


@pytest.mark.parametrize(
    "contents",
    [
        "",
        "ab" * 31,  # truncated: `ecdsa` raises MalformedPointError, an AssertionError
        "ab" * 40,
        "not hex at all",
        "00" * 32,  # a valid-length scalar that is out of range
        "-----BEGIN OPENSSH PRIVATE KEY-----\nwrong file entirely\n",
    ],
)
def test_a_corrupt_key_file_raises_valueerror_without_echoing_its_contents(tmp_path, contents):
    """`cli.main` renders `ValueError`/`OSError` as the documented
    `{"error": ...}` JSON and lets anything else escape as a traceback.
    `ecdsa` signals a bad scalar with `MalformedPointError`, which
    subclasses `AssertionError` -- so it has to be converted here.
    """
    key_file = tmp_path / "agent.key"
    key_file.write_text(contents, encoding="utf-8")

    with pytest.raises(ValueError) as excinfo:
        load_or_create_agent(str(key_file))

    message = str(excinfo.value)
    assert str(key_file) in message
    # Only the substantial fragments -- a message that happens to share
    # the word "file" with an SSH header is fine, a message carrying 31
    # of the 32 bytes of somebody's private key is not.
    for fragment in contents.split():
        if len(fragment) >= 8:
            assert fragment not in message, "the file's contents may still be most of a live key"


def test_a_corrupt_key_file_is_not_overwritten(tmp_path):
    """The bad file might be a damaged copy of a funded identity. Losing
    it silently is worse than failing loudly.
    """
    key_file = tmp_path / "agent.key"
    key_file.write_text("ab" * 31, encoding="utf-8")

    with pytest.raises(ValueError):
        load_or_create_agent(str(key_file))

    assert key_file.read_text(encoding="utf-8") == "ab" * 31
