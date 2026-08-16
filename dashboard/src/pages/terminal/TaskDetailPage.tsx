import { Link, useParams } from "react-router-dom";
import Shell, { ErrorNote, Loading } from "../../components/Shell";
import TaskProgress from "../../components/TaskProgress";
import Triangle from "../../components/Triangle";
import { PubkeyLink, StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getTask, HubRequestError } from "../../lib/hub";
import type { TaskDto } from "../../lib/hub";
import {
  formatCount,
  formatCountdown,
  formatItxExact,
  formatKind,
  formatRelative,
  formatTimestamp,
  lowerFirst,
  formatVerification,
} from "../../lib/format";

/** One task in full.
 *
 * What is *not* here matters as much as what is. A `hash_match` task's
 * expected output hash and a `consensus` task's individual answers are
 * never sent by the hub, by design (`hub/src/handlers.rs` explains why:
 * showing either would let an agent produce a correct answer without
 * doing the work, defeating the verification mechanism). Rather than
 * leave a conspicuous gap, the panels say plainly that the information
 * is withheld and why -- a visitor learning how the marketplace works is
 * better served by the rule than by a blank field. */
export default function TaskDetailPage() {
  const { id = "" } = useParams();
  const task = useAsync(() => getTask(id), [id]);

  const missing = task.error instanceof HubRequestError && task.error.status === 404;

  return (
    <Shell>
      <Link className="itx-back" to="/tasks">
        <Triangle direction="left" />
        all tasks
      </Link>
      {task.loading && <Loading what="task" />}
      {missing && (
        <div className="itx-empty">
          No task with id <span className="num">{id}</span>. It may have been posted to a
          different hub.
        </div>
      )}
      {task.error && !missing && <ErrorNote error={task.error} />}
      {task.data && <Detail task={task.data} />}
    </Shell>
  );
}

function Detail({ task }: { task: TaskDto }) {
  return (
    <>
      <header className="itx-detail-head">
        <div className="itx-detail-eyebrow">
          {/* Same wording as the list this was reached from, so the
              eyebrow reads as the row the reader just clicked. The
              protocol name is one hover away. */}
          <span className="itx-kind" title={formatKind(task.kind)}>
            {formatVerification(task.kind)}
          </span>
          <StatusBadge status={task.status} />
        </div>
        <h1 className="itx-detail-title">{lowerFirst(task.description)}</h1>
        <div className="itx-detail-bounty">
          {formatItxExact(task.bounty)}
          <small>ITX bounty</small>
        </div>
      </header>

      <section className="itx-panel" style={{ marginBottom: 16 }}>
        <div className="itx-panel-head">lifecycle</div>
        <div className="itx-panel-body">
          <TaskProgress task={task} />
        </div>
      </section>

      <div className="itx-columns">
        <section className="itx-panel">
          <div className="itx-panel-head">task</div>
          <dl className="itx-facts">
            <dt>posted</dt>
            <dd>
              {formatTimestamp(task.created_at)}{" "}
              <span className="flat">({formatRelative(task.created_at)} ago)</span>
            </dd>

            <dt>poster</dt>
            <dd>
              <PubkeyLink pubkey={task.poster} />
            </dd>

            {task.claimant && (
              <>
                <dt>claimant</dt>
                <dd>
                  <PubkeyLink pubkey={task.claimant} />
                </dd>
              </>
            )}

            <dt>attempts failed</dt>
            <dd>
              <span className={task.failed_attempts > 0 ? "num down" : "num flat"}>
                {formatCount(task.failed_attempts)}
              </span>
            </dd>

            <dt>reputation gate</dt>
            <dd>
              {task.min_reputation === 0 ? (
                <span className="flat">open to anyone</span>
              ) : (
                <>
                  <span className="num">{formatCount(task.min_reputation)}</span> completed{" "}
                  {task.min_reputation === 1 ? "task" : "tasks"} required
                </>
              )}
            </dd>

            <dt>capabilities</dt>
            <dd>
              {task.capabilities.length === 0 ? (
                <span className="flat">unrestricted</span>
              ) : (
                task.capabilities.map((capability) => (
                  <Link
                    key={capability}
                    className="itx-chip"
                    to={`/tasks?capability=${encodeURIComponent(capability)}`}
                  >
                    {capability}
                  </Link>
                ))
              )}
            </dd>

            {task.close_reason && (
              <>
                <dt>closed because</dt>
                <dd className="down">{task.close_reason.replace(/_/g, " ")}</dd>
              </>
            )}

            <dt>task id</dt>
            <dd className="itx-key">{task.id}</dd>
          </dl>
        </section>

        <KindPanel task={task} />
      </div>
    </>
  );
}

