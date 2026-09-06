"""Checks on the two files that decide how this package is installed and
launched, `pyproject.toml` and `server.json`.

Neither is exercised by importing anything, so nothing else in this suite
notices when they disagree -- which is how the MCP registry entry came to
generate a command (`uvx --from itx-agent-sdk[mcp] itx-agent-sdk`) that
the wheel provided no executable for.
"""

from pathlib import Path

import pytest

tomllib = pytest.importorskip("tomllib", reason="tomllib is stdlib from Python 3.11")

import json  # noqa: E402

PACKAGE_ROOT = Path(__file__).resolve().parent.parent


@pytest.fixture(scope="module")
def pyproject() -> dict:
    with open(PACKAGE_ROOT / "pyproject.toml", "rb") as fh:
        return tomllib.load(fh)


@pytest.fixture(scope="module")
def server_json() -> dict:
    with open(PACKAGE_ROOT / "server.json", encoding="utf-8") as fh:
        return json.load(fh)


def test_the_registry_composed_command_names_a_console_script(pyproject, server_json):
    """The MCP registry builds its launch command as `<runtimeHint>
    <runtimeArguments...> <identifier>`, so `identifier` has to be an
    executable the installed package provides -- not just the name of the
    distribution it came from.
    """
    package = server_json["packages"][0]
    scripts = pyproject["project"]["scripts"]
    assert package["identifier"] in scripts, (
        f"server.json's identifier {package['identifier']!r} is not a console script; "
        f"the registry would generate a command that cannot run. Scripts: {sorted(scripts)}"
    )


def test_the_registry_installs_the_extra_the_mcp_server_needs(pyproject, server_json):
    """`itx-agent-sdk` runs the MCP server, which imports the `mcp`
    runtime -- an optional extra. The registry entry has to ask for it.
    """
    package = server_json["packages"][0]
    from_args = [a["value"] for a in package["runtimeArguments"] if a.get("name") == "--from"]
    assert from_args == ["itx-agent-sdk[mcp]"], from_args
    assert "mcp" in pyproject["project"]["optional-dependencies"]


def test_every_console_script_points_at_something_importable(pyproject):
    import importlib

    for name, target in pyproject["project"]["scripts"].items():
        module_name, _, attribute = target.partition(":")
        module = pytest.importorskip(module_name, reason=f"{name} needs an optional extra")
        assert callable(getattr(module, attribute)), f"{name} -> {target}"
        importlib.import_module(module_name)


def test_the_version_is_the_same_in_pyproject_and_server_json(pyproject, server_json):
    """The publish workflow refuses a mismatch, but only a tag push
    carries a version to compare against -- this catches the same drift
    on a manual `workflow_dispatch` run, and in local development.
    """
    version = pyproject["project"]["version"]
    assert server_json["version"] == version
    assert server_json["packages"][0]["version"] == version


def test_an_environment_variable_with_a_default_is_not_also_required(server_json):
    """A registry client prompts for anything `isRequired`, so a variable
    that is both required and defaulted asks the user to supply something
    the README calls optional.
    """
    for env in server_json["packages"][0]["environmentVariables"]:
        if "default" in env:
            assert env["isRequired"] is False, env["name"]
