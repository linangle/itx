"""Unit tests for the `itx-agent` command line: the pure ranking helper,
each subcommand's request against a mocked `HubClient`, the faucet's
already-claimed path, and the two invariants the skill relies on --
output is JSON, and the private key is never in it."""

import io
import json
from unittest.mock import MagicMock

import pytest

from itx_agent_sdk import Agent, HubError, cli

OWN = "02" + "ab" * 32
OTHER = "03" + "cd" * 32


def task(id_, poster=OTHER, bounty=10, min_reputation=0, claimant=None):
    return {
        "id": id_,
        "poster": poster,
        "claimant": claimant,
        "bounty": bounty,
        "min_reputation": min_reputation,
        "created_at": "2026-09-05T00:00:00+00:00",
    }


# -- eligible_tasks --------------------------------------------------------


def test_eligible_tasks_drops_own_and_gated_tasks_and_ranks_by_bounty():
    board = [
        task("mine", poster=OWN, bounty=100),
        task("too-hard", min_reputation=5, bounty=90),
        task("small", bounty=1),
        task("big", bounty=50),
    ]
    ranked = cli.eligible_tasks(board, OWN, completed=2)
    assert [t["id"] for t in ranked] == ["big", "small"]


def test_eligible_tasks_honours_min_bounty_and_limit():
    board = [task(str(i), bounty=i) for i in range(10)]
    ranked = cli.eligible_tasks(board, OWN, completed=0, min_bounty=5, limit=3)
    assert [t["bounty"] for t in ranked] == [9, 8, 7]


# -- subcommands against a mocked client ---------------------------------


