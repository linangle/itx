import type { TaskDto, TaskStatus } from "../lib/hub";

/** Where a task sits in its lifecycle.
 *
 * Each kind has a genuinely different sequence, so there's no single
 * shared stepper: a `hash_match` task is claimed by one agent and
 * verified on submission, a `consensus` task fills with N assignees
 * before anyone's answer counts, and a `disputable` task's answer has to
 * survive a challenge window. Showing all three as "Open → Claimed →
 * Paid" would misdescribe two of them.
 *
 * `Closed` is deliberately *not* a step. It's an off-path terminal state
 * (no majority, under-subscribed, or cancelled) that can interrupt the
 * sequence at different points depending on kind, so it renders as a
 * derailed final marker rather than pretending to be progress.
 */
const SEQUENCES: Record<TaskDto["kind"], { label: string; statuses: TaskStatus[] }[]> = {
  hash_match: [
    { label: "Posted", statuses: ["Open"] },
    { label: "Claimed", statuses: ["Claimed"] },
    { label: "Verified", statuses: ["Verified"] },
    { label: "Paid", statuses: ["Paid"] },
  ],
  consensus: [
    { label: "Posted", statuses: ["Open"] },
    { label: "Filled", statuses: ["Claimed"] },
    { label: "Resolved", statuses: ["Verified"] },
    { label: "Paid", statuses: ["Paid"] },
  ],
  disputable: [
    { label: "Posted", statuses: ["Open"] },
    { label: "Answered", statuses: ["AwaitingDispute"] },
    { label: "Challenged", statuses: ["Disputed"] },
    { label: "Settled", statuses: ["Verified", "Paid"] },
  ],
};

export default function TaskProgress({ task }: { task: TaskDto }) {
  const steps = SEQUENCES[task.kind];
  const currentIndex = steps.findIndex((step) => step.statuses.includes(task.status));
  const closed = task.status === "Closed";

  return (
    <div>
      <div className="itx-steps">
        {steps.map((step, index) => {
          // A Closed task never reached its own final step, so nothing is
          // marked current -- the last marker is flagged derailed instead.
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
              <div className="itx-step-label">{derailed ? "Closed" : step.label}</div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
