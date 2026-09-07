import type { TaskDto, TaskStatus } from "../lib/hub";

/** Where a task sits in its lifecycle.
 *
 * Each kind has a genuinely different sequence, so there is no single
 * shared stepper: a `hash_match` task is claimed by one agent and
 * verified on submission, a `consensus` task fills with N assignees
 * before anyone's answer counts, and a `disputable` task's answer has to
 * survive a challenge window.
 *
 * `Closed` is deliberately *not* a step. It is an off-path terminal state
 * that can interrupt the sequence at different points depending on kind,
 * so it renders as a derailed final marker rather than as progress.
 * `PayoutFailed` is the other one, for the same reason and with its own
 * label -- a task nobody was paid for did not reach "paid", and saying so
 * is the whole point of the status existing.
 *
 * `Submitted` shares the verified/resolved/settled step rather than
 * getting one of its own. That is the honest placement: the work is done
 * and the money is on the wire, so the step before "paid" is exactly
 * where it belongs, and "paid" stays reserved for a payout confirmed on
 * chain. Without it here a settling task showed no progress at all.
 */
const SEQUENCES: Record<TaskDto["kind"], { label: string; statuses: TaskStatus[] }[]> = {
  hash_match: [
    { label: "posted", statuses: ["Open"] },
    { label: "claimed", statuses: ["Claimed"] },
    { label: "verified", statuses: ["Verified", "Submitted"] },
    { label: "paid", statuses: ["Paid"] },
  ],
  consensus: [
    { label: "posted", statuses: ["Open"] },
    { label: "filled", statuses: ["Claimed"] },
    { label: "resolved", statuses: ["Verified", "Submitted"] },
    { label: "paid", statuses: ["Paid"] },
  ],
  disputable: [
    { label: "posted", statuses: ["Open"] },
    { label: "answered", statuses: ["AwaitingDispute"] },
    { label: "challenged", statuses: ["Disputed"] },
    { label: "settled", statuses: ["Verified", "Submitted", "Paid"] },
  ],
};

export default function TaskProgress({ task }: { task: TaskDto }) {
  const steps = SEQUENCES[task.kind];
  const currentIndex = steps.findIndex((step) => step.statuses.includes(task.status));
  // Both off-path terminals derail the last marker; only the label
  // differs, and the difference matters -- "closed" means nobody was
  // owed anything, "payout failed" means somebody still is.
  const derailedLabel =
    task.status === "Closed" ? "closed" : task.status === "PayoutFailed" ? "payout failed" : null;
  const closed = derailedLabel !== null;

  return (
    <div>
      <div className="itx-steps">
        {steps.map((step, index) => {
          // A closed or payout-failed task never reached its own final
          // step, so nothing is marked current -- the last marker is
          // flagged derailed instead.
          const derailed = closed && index === steps.length - 1;
          const reached = !closed && currentIndex >= 0 && index < currentIndex;
          const current = !closed && index === currentIndex;
          const className = derailed
            ? "derailed"
            : current
              ? "current"
              : reached
                ? "reached"
                : "";
          return (
            <div key={step.label} className={`itx-step ${className}`}>
              <div className="itx-step-bar" />
              <div className="itx-step-label">{derailed ? derailedLabel : step.label}</div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
