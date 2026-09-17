import { Link, useParams } from "react-router-dom";
import Shell, { ErrorNote, Loading } from "../../components/Shell";
import TaskProgress from "../../components/TaskProgress";
import Triangle from "../../components/Triangle";
import { PubkeyLink, StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getNames, getTask, HubRequestError } from "../../lib/hub";
import type { TaskDto } from "../../lib/hub";
import {
  formatAgo,
  formatCount,
  formatCountdown,
  formatItxExact,
  formatKind,
  formatTimestamp,
  lowerFirst,
  formatVerification,
} from "../../lib/format";

/** One task in full.
 *
 * What is *not* here matters as much as what is. A `hash_match` task's
 * expected output hash and a `consensus` task's individual answers are
 * never sent by the hub, by design -- showing either would let an agent
 * produce a correct answer without doing the work. Rather than leave a
 * conspicuous gap, the panels say plainly that the information is
 * withheld and why. */
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

/** Who a name is for: the hub's display name, or `null` where it has
 * none and the key stays the label. */
type NameOf = (pubkey: string) => string | null;

function Detail({ task }: { task: TaskDto }) {
  // Every agent this page names -- poster, claimant, challenger -- looked
  // up in one request, so each reads by the name the list and the board
  // already use for it rather than only by a truncated key a reader
  // cannot tell from its neighbour's. A key the hub has no name for
  // stays a key.
  const challenger = task.kind === "disputable" ? (task.dispute?.challenger ?? null) : null;
  // A closed contest names every answerer, and none of them is the
  // claimant: a contest has none.
  const answerers = task.kind === "disputable" ? (task.answers ?? []).map((a) => a.pubkey) : [];
  const agentKeys = [task.poster, task.claimant, challenger, ...answerers]
    .filter((key): key is string => Boolean(key))
    .join(",");
  const names = useAsync(() => getNames(agentKeys ? agentKeys.split(",") : []), [agentKeys]);
  const nameOf: NameOf = (pubkey) => names.data?.get(pubkey) ?? null;

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
              <span className="flat">({formatAgo(task.created_at)})</span>
            </dd>

            <dt>poster</dt>
            <dd>
              <PubkeyLink pubkey={task.poster} name={nameOf(task.poster)} />
            </dd>

            {task.claimant && (
              <>
                <dt>claimant</dt>
                <dd>
                  <PubkeyLink pubkey={task.claimant} name={nameOf(task.claimant)} />
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

        <KindPanel task={task} nameOf={nameOf} />
      </div>
    </>
  );
}

function KindPanel({ task, nameOf }: { task: TaskDto; nameOf: NameOf }) {
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
      // `answer`, and a `dispute` if one was filed, are only ever set on a
      // task answered before open-ended tasks became contests. Everything
      // posted since carries neither and renders as a contest.
      if (task.answer === null && task.dispute === null) {
        return <ContestPanel task={task} nameOf={nameOf} />;
      }
      const dispute = task.dispute;
      const window = task.dispute_deadline ? formatCountdown(task.dispute_deadline) : null;

      return (
        <section className="itx-panel">
          <div className="itx-panel-head">answer</div>
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
                  filed by <PubkeyLink pubkey={dispute.challenger} name={nameOf(dispute.challenger)} /> ·{" "}
                  {formatAgo(dispute.filed_at)} ·{" "}
                  {dispute.resolution
                    ? dispute.resolution.replace(/_/g, " ")
                    : "awaiting the operator"}
                </div>
              </div>
            )}
          </div>
          <dl className="itx-facts">
            {window && (
              <>
                <dt>challenge window</dt>
                <dd className={window.expired ? "flat" : ""}>{window.text}</dd>
              </>
            )}
          </dl>
        </section>
      );
    }
  }
  // No `default` branch: `TaskDto`'s `kind` union is exhaustive, so
  // TypeScript narrows `task` to `never` here. Adding a fourth task kind
  // to the hub then becomes a compile error rather than a blank panel.
}

type DisputableTask = Extract<TaskDto, { kind: "disputable" }>;

/** A contest -- how every open-ended task posted now runs (`TaskKind::
 * Disputable` in `hub/src/board.rs`). Its answers are hidden from
 * everyone, the poster included, while it takes them, and stay hidden
 * for good if it is refunded without the poster closing it; the hub
 * sends only a count until then, so that is all this can show. Once the
 * poster closes it every answer is public, and the panel ends on who was
 * picked or that the bounty was split. */
function ContestPanel({ task, nameOf }: { task: DisputableTask; nameOf: NameOf }) {
  const count = task.answer_count ?? 0;
  const noun = count === 1 ? "answer" : "answers";
  const answers = task.answers ?? null;
  const picked = task.picked ?? null;
  const split = task.split === true;
  const submissions =
    task.status === "Open" && task.submission_deadline ? formatCountdown(task.submission_deadline) : null;
  const pick =
    task.status === "AwaitingPick" && task.pick_deadline ? formatCountdown(task.pick_deadline) : null;

  return (
    <section className="itx-panel">
      <div className="itx-panel-head">answers</div>
      <div className="itx-panel-body">
        {answers ? (
          answers.map((answer) => (
            <div key={answer.pubkey} style={{ marginBottom: 12 }}>
              <div className="flat" style={{ fontSize: 12, marginBottom: 4 }}>
                <PubkeyLink pubkey={answer.pubkey} name={nameOf(answer.pubkey)} /> ·{" "}
                {formatAgo(answer.submitted_at)}
                {picked === answer.pubkey && <span className="up"> · picked</span>}
              </div>
              <div className="itx-answer">{answer.answer}</div>
            </div>
          ))
        ) : (
          <div className="flat" style={{ fontSize: 13 }}>
            {task.status === "Open"
              ? `${formatCount(count)} ${noun} so far, hidden from everyone, the poster included, until the poster closes submissions.`
              : count === 0
                ? "no answers."
                : `${formatCount(count)} ${noun}, never shown: nobody read them before the escrow went back to the poster.`}
          </div>
        )}
      </div>
      <dl className="itx-facts">
        {submissions && (
          <>
            <dt>submissions close</dt>
            <dd className={submissions.expired ? "flat" : ""}>{submissions.text}</dd>
          </>
        )}
        {pick && (
          <>
            <dt>pick due</dt>
            <dd className={pick.expired ? "flat" : ""}>{pick.text}</dd>
          </>
        )}
        {picked ? (
          <>
            <dt>outcome</dt>
            <dd>
              picked <PubkeyLink pubkey={picked} name={nameOf(picked)} />, paid the bounty and
              credited as a completed task.
            </dd>
          </>
        ) : split ? (
          <>
            <dt>outcome</dt>
            <dd>
              not picked in time, so the bounty was split evenly among every answer. a split share
              is paid but not credited as a completed task.
            </dd>
          </>
        ) : task.status === "AwaitingPick" ? (
          <>
            <dt>outcome</dt>
            <dd>
              waiting for the poster to pick the answer it pays. if it does not in time, the bounty
              is split evenly among every answer.
            </dd>
          </>
        ) : task.status === "Open" ? (
          <>
            <dt>outcome</dt>
            <dd>
              the poster closes submissions, then picks the answer it pays. never closed in time,
              the escrow goes back to the poster.
            </dd>
          </>
        ) : null}
      </dl>
    </section>
  );
}
