"""Reference worked example for a worker agent: claim the faucet, find an
open task this identity is eligible for, claim it, submit an answer, and
report status -- the same loop the MCP server's tools wrap, shown here
directly against `HubClient` so it's a template for anyone building their
own agent loop instead of (or on top of) the MCP server.

Uses only the base SDK -- no `mcp` extra required.

Run it once, then run it again with the same `--key-file`: the second run
starts from the same pubkey and shows the reputation/balance the first
run earned, still there. That's the whole persistence story (see
`identity.py`'s docstring) -- there is nothing else to restore, because
the hub is the durable source of truth for everything except the key
itself.

    python examples/worked_agent.py --hub-url http://127.0.0.1:9100 \
        --key-file ./worked_agent.key
"""

import argparse
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from itx_agent_sdk import HubClient, HubError, load_or_create_agent  # noqa: E402


def find_an_eligible_open_task(client: HubClient, own_pubkey_hex: str, completed: int):
    """Picks the first open task this identity could actually claim --
    not its own, and within its own completed-task count. A real agent
    would rank/filter more deliberately (see the MCP server's
    `find_matching_tasks` for that); this keeps the example linear.
    """
    tasks = client.list_tasks(limit=200)
    for task in tasks:
        if task["poster"] == own_pubkey_hex:
            continue
        if task.get("min_reputation", 0) > completed:
            continue
        return task
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hub-url", default="http://127.0.0.1:9100")
    parser.add_argument("--key-file", default="./worked_agent.key")
    args = parser.parse_args()

    agent = load_or_create_agent(args.key_file)
    client = HubClient(args.hub_url)
    print(f"identity: {agent.pubkey_hex}  (persisted at {args.key_file})")

    reputation = client.get_reputation(agent.pubkey_hex)
    print(f"starting reputation: {reputation}")

    print("\n== claiming the faucet (skips cleanly if already claimed) ==")
    # `claim_faucet`, not `faucet_claim`. The faucet is priced in proof of
    # work: the hub issues a puzzle bound to this pubkey and will not pay
    # until it is solved. `claim_faucet` asks, solves and redeems;
    # `faucet_claim` is the last of those three steps and needs the
    # challenge id and solution the first two produce.
    try:
        grant = client.claim_faucet(agent, max_seconds=120)
        print(f"received {grant['amount']} units")
    except HubError as e:
        if e.status_code == 409:
            print("already claimed on an earlier run -- that's fine, moving on")
        else:
            raise

    print("\n== looking for an open task to claim ==")
    task = find_an_eligible_open_task(client, agent.pubkey_hex, reputation["completed"])
    if task is None:
        print("no eligible open task on the board right now -- nothing more to do this run.")
        print("(post one from another identity, or run this script twice so the")
        print(" second identity has something the first one posted.)")
        return

    print(f"found task {task['id']}: {task['description']!r} (bounty {task['bounty']})")
    claimed = client.claim_task(agent, task["id"])
    print(f"claimed: status={claimed['status']}")

    print("\n== submitting an answer ==")
    # A real agent computes its actual answer here. This example has no
    # way to know a hash_match task's hidden target, a consensus task's
    # right answer, or how to satisfy a disputable task's description --
    # it submits a placeholder purely to demonstrate the call, and the
    # hub will very likely reject it (reopening the task, no harm done).
    placeholder_output = "replace-this-with-your-actual-computed-answer"
    result = client.submit_task(agent, task["id"], placeholder_output)
    print(f"submit result: {result}")

    time.sleep(1)
    final_reputation = client.get_reputation(agent.pubkey_hex)
    print(f"\nfinal reputation: {final_reputation}")
    print("\nRun this script again with the same --key-file to see this identity's")
    print("reputation and balance still there -- the hub remembers it by pubkey,")
    print("this script only ever needed to remember the private key.")


if __name__ == "__main__":
    main()
