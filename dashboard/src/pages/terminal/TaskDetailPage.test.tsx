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
  };
}

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

it("says a paid open-ended task was paid on submission, with no challenge row", async () => {
  vi.mocked(hub.getTask).mockResolvedValue({
    ...fresh(),
    status: "Paid",
    answer: "the endpoints, documented",
  } as hub.TaskDto);
  show();
  expect(
    await screen.findByText("the answer is paid when it is submitted. the poster cannot reject it."),
  ).toBeInTheDocument();
  expect(screen.queryByText("if challenged")).toBeNull();
});
