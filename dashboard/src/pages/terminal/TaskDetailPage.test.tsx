import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, expect, it, vi } from "vitest";
import * as hub from "../../lib/hub";
import TaskDetailPage from "./TaskDetailPage";

vi.mock("../../lib/hub");
vi.mock("../../components/Shell", () => ({
  default: ({ children }: { children: React.ReactNode }) => children,
  Loading: () => null,
  ErrorNote: () => null,
  Empty: ({ children }: { children: React.ReactNode }) => children,
}));

const POSTER = "02" + "a".repeat(60) + "cf0b";

function fresh(): hub.TaskDto {
  return {
    id: "t1",
    description: "Document a REST API from its OpenAPI spec",
    bounty: 3_391_881_997,
    status: "Open",
    poster: POSTER,
    claimant: null,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    capabilities: [],
    created_at: new Date().toISOString(),
    settled_at: null,
    kind: "disputable",
    answer: null,
    dispute_deadline: null,
    dispute: null,
    submission_deadline: new Date(Date.now() + 3.5 * 3_600_000).toISOString(),
    answer_count: 0,
    answers: null,
    pick_deadline: null,
    picked: null,
    split: false,
  };
}

const ANSWERER = "03" + "b".repeat(60) + "d00d";

function show() {
  return render(
    <MemoryRouter initialEntries={["/tasks/t1"]}>
      <Routes>
        <Route path="/tasks/:id" element={<TaskDetailPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  vi.mocked(hub.getTask).mockResolvedValue(fresh());
  vi.mocked(hub.getNames).mockResolvedValue(new Map());
});

it("names the poster the way the list does, with the key kept beside it", async () => {
  vi.mocked(hub.getNames).mockResolvedValue(new Map([[POSTER, "SourHeron"]]));
  show();
  const poster = await screen.findByRole("link", { name: /SourHeron/ });
  expect(poster).toHaveAttribute("href", `/agents/${POSTER}`);
  expect(poster).toHaveTextContent("02aa…cf0b");
  // Waited for, not asserted on the spot: the names request is made in
  // an effect that runs after the task's text is on the page, and on a
  // loaded CI runner the text can be found before that effect has run.
  await waitFor(() => expect(vi.mocked(hub.getNames)).toHaveBeenCalledWith([POSTER]));
});

it("keeps the key as the label for a poster the hub has not named", async () => {
  show();
  const poster = await screen.findByRole("link", { name: /02aaaa…cf0b/ });
  expect(poster).toHaveAttribute("href", `/agents/${POSTER}`);
});

it("dates a fresh task as just now, not just now ago", async () => {
  show();
  expect(await screen.findByText("(just now)")).toBeInTheDocument();
  expect(screen.queryByText(/just now ago/)).not.toBeInTheDocument();
});

it("keeps an open contest's answers hidden and says how many there are", async () => {
  vi.mocked(hub.getTask).mockResolvedValue({ ...fresh(), answer_count: 3 } as hub.TaskDto);
  show();
  expect(
    await screen.findByText(/^3 answers so far, hidden from everyone, the poster included/),
  ).toBeInTheDocument();
  expect(screen.getByText("submissions close")).toBeInTheDocument();
  expect(screen.getByText("in 3h")).toBeInTheDocument();
  expect(screen.queryByText("pick due")).toBeNull();
});

it("shows a closed contest's answers and when the pick is due", async () => {
  vi.mocked(hub.getTask).mockResolvedValue({
    ...fresh(),
    status: "AwaitingPick",
    answer_count: 1,
    answers: [{ pubkey: ANSWERER, answer: "the endpoints, documented", submitted_at: "2026-09-16T12:00:00Z" }],
    pick_deadline: new Date(Date.now() + 2.5 * 3_600_000).toISOString(),
  } as hub.TaskDto);
  show();
  expect(await screen.findByText("the endpoints, documented")).toBeInTheDocument();
  expect(screen.getByText("pick due")).toBeInTheDocument();
  expect(screen.getByText("in 2h")).toBeInTheDocument();
  expect(screen.getByText(/waiting for the poster to pick/)).toBeInTheDocument();
  expect(screen.queryByText("submissions close")).toBeNull();
  // Waited for, for the reason above -- this is the assertion that caught
  // it, failing in CI with the poster-only calls from earlier tests and
  // no call yet for this one.
  await waitFor(() =>
    expect(vi.mocked(hub.getNames)).toHaveBeenCalledWith([POSTER, ANSWERER]),
  );
});

it("says which answer a settled contest picked, or that it was split", async () => {
  const settled = {
    ...fresh(),
    status: "Paid",
    answer_count: 1,
    answers: [{ pubkey: ANSWERER, answer: "the endpoints, documented", submitted_at: "2026-09-16T12:00:00Z" }],
    pick_deadline: "2026-09-16T14:00:00Z",
  };
  vi.mocked(hub.getTask).mockResolvedValue({ ...settled, picked: ANSWERER } as hub.TaskDto);
  const { unmount } = show();
  expect(await screen.findByText(/paid the bounty and credited as a completed task/)).toBeInTheDocument();
  expect(screen.getByText("· picked")).toBeInTheDocument();
  unmount();

  vi.mocked(hub.getTask).mockResolvedValue({ ...settled, split: true } as hub.TaskDto);
  const split = show();
  expect(await screen.findByText(/split evenly among every answer/)).toBeInTheDocument();
  expect(screen.getByText(/every share is paid/)).toBeInTheDocument();
  expect(screen.queryByText("· picked")).toBeNull();
  split.unmount();
});

it("does not call a contest paid before its payment lands", async () => {
  const settling = {
    ...fresh(),
    answer_count: 1,
    answers: [{ pubkey: ANSWERER, answer: "the endpoints, documented", submitted_at: "2026-09-16T12:00:00Z" }],
    pick_deadline: "2026-09-16T14:00:00Z",
  };
  vi.mocked(hub.getTask).mockResolvedValue({ ...settling, status: "Submitted", picked: ANSWERER } as hub.TaskDto);
  const { unmount } = show();
  expect(await screen.findByText(/the bounty is on its way/)).toBeInTheDocument();
  expect(screen.queryByText(/paid the bounty/)).toBeNull();
  unmount();

  vi.mocked(hub.getTask).mockResolvedValue({ ...settling, status: "PayoutFailed", split: true } as hub.TaskDto);
  show();
  expect(await screen.findByText(/the payout failed, so the shares are still owed/)).toBeInTheDocument();
  expect(screen.queryByText(/every share is paid/)).toBeNull();
});

function consensus(): hub.TaskDto {
  return {
    ...fresh(),
    status: "Claimed",
    kind: "consensus",
    num_assignees: 3,
    assignees_joined: 3,
    join_deadline: "2026-09-16T12:00:00Z",
    submission_deadline: new Date(Date.now() + 2.5 * 3_600_000).toISOString(),
    winning_answer: null,
  } as hub.TaskDto;
}

it("keeps a consensus task's answers hidden until it resolves", async () => {
  vi.mocked(hub.getTask).mockResolvedValue(consensus());
  show();
  expect(await screen.findByText(/hidden from everyone until the task resolves/)).toBeInTheDocument();
  expect(screen.queryByText("winning answer")).toBeNull();
});

it("shows a resolved consensus task's winning answer, and says when there is none", async () => {
  vi.mocked(hub.getTask).mockResolvedValue({ ...consensus(), status: "Paid", winning_answer: "positive" } as hub.TaskDto);
  const { unmount } = show();
  expect(await screen.findByText("positive")).toBeInTheDocument();
  expect(screen.getByText("winning answer")).toBeInTheDocument();
  expect(screen.getByText(/only the winning answer is shown/)).toBeInTheDocument();
  unmount();

  vi.mocked(hub.getTask).mockResolvedValue({ ...consensus(), status: "Closed", close_reason: "no_majority" } as hub.TaskDto);
  const tied = show();
  expect(await screen.findByText(/no answer won a majority/)).toBeInTheDocument();
  expect(screen.queryByText("winning answer")).toBeNull();
  tied.unmount();

  vi.mocked(hub.getTask).mockResolvedValue({ ...consensus(), status: "Closed", close_reason: "understaffed" } as hub.TaskDto);
  show();
  expect(await screen.findByText(/the task closed before it resolved/)).toBeInTheDocument();
});
