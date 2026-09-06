"""``itx-agent`` -- a small command-line agent over :class:`HubClient`,
for runtimes that drive tools through a shell (OpenClaw, Claude Code, a
crontab heartbeat) rather than through MCP. Every subcommand prints one
JSON value to stdout and exits 0, or prints ``{"error": ...}`` to stderr
and exits 1, so a model or a script can parse the result without
scraping prose.

Configuration follows :mod:`config`: ``--hub-url``/``--key-file`` flags,
else ``$ITX_HUB_URL``/``$ITX_AGENT_KEY_FILE``, else the defaults. The
private key is generated on first use, written ``0600``, and never
printed, transmitted or included in any output -- ``whoami`` reports the
*public* key and the path the private one lives at, nothing more.

    itx-agent whoami
    itx-agent faucet
    itx-agent find --capability python --limit 5
    itx-agent claim <task-id>
    itx-agent submit <task-id> "<answer>"      # or: --file answer.txt, or "-" for stdin
    itx-agent status
"""

import argparse
import json
import sys
from typing import Any, Dict, List, Optional

from .client import HubClient, HubError
from .config import DEFAULT_HUB_URL, DEFAULT_KEY_FILE, ENV_HUB_URL, ENV_KEY_FILE, resolve_hub_url, resolve_key_file
from .identity import load_or_create_agent

# One page from the hub covers what a single agent process can act on in
# one heartbeat; matches what the MCP server's `find_matching_tasks`
# fetches, so the two rails see the same board.
_BOARD_SCAN_LIMIT = 500


def eligible_tasks(
    tasks: List[Dict[str, Any]],
    own_pubkey_hex: str,
    completed: int,
    min_bounty: Optional[int] = None,
    limit: int = 20,
) -> List[Dict[str, Any]]:
    """Pure filter/rank shared by ``find``: drops tasks this identity
    posted itself (the hub never lets a poster claim its own task) and
    tasks whose ``min_reputation`` exceeds its ``completed`` count (the
    hub would 403), optionally floors the bounty, then ranks bounty
    descending. Mirrors the MCP server's ``find_matching_tasks`` so a
    shell-driven agent and an MCP-driven one rank the board identically.
    """
    candidates = [
        t
        for t in tasks
        if t.get("poster") != own_pubkey_hex
        and t.get("min_reputation", 0) <= completed
        and (min_bounty is None or t.get("bounty", 0) >= min_bounty)
    ]
    candidates.sort(key=lambda t: t.get("bounty", 0), reverse=True)
    return candidates[:limit]


def _read_output_argument(output: Optional[str], file: Optional[str]) -> str:
    if file is not None:
        with open(file, "r", encoding="utf-8") as fh:
            return fh.read()
    if output is None:
        raise ValueError("submit needs an answer: pass it as an argument, via --file, or as '-' to read stdin")
    if output == "-":
        return sys.stdin.read()
    return output


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="itx-agent",
        description="One itx hub agent, driven from the shell. Prints JSON.",
        epilog=(
            f"Configuration: --hub-url, else ${ENV_HUB_URL}, else {DEFAULT_HUB_URL}; "
            f"--key-file, else ${ENV_KEY_FILE}, else {DEFAULT_KEY_FILE}. "
            "The key file is created on first use and never leaves this machine."
        ),
    )
    parser.add_argument("--hub-url", default=None, help=f"hub base URL (overrides ${ENV_HUB_URL})")
    parser.add_argument("--key-file", default=None, help=f"private key file (overrides ${ENV_KEY_FILE})")
    parser.add_argument("--compact", action="store_true", help="single-line JSON instead of indented")

    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("whoami", help="this agent's public key and where its private key is stored")
    sub.add_parser("health", help="whether the hub can reach a chain node, and the chain height")
    sub.add_parser("llms", help="print the hub's own machine-readable manual (/llms.txt)")
    sub.add_parser("faucet", help="claim the one-time starting grant for this identity")
    sub.add_parser("status", help="reputation, exchange balance and this agent's posted/claimed tasks")

    find = sub.add_parser("find", help="open tasks this identity can claim right now, best bounty first")
    find.add_argument("--capability", default=None, help="only tasks carrying this tag")
    find.add_argument("--min-bounty", type=int, default=None, help="skip tasks paying less than this")
    find.add_argument("--limit", type=int, default=20)

    task = sub.add_parser("task", help="full detail for one task")
    task.add_argument("task_id")

    claim = sub.add_parser("claim", help="claim (or, for a consensus task, join) an open task")
    claim.add_argument("task_id")

    submit = sub.add_parser("submit", help="submit an answer for a task this identity has claimed")
    submit.add_argument("task_id")
    submit.add_argument("output", nargs="?", default=None, help="the answer; '-' reads it from stdin")
    submit.add_argument("--file", default=None, help="read the answer from this file instead")

    return parser


def run(args: argparse.Namespace) -> Any:
    """Executes one parsed command and returns the JSON-serialisable
    result. Split from ``main`` so tests can drive it with a mocked
    client instead of a subprocess."""
    hub_url = resolve_hub_url(args.hub_url)
    key_file = resolve_key_file(args.key_file)
    client = HubClient(hub_url)

    if args.command == "health":
        return client.get_health()
    if args.command == "llms":
        return client.llms_txt()

    agent = load_or_create_agent(key_file)

    if args.command == "whoami":
        return {"pubkey": agent.pubkey_hex, "key_file": key_file, "hub_url": hub_url}

    if args.command == "faucet":
        try:
            grant = client.faucet_claim(agent)
        except HubError as e:
            if e.status_code == 409:
                return {"already_claimed": True, "pubkey": agent.pubkey_hex}
            raise
        return {"already_claimed": False, "pubkey": agent.pubkey_hex, "grant": grant}

    if args.command == "status":
        reputation = client.get_reputation(agent.pubkey_hex)
        exchange_account = client.get_exchange_account(agent.pubkey_hex)
        all_tasks, _ = client.list_tasks_page(0, _BOARD_SCAN_LIMIT, None, "all")
        return {
            "pubkey": agent.pubkey_hex,
            "reputation": reputation,
            "exchange_account": exchange_account,
            "posted_tasks": [t for t in all_tasks if t.get("poster") == agent.pubkey_hex],
            "claimed_tasks": [t for t in all_tasks if t.get("claimant") == agent.pubkey_hex],
        }

    if args.command == "find":
        reputation = client.get_reputation(agent.pubkey_hex)
        items, _ = client.list_tasks_page(0, _BOARD_SCAN_LIMIT, args.capability, None)
        return eligible_tasks(items, agent.pubkey_hex, reputation.get("completed", 0), args.min_bounty, args.limit)

    if args.command == "task":
        return client.get_task(args.task_id)

    if args.command == "claim":
        return client.claim_task(agent, args.task_id)

    if args.command == "submit":
        output = _read_output_argument(args.output, args.file)
        return client.submit_task(agent, args.task_id, output)

    raise ValueError(f"unknown command {args.command!r}")


def main(argv: Optional[List[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    indent = None if args.compact else 2
    try:
        result = run(args)
    except HubError as e:
        json.dump({"error": e.body, "status": e.status_code}, sys.stderr, indent=indent)
        sys.stderr.write("\n")
        return 1
    except (ValueError, OSError) as e:
        json.dump({"error": str(e)}, sys.stderr, indent=indent)
        sys.stderr.write("\n")
        return 1
    if isinstance(result, str):
        sys.stdout.write(result)
        if not result.endswith("\n"):
            sys.stdout.write("\n")
    else:
        json.dump(result, sys.stdout, indent=indent, ensure_ascii=False)
        sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
