"""Unit tests for `config` -- the one place both console scripts get
their hub URL and key-file path from, with the precedence
flag > environment > default that the README and the skill document."""

from pathlib import Path

from itx_agent_sdk import config


def test_hub_url_prefers_the_explicit_flag(monkeypatch):
    monkeypatch.setenv(config.ENV_HUB_URL, "http://from-env:1")
    assert config.resolve_hub_url("http://from-flag:2") == "http://from-flag:2"


def test_hub_url_falls_back_to_the_environment(monkeypatch):
    monkeypatch.setenv(config.ENV_HUB_URL, "http://from-env:1")
    assert config.resolve_hub_url(None) == "http://from-env:1"


def test_hub_url_falls_back_to_the_default(monkeypatch):
    monkeypatch.delenv(config.ENV_HUB_URL, raising=False)
    assert config.resolve_hub_url(None) == config.DEFAULT_HUB_URL


def test_key_file_precedence_and_tilde_expansion(monkeypatch, tmp_path):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv(config.ENV_KEY_FILE, raising=False)

    default = Path(config.resolve_key_file(None))
    assert default.is_absolute()
    assert default == tmp_path / ".itx" / "agent.key"

    monkeypatch.setenv(config.ENV_KEY_FILE, "~/elsewhere/agent.key")
    assert config.resolve_key_file(None) == str(tmp_path / "elsewhere" / "agent.key")

    assert config.resolve_key_file("~/explicit.key") == str(tmp_path / "explicit.key")


def test_default_key_file_is_not_relative_to_the_working_directory():
    # A cron heartbeat or an MCP client launches the process from an
    # arbitrary directory; a cwd-relative default would silently mint a
    # fresh identity there and strand the funded one.
    assert config.DEFAULT_KEY_FILE.startswith("~")
