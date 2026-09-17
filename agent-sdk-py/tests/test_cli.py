"""Unit tests for the `itx-agent` command line: the pure ranking helper,
each subcommand's request against a mocked `HubClient`, the faucet's
already-claimed path, and the two invariants the skill relies on --
output is JSON, and the private key is never in it."""

import io
import json
from unittest.mock import MagicMock

import pytest

from itx_agent_sdk import Agent, HubClient, HubError, cli

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


def _cli_challenge(expected_hashes: int = 64) -> dict:
    target = (1 << 256) // expected_hashes
    return {
        "challenge_id": "0f5f1e1a-0000-4000-8000-00000000abcd",
        "target": f"{target:064x}",
        "expected_hashes": expected_hashes,
        "preimage_template": "cli-test:{solution}",
    }


def test_faucet_solves_a_challenge_then_reports_the_grant(env):
    client, run, _ = env
    client.faucet_challenge.return_value = _cli_challenge()
    client.faucet_claim.return_value = {"amount": 50_000_000}

    result = run("faucet")

    assert result["already_claimed"] is False
    assert result["solved"] is True
    assert result["grant"] == {"amount": 50_000_000}
    # The solve time is reported because it is the only step here that
    # takes real time, and a cron log should show what it cost.
    assert isinstance(result["solve_seconds"], float)
    assert result["expected_hashes"] == 64
    # Redeemed with the id it was issued, not a fresh one.
    client.faucet_claim.assert_called_once()
    assert client.faucet_claim.call_args[0][1] == "0f5f1e1a-0000-4000-8000-00000000abcd"


def test_faucet_reports_already_claimed_without_solving(env):
    """The refusal comes at the challenge step, so an already-granted key
    never spends the CPU."""
    client, run, _ = env
    client.faucet_challenge.side_effect = HubError(409, {"error": "already claimed"})

    result = run("faucet")

    assert result["already_claimed"] is True
    client.faucet_claim.assert_not_called()


def test_faucet_reraises_anything_but_a_409(env):
    client, run, _ = env
    client.faucet_challenge.side_effect = HubError(503, {"error": "no node"})
    with pytest.raises(HubError):
        run("faucet")


def test_faucet_gives_up_rather_than_hanging_on_an_unreachable_difficulty(env):
    """An operator can raise the knob past what a given machine can do.
    That has to end in a JSON answer, not a wedged cron job."""
    client, run, _ = env
    impossible = _cli_challenge()
    impossible["target"] = f"{0:064x}"
    client.faucet_challenge.return_value = impossible

    result = run("faucet", "--max-seconds", "0.25")

    assert result["solved"] is False
    assert "error" in result
    client.faucet_claim.assert_not_called()


def test_find_scans_the_open_board_with_the_capability_filter(env):
    client, run, _ = env
    own_pubkey = run("whoami")["pubkey"]
    client.get_reputation.return_value = {"completed": 1, "failed": 0}
    client.list_tasks_scan.return_value = (
        [task("a", bounty=5), task("b", bounty=7), task("c", poster=own_pubkey, bounty=99)],
        3,
    )
    result = run("find", "--capability", "python", "--limit", "1")
    client.list_tasks_scan.assert_called_once_with(capability="python")
    assert [t["id"] for t in result] == ["b"]


def test_status_splits_posted_and_claimed_tasks(env):
    client, run, _ = env
    own_pubkey = run("whoami")["pubkey"]
    client.get_reputation.return_value = {"completed": 0, "failed": 0}
    client.get_exchange_account.return_value = {"base_balance": 0}
    client.list_tasks_scan.return_value = (
        [task("posted", poster=own_pubkey), task("claimed", claimant=own_pubkey), task("other")],
        3,
    )
    result = run("status")
    client.list_tasks_scan.assert_called_once_with(status="all")
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


def test_a_corrupt_key_file_is_reported_as_json_not_a_traceback(env, capsys, tmp_path):
    """The skill parses stderr as JSON on failure. `ecdsa` raises
    `MalformedPointError`, which subclasses `AssertionError` rather than
    `ValueError`, so a truncated key file used to escape `main`'s handler
    entirely and print a traceback instead.
    """
    key_file = tmp_path / "corrupt.key"
    key_file.write_text("ab" * 31, encoding="utf-8")

    assert cli.main(["--compact", "--key-file", str(key_file), "whoami"]) == 1

    captured = capsys.readouterr()
    assert captured.out == ""
    payload = json.loads(captured.err)
    assert set(payload) == {"error"}, "a local failure carries no hub status"
    assert "ab" * 31 not in payload["error"]


def test_a_base_url_with_a_path_prefix_is_reported_as_json(env, monkeypatch, capsys):
    """`HubClient` refuses it at construction; `main` has to turn that
    into the documented error shape rather than a traceback.
    """
    monkeypatch.setattr(cli, "HubClient", HubClient)
    monkeypatch.setenv(cli.ENV_HUB_URL, "https://hub.test/api")

    assert cli.main(["--compact", "health"]) == 1

    payload = json.loads(capsys.readouterr().err)
    assert "no path, query or fragment" in payload["error"]


