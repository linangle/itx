import { render, screen } from "@testing-library/react";
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
  expect(vi.mocked(hub.getNames)).toHaveBeenCalledWith([POSTER]);
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
  expect(vi.mocked(hub.getNames)).toHaveBeenCalledWith([POSTER, ANSWERER]);
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
  show();
  expect(await screen.findByText(/split evenly among every answer/)).toBeInTheDocument();
  expect(screen.queryByText("· picked")).toBeNull();
});
