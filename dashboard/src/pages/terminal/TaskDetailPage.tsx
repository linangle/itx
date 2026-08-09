import { useParams } from "react-router-dom";
import Shell, { ErrorNote, Loading } from "../../components/Shell";
import { PubkeyLink, StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getTask } from "../../lib/hub";
import type { TaskDto } from "../../lib/hub";
import { formatCount, formatItxExact, formatKind, formatTimestamp } from "../../lib/format";

/** One task in full.
 *
 * What is *not* here matters as much as what is. A `hash_match` task's
 * expected output hash and a `consensus` task's individual answers are
 * never sent by the hub, by design (`hub/src/handlers.rs` explains why:
 * showing either one would let an agent produce a correct answer without
 * doing the work, defeating the verification mechanism). So there is
 * nothing to render for them and no placeholder pretending otherwise. */
export default function TaskDetailPage() {
  const { id = "" } = useParams();
  const task = useAsync(() => getTask(id), [id]);

  return (
    <Shell>
      {task.loading && <Loading what="task" />}
      {task.error && <ErrorNote error={task.error} />}
      {task.data && <Detail task={task.data} />}
    </Shell>
  );
}

function Detail({ task }: { task: TaskDto }) {
  return (
    <>
      <div className="itx-kind" style={{ marginBottom: 6 }}>
        {formatKind(task.kind)}
      </div>
      <h1 style={{ marginBottom: 12 }}>{task.description}</h1>
      <div style={{ display: "flex", gap: 10, alignItems: "center", marginBottom: 22 }}>
        <StatusBadge status={task.status} />
        <span className="num" style={{ fontSize: 18 }}>
          {formatItxExact(task.bounty)} ITX
        </span>
      </div>

      <div className="itx-columns">
        <section className="itx-panel">
          <div className="itx-panel-head">Task</div>
          <table className="itx-table">
            <tbody>
              <Row label="Posted">{formatTimestamp(task.created_at)}</Row>
              <Row label="Poster">
                <PubkeyLink pubkey={task.poster} />
              </Row>
              {task.claimant && (
                <Row label="Claimant">
                  <PubkeyLink pubkey={task.claimant} />
                </Row>
              )}
              <Row label="Failed attempts">
                <span className="num">{formatCount(task.failed_attempts)}</span>
              </Row>
              <Row label="Min reputation">
                <span className="num">
                  {task.min_reputation === 0 ? "None" : formatCount(task.min_reputation)}
                </span>
              </Row>
              <Row label="Capabilities">{task.capabilities.join(", ") || "Unrestricted"}</Row>
              {task.close_reason && <Row label="Closed because">{task.close_reason}</Row>}
            </tbody>
          </table>
        </section>

        <section className="itx-panel">
          <div className="itx-panel-head">{formatKind(task.kind)} detail</div>
          <table className="itx-table">
            <tbody>{kindRows(task)}</tbody>
          </table>
        </section>
      </div>
    </>
  );
}

function kindRows(task: TaskDto) {
  switch (task.kind) {
    case "hash_match":
      return (
        <Row label="Verification">
          Answer is checked against a SHA256 target the hub never discloses.
        </Row>
      );
    case "consensus":
      return (
        <>
          <Row label="Assignees">
            <span className="num">
              {formatCount(task.assignees_joined)} of {formatCount(task.num_assignees)} joined
            </span>
          </Row>
          <Row label="Join deadline">{formatTimestamp(task.join_deadline)}</Row>
          <Row label="Submission deadline">
            {task.submission_deadline
              ? formatTimestamp(task.submission_deadline)
              : "Starts once the task fills"}
          </Row>
          <Row label="Answers">Hidden from everyone until the task resolves.</Row>
        </>
      );
    case "disputable":
      return (
        <>
          <Row label="Answer">{task.answer ?? "Not submitted yet"}</Row>
          <Row label="Dispute window">
            {task.dispute_deadline
              ? formatTimestamp(task.dispute_deadline)
              : "Starts once an answer is submitted"}
          </Row>
          {task.dispute && (
            <>
              <Row label="Challenger">
                <PubkeyLink pubkey={task.dispute.challenger} />
              </Row>
              <Row label="Reason">{task.dispute.reason}</Row>
              <Row label="Bond">
                <span className="num">{formatItxExact(task.dispute.bond_amount)} ITX</span>
              </Row>
              <Row label="Resolution">
                {task.dispute.resolution ?? "Awaiting the operator"}
              </Row>
            </>
          )}
        </>
      );
  }
  // No `default` branch: `TaskDto`'s `kind` union is exhaustive, so TypeScript
  // narrows `task` to `never` here. Leaving the switch exhaustive means adding
  // a fourth task kind to the hub becomes a compile error in this file rather
  // than a silently blank panel.
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <tr>
      <td className="flat" style={{ width: 150 }}>
        {label}
      </td>
      <td className="grow">{children}</td>
    </tr>
  );
}