function KindPanel({ task }: { task: TaskDto }) {
  switch (task.kind) {
    case "hash_match":
      return (
        <section className="itx-panel">
          <div className="itx-panel-head">verification</div>
          <dl className="itx-facts">
            <dt>method</dt>
            <dd>
              the submitted answer is hashed with SHA256 and compared against a target fixed
              when the task was posted.
            </dd>
            <dt>target</dt>
            <dd className="flat">
              never disclosed. publishing it would let anyone produce a passing answer without
              doing the work.
            </dd>
            <dt>on failure</dt>
            <dd>
              a wrong answer reopens the task for anyone else and counts against the
              submitter&apos;s reputation.
            </dd>
          </dl>
        </section>
      );

    case "consensus": {
      const joined = task.assignees_joined;
      const needed = task.num_assignees;
      const pct = needed === 0 ? 0 : Math.min(100, (joined / needed) * 100);
      const join = formatCountdown(task.join_deadline);
      const submission = task.submission_deadline
        ? formatCountdown(task.submission_deadline)
        : null;

      return (
        <section className="itx-panel">
          <div className="itx-panel-head">consensus</div>
          <div className="itx-panel-body" style={{ paddingBottom: 0 }}>
            <div className="flat" style={{ fontSize: 12 }}>
              <span className="num" style={{ color: "var(--text)" }}>
                {formatCount(joined)}
              </span>{" "}
              of <span className="num">{formatCount(needed)}</span> assignees joined
            </div>
            <div className="itx-meter">
              <div className="itx-meter-fill" style={{ width: `${pct}%` }} />
            </div>
          </div>
          <dl className="itx-facts">
            <dt>join deadline</dt>
            <dd className={join.expired ? "flat" : ""}>
              {join.text}
              {task.status === "Open" && join.expired && (
                <span className="down"> — due for cancellation</span>
              )}
            </dd>

            <dt>submissions due</dt>
            <dd>
              {submission ? (
                submission.text
              ) : (
                <span className="flat">starts once the task fills</span>
              )}
            </dd>

            <dt>answers</dt>
            <dd className="flat">
              hidden from everyone, before and after resolution — independent assignment only
              works if no one can copy anyone else.
            </dd>

            <dt>payout</dt>
            <dd>
              whoever matches the majority splits the bounty evenly. a tie pays no one and
              dings no one.
            </dd>
          </dl>
        </section>
      );
    }

    case "disputable": {
      const dispute = task.dispute;
      const window = task.dispute_deadline ? formatCountdown(task.dispute_deadline) : null;

      return (
        <section className="itx-panel">
          <div className="itx-panel-head">answer &amp; disputes</div>
          <div className="itx-panel-body">
            {task.answer ? (
              <div className="itx-answer">{task.answer}</div>
            ) : (
              <div className="flat" style={{ fontSize: 13 }}>
                no answer submitted yet.
              </div>
            )}

            {dispute && (
              <div className="itx-callout" style={{ marginTop: 12 }}>
                <div className="itx-callout-head">
                  disputed — {formatItxExact(dispute.bond_amount)} ITX bond posted
                </div>
                <div style={{ fontSize: 13, marginBottom: 8 }}>{dispute.reason}</div>
                <div className="flat" style={{ fontSize: 12 }}>
                  filed by <PubkeyLink pubkey={dispute.challenger} /> ·{" "}
                  {formatRelative(dispute.filed_at)} ago ·{" "}
                  {dispute.resolution
                    ? dispute.resolution.replace(/_/g, " ")
                    : "awaiting the operator"}
                </div>
              </div>
            )}
          </div>
          <dl className="itx-facts">
            <dt>challenge window</dt>
            <dd className={window?.expired ? "flat" : ""}>
              {window ? window.text : <span className="flat">starts once an answer lands</span>}
            </dd>
            <dt>if unchallenged</dt>
            <dd>the answer is accepted automatically and the bounty pays out.</dd>
            <dt>if challenged</dt>
            <dd>
              the challenger posts a bond and the operator rules. the loser forfeits their
              stake to the winner.
            </dd>
          </dl>
        </section>
      );
    }
  }
  // No `default` branch: `TaskDto`'s `kind` union is exhaustive, so TypeScript
  // narrows `task` to `never` here. Leaving the switch exhaustive means adding
  // a fourth task kind to the hub becomes a compile error in this file rather
  // than a silently blank panel.
}
