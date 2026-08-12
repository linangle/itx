import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as hub from "../../lib/hub";
import TasksPage from "./TasksPage";

vi.mock("../../lib/hub");

const POSTER = "02" + "a".repeat(64);

function task(overrides: Partial<hub.TaskDto> = {}): hub.TaskDto {
  return {
    id: "task-1",
    description: "Transcribe an interview",
    bounty: 1_000,
    status: "Open",
    poster: POSTER,
    claimant: null,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    capabilities: ["transcription"],
    created_at: new Date().toISOString(),
    kind: "hash_match",
    ...overrides,
  } as hub.TaskDto;
}

function show(tasks: hub.TaskDto[], route = "/tasks") {
  vi.mocked(hub.listAllTasks).mockResolvedValue({
    items: tasks,
    total: tasks.length,
    complete: true,
  });
  return render(
    <MemoryRouter initialEntries={[route]}>
      <TasksPage />
    </MemoryRouter>,
  );
}

/** The row cells, minus the header. */
async function rows() {
  const table = await screen.findByRole("table");
  return within(table)
    .getAllByRole("row")
    .slice(1)
    .map((row) =>
      within(row)
        .getAllByRole("cell")
        .map((cell) => cell.textContent),
    );
}

describe("terminal TasksPage", () => {
  beforeEach(() => {
    vi.mocked(hub.listLatestTasks).mockResolvedValue([]);
    // Poster names, resolved a page at a time. Empty by default so rows
    // render as bare keys; the naming case has its own test.
    vi.mocked(hub.getNames).mockResolvedValue(new Map());
    // `/board/summary` is what the sector and market pickers list. Every
    // test here supplies it explicitly; the fallback path (an older hub
    // that 404s the route) has its own case at the bottom.
    vi.mocked(hub.getBoardSummary).mockResolvedValue({
      first_task_at: new Date(Date.now() - 86_400_000).toISOString(),
      window_ms: 1,
      buckets: 1,
      total_tasks: 0,
      totals: {
        open_tasks: 0,
        open_bounty: 0,
        paid_tasks: 0,
        paid_bounty: 0,
        posted_series: [],
      },
      kinds: [],
      capabilities: ["transcription", "python", "coding/rust"].map((capability) => ({
        capability,
        open: 1,
        open_bounty: 1,
        posted: 1,
        posted_series: [],
        bounty_series: [],
      })),
    });
  });

  it("shows a task's sector beside its market", async () => {
    show([task({ capabilities: ["transcription"] })]);

    expect(await rows()).toEqual([expect.arrayContaining(["data", "transcription"])]);
  });

  it("reads a namespaced tag as its own sector and drops the prefix from the market", async () => {
    show([task({ capabilities: ["coding/rust"] })]);

    const [cells] = await rows();
    expect(cells).toContain("coding");
    // The label loses the sector it already sits next to; the full tag
    // is still what the hub would be asked to filter on.
    expect(cells).toContain("rust");
    expect(cells).not.toContain("coding/rust");
  });

  it("filters the table by sector", async () => {
    show(
      [
        task({ id: "a", description: "Transcribe", capabilities: ["transcription"] }),
        task({ id: "b", description: "Port a crate", capabilities: ["rust"] }),
      ],
      "/tasks?sector=coding",
    );

    const table = await screen.findByRole("table");
    expect(within(table).getByText("Port a crate")).toBeInTheDocument();
    expect(within(table).queryByText("Transcribe")).not.toBeInTheDocument();
  });

  it("titles a kind page in plain language and explains how it settles", async () => {
    show(
      [task({ kind: "disputable", answer: null, dispute_deadline: null, dispute: null })],
      "/tasks?kind=disputable",
    );

    expect(
      await screen.findByRole("heading", { name: "challenge window tasks" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/stands unless someone challenges it/)).toBeInTheDocument();
    // The protocol name is demoted, not hidden -- someone reading the
    // hub's API has to be able to line the two up.
    expect(screen.getByText("disputable")).toBeInTheDocument();
  });

  it("says how verification differs from status", async () => {
    show([task()]);

    expect(
      await screen.findByText(/status is where it has reached in that process/),
    ).toBeInTheDocument();
  });

  it("browses markets from the picker without typing", async () => {
    const user = userEvent.setup();
    show([task()]);
    await screen.findByRole("table");

    await user.click(screen.getByRole("button", { name: "Show Filter by market options" }));
    const list = screen.getByRole("listbox", { name: "Filter by market" });
    // Sourced from `/board/summary`, so it offers markets no task on
    // this page carries.
    expect(within(list).getByRole("option", { name: "python" })).toBeInTheDocument();

    await user.click(within(list).getByRole("option", { name: "python" }));
    // Committing a market re-requests the board with it applied.
    await waitFor(() =>
      expect(vi.mocked(hub.listAllTasks)).toHaveBeenLastCalledWith({
        status: "all",
        capability: "python",
      }),
    );
  });

  it("narrows the market picker to the chosen sector", async () => {
    const user = userEvent.setup();
    show([task({ capabilities: ["cpp"] })], "/tasks?sector=coding");
    await screen.findByRole("table");

    await user.click(screen.getByRole("button", { name: "Show Filter by market options" }));
    const list = screen.getByRole("listbox", { name: "Filter by market" });
    expect(within(list).getByRole("option", { name: "rust" })).toBeInTheDocument();
    expect(within(list).queryByRole("option", { name: "transcription" })).toBeNull();
  });

  it("filters on a typed tag the picker has never heard of", async () => {
    const user = userEvent.setup();
    show([task()]);
    await screen.findByRole("table");

    await user.type(screen.getByRole("combobox", { name: "Filter by market" }), "quantum{Enter}");

    await waitFor(() =>
      expect(vi.mocked(hub.listAllTasks)).toHaveBeenLastCalledWith({
        status: "all",
        capability: "quantum",
      }),
    );
  });

  it("labels a poster with the name the hub assigned it", async () => {
    vi.mocked(hub.getNames).mockResolvedValue(new Map([[POSTER, "SwiftWarlock"]]));
    show([task()]);

    expect(await screen.findByText("SwiftWarlock")).toBeInTheDocument();
    // The key stays on the row: the name is the hub's label, the key is
    // the identity.
    expect(screen.getByText(/02aa/)).toBeInTheDocument();
  });

  it("asks for names only for the posters on the page", async () => {
    const other = "03" + "b".repeat(64);
    show([task({ id: "a" }), task({ id: "b", poster: other })]);
    await screen.findByRole("table");

    await waitFor(() =>
      expect(vi.mocked(hub.getNames)).toHaveBeenCalledWith([POSTER, other]),
    );
  });

  it("falls back to the key for a poster the hub has never named", async () => {
    vi.mocked(hub.getNames).mockResolvedValue(new Map([[POSTER, null]]));
    show([task()]);

    const table = await screen.findByRole("table");
    expect(within(table).getByText("02aaaa…aaaa")).toBeInTheDocument();
  });

  it("sorts by a column when its header is clicked, and reverses on a second click", async () => {
    const user = userEvent.setup();
    show([
      task({ id: "cheap", description: "Cheap task", bounty: 10 }),
      task({ id: "rich", description: "Rich task", bounty: 9_000 }),
    ]);
    await screen.findByRole("table");

    const descriptions = () =>
      within(screen.getByRole("table"))
        .getAllByRole("row")
        .slice(1)
        .map((row) => within(row).getAllByRole("cell")[0].textContent);

    await user.click(screen.getByRole("button", { name: /Bounty/ }));
    expect(descriptions()).toEqual(["Cheap task", "Rich task"]);
    expect(screen.getByRole("columnheader", { name: /Bounty/ })).toHaveAttribute(
      "aria-sort",
      "ascending",
    );

    await user.click(screen.getByRole("button", { name: /Bounty/ }));
    expect(descriptions()).toEqual(["Rich task", "Cheap task"]);
    expect(screen.getByRole("columnheader", { name: /Bounty/ })).toHaveAttribute(
      "aria-sort",
      "descending",
    );
  });

  it("reads its ordering out of the URL", async () => {
    show(
      [
        task({ id: "a", description: "Zebra" }),
        task({ id: "b", description: "Aardvark" }),
      ],
      "/tasks?sort=task&dir=asc",
    );

    const table = await screen.findByRole("table");
    const first = within(table).getAllByRole("row")[1];
    expect(within(first).getAllByRole("cell")[0]).toHaveTextContent("Aardvark");
  });

  it("still offers the tags in hand when the hub has no /board/summary", async () => {
    const user = userEvent.setup();
    vi.mocked(hub.getBoardSummary).mockRejectedValue(new Error("/board/summary -> HTTP 404"));
    show([task({ capabilities: ["ocr"] })]);
    await screen.findByRole("table");

    await user.click(screen.getByRole("button", { name: "Show Filter by sector options" }));
    const list = screen.getByRole("listbox", { name: "Filter by sector" });
    expect(within(list).getByRole("option", { name: "data" })).toBeInTheDocument();
  });
});