@pytest.fixture
def env(monkeypatch, tmp_path):
    """A fresh identity in a temp home, a mocked `HubClient`, and a
    runner that parses argv the way the console script would."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv(cli.ENV_KEY_FILE, raising=False)
    monkeypatch.setenv(cli.ENV_HUB_URL, "http://hub.test")
    client = MagicMock()
    monkeypatch.setattr(cli, "HubClient", MagicMock(return_value=client))

    def run(*argv):
        return cli.run(cli.parse_args(list(argv)))

    return client, run, tmp_path


def test_whoami_reports_pubkey_and_paths_but_never_the_private_key(env):
    client, run, home = env
    result = run("whoami")
    key_file = home / ".itx" / "agent.key"
    assert key_file.exists()
    private_hex = key_file.read_text().strip()
    agent = Agent.from_private_key_hex(private_hex)

    assert result == {"pubkey": agent.pubkey_hex, "key_file": str(key_file), "hub_url": "http://hub.test"}
    assert private_hex not in json.dumps(result)


def test_faucet_reports_a_grant_and_then_already_claimed(env):
    client, run, _ = env
    client.faucet_claim.return_value = {"amount": 50_000_000}
    first = run("faucet")
    assert first["already_claimed"] is False
    assert first["grant"] == {"amount": 50_000_000}

    client.faucet_claim.side_effect = HubError(409, {"error": "already claimed"})
    second = run("faucet")
    assert second["already_claimed"] is True
    assert second["pubkey"] == first["pubkey"]


def test_faucet_reraises_anything_but_a_409(env):
    client, run, _ = env
    client.faucet_claim.side_effect = HubError(503, {"error": "no node"})
    with pytest.raises(HubError):
        run("faucet")


def test_find_scans_the_open_board_with_the_capability_filter(env):
    client, run, _ = env
    own_pubkey = run("whoami")["pubkey"]
    client.get_reputation.return_value = {"completed": 1, "failed": 0}
    client.list_tasks_page.return_value = (
        [task("a", bounty=5), task("b", bounty=7), task("c", poster=own_pubkey, bounty=99)],
        3,
    )
    result = run("find", "--capability", "python", "--limit", "1")
    client.list_tasks_page.assert_called_once_with(0, cli._BOARD_SCAN_LIMIT, "python", None)
    assert [t["id"] for t in result] == ["b"]


def test_status_splits_posted_and_claimed_tasks(env):
    client, run, _ = env
    own_pubkey = run("whoami")["pubkey"]
    client.get_reputation.return_value = {"completed": 0, "failed": 0}
    client.get_exchange_account.return_value = {"base_balance": 0}
    client.list_tasks_page.return_value = (
        [task("posted", poster=own_pubkey), task("claimed", claimant=own_pubkey), task("other")],
        3,
    )
    result = run("status")
    client.list_tasks_page.assert_called_once_with(0, cli._BOARD_SCAN_LIMIT, None, "all")
    assert [t["id"] for t in result["posted_tasks"]] == ["posted"]
    assert [t["id"] for t in result["claimed_tasks"]] == ["claimed"]


def test_claim_and_task_pass_the_id_through(env):
    client, run, _ = env
    client.claim_task.return_value = {"status": "Claimed"}
    assert run("claim", "t1") == {"status": "Claimed"}
    agent_used, task_id = client.claim_task.call_args.args
    assert task_id == "t1"
    assert isinstance(agent_used, Agent)

    client.get_task.return_value = {"id": "t1"}
    assert run("task", "t1") == {"id": "t1"}
    client.get_task.assert_called_once_with("t1")


def test_submit_takes_the_answer_from_argv_stdin_or_a_file(env, monkeypatch, tmp_path):
    client, run, _ = env
    client.submit_task.return_value = {"ok": True}

    run("submit", "t1", "inline answer")
    assert client.submit_task.call_args.args[1:] == ("t1", "inline answer")

    monkeypatch.setattr("sys.stdin", io.StringIO("from stdin"))
    run("submit", "t1", "-")
    assert client.submit_task.call_args.args[1:] == ("t1", "from stdin")

    answer_file = tmp_path / "answer.txt"
    answer_file.write_text("from a file", encoding="utf-8")
    run("submit", "t1", "--file", str(answer_file))
    assert client.submit_task.call_args.args[1:] == ("t1", "from a file")


def test_submit_without_an_answer_is_an_error(env):
    client, run, _ = env
    with pytest.raises(ValueError):
        run("submit", "t1")


def test_health_and_llms_need_no_identity(env):
    client, run, home = env
    client.get_health.return_value = {"status": "ok"}
    client.llms_txt.return_value = "# itx agent hub\n"
    assert run("health") == {"status": "ok"}
    assert run("llms") == "# itx agent hub\n"
    assert not (home / ".itx").exists()


def test_global_flags_are_accepted_before_or_after_the_subcommand(env):
    client, run, home = env
    before = run("--hub-url", "http://before:1", "--key-file", str(home / "a.key"), "whoami")
    after = run("whoami", "--hub-url", "http://after:2", "--key-file", str(home / "b.key"))
    assert before["hub_url"] == "http://before:1" and before["key_file"] == str(home / "a.key")
    assert after["hub_url"] == "http://after:2" and after["key_file"] == str(home / "b.key")
    # A flag before the subcommand survives the subcommand's own parse.
    assert run("--hub-url", "http://kept:3", "whoami")["hub_url"] == "http://kept:3"
    assert cli.parse_args(["health", "--compact"]).compact is True


# -- main(): exit codes and JSON on both streams ------------------------


def test_main_prints_json_and_exits_zero(env, capsys):
    client, _, _ = env
    client.get_health.return_value = {"status": "ok", "chain_height": 3}
    assert cli.main(["--compact", "health"]) == 0
    out = capsys.readouterr().out
    assert json.loads(out) == {"status": "ok", "chain_height": 3}
    assert out.count("\n") == 1


def test_main_reports_hub_errors_as_json_on_stderr(env, capsys):
    client, _, _ = env
    client.get_health.side_effect = HubError(503, {"error": "no reachable node"})
    assert cli.main(["health"]) == 1
    captured = capsys.readouterr()
    assert captured.out == ""
    assert json.loads(captured.err) == {"error": {"error": "no reachable node"}, "status": 503}
