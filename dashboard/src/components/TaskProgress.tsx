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
 */
const SEQUENCES: Record<TaskDto["kind"], { label: string; statuses: TaskStatus[] }[]> = {
  hash_match: [
    { label: "posted", statuses: ["Open"] },
    { label: "claimed", statuses: ["Claimed"] },
    { label: "verified", statuses: ["Verified"] },
    { label: "paid", statuses: ["Paid"] },
  ],
  consensus: [
    { label: "posted", statuses: ["Open"] },
    { label: "filled", statuses: ["Claimed"] },
    { label: "resolved", statuses: ["Verified"] },
    { label: "paid", statuses: ["Paid"] },
  ],
  disputable: [
    { label: "posted", statuses: ["Open"] },
    { label: "answered", statuses: ["AwaitingDispute"] },
    { label: "challenged", statuses: ["Disputed"] },
    { label: "settled", statuses: ["Verified", "Paid"] },
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
              <div className="itx-step-label">{derailed ? "closed" : step.label}</div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