# -- wallet, post, confirm, send ------------------------------------------
#
# The demand side. These exist so that anyone can put work on the board,
# which before them needed a node port no stranger can reach.

import hashlib  # noqa: E402

ESCROW = {"escrow_id": "e1", "deposit_address": "02" + "ee" * 32, "required_amount": 1_500, "expires_at": "x"}


def test_wallet_reads_this_identitys_own_wallet(env):
    client, run, home = env
    client.get_wallet.return_value = {"balance": 5}
    assert run("wallet") == {"balance": 5}
    agent = Agent.from_private_key_hex((home / ".itx" / "agent.key").read_text().strip())
    client.get_wallet.assert_called_once_with(agent.pubkey_hex)


def test_post_reserves_pays_and_waits_for_the_block(env):
    client, run, _ = env
    client.create_task_escrow.return_value = ESCROW
    client.fund_escrow.return_value = {"tx_hash": "dd" * 32}
    client.wait_for_task_funding.return_value = {"id": "t1", "status": "Open"}

    result = run("post", "--description", "reverse it", "--bounty", "500", "--answer", "tset", "--capability", "text/reversal")

    args = client.create_task_escrow.call_args[0]
    assert args[1:] == ("reverse it", 500, hashlib.sha256(b"tset").hexdigest(), 0, ["text/reversal"]), \
        "only the answer's hash goes to the hub"
    assert client.fund_escrow.call_args[0][1] == ESCROW
    assert client.wait_for_task_funding.call_args[0][1] == "e1"
    assert result == {"reservation": ESCROW, "sent": {"tx_hash": "dd" * 32}, "task": {"id": "t1", "status": "Open"}}


def test_post_no_wait_stops_after_paying_and_says_how_to_finish(env):
    client, run, _ = env
    client.create_task_escrow.return_value = ESCROW
    client.fund_escrow.return_value = {"tx_hash": "dd" * 32}
    result = run("post", "--description", "d", "--bounty", "5", "--expected-output-hash", "ab" * 32, "--no-wait")
    client.wait_for_task_funding.assert_not_called()
    assert result["task"] is None
    assert "itx-agent confirm e1" in result["next"]


def test_post_knows_each_kinds_flags(env):
    client, run, _ = env
    client.create_consensus_task_escrow.return_value = ESCROW
    client.create_disputable_task_escrow.return_value = ESCROW
    client.fund_escrow.return_value = {}
    client.wait_for_task_funding.return_value = {}

    run("post", "--kind", "consensus", "--description", "d", "--bounty", "9", "--num-assignees", "3",
        "--join-window-minutes", "10", "--submission-window-minutes", "20", "--min-reputation", "2")
    assert client.create_consensus_task_escrow.call_args[0][1:] == ("d", 9, 3, 10, 20, 2, None)

    run("post", "--kind", "disputable", "--description", "d", "--bounty", "9", "--dispute-window-minutes", "30")
    assert client.create_disputable_task_escrow.call_args[0][1:] == ("d", 9, 30, 1, None), \
        "a disputable task defaults to min_reputation 1, as the hub does"
    run("post", "--kind", "disputable", "--description", "d", "--bounty", "9", "--dispute-window-minutes", "30",
        "--min-reputation", "0")
    assert client.create_disputable_task_escrow.call_args[0][1:] == ("d", 9, 30, 0, None)

    with pytest.raises(ValueError, match="consensus task needs --num-assignees"):
        run("post", "--kind", "consensus", "--description", "d", "--bounty", "9")
    run("post", "--kind", "disputable", "--description", "d", "--bounty", "9")
    assert client.create_disputable_task_escrow.call_args[0][1:] == ("d", 9, 60, 1, None), \
        "the hub ignores the dispute window, so the command does not ask for one"
    with pytest.raises(ValueError, match="exactly one of"):
        run("post", "--description", "d", "--bounty", "9")
    with pytest.raises(ValueError, match="exactly one of"):
        run("post", "--description", "d", "--bounty", "9", "--answer", "a", "--expected-output-hash", "ab" * 32)
    # None of the refusals reserved anything.
    assert client.create_task_escrow.call_count == 0
    assert client.fund_escrow.call_count == 4


def test_confirm_and_send_pass_their_arguments_through(env):
    client, run, _ = env
    client.confirm_task_escrow.return_value = {"id": "t1"}
    assert run("confirm", "e1") == {"id": "t1"}
    assert client.confirm_task_escrow.call_args[0][1] == "e1"

    client.send.return_value = {"tx_hash": "dd" * 32}
    assert run("send", "03" + "cd" * 32, "250") == {"tx_hash": "dd" * 32}
    assert client.send.call_args[0][1:] == ("03" + "cd" * 32, 250)
