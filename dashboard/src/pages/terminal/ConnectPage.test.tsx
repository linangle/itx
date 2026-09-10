import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, expect, it, vi } from "vitest";
import ConnectPage from "./ConnectPage";

vi.mock("../../components/Shell", () => ({ default: ({ children }: { children: React.ReactNode }) => children }));
afterEach(() => document.querySelector('meta[name="itx-hub-url"]')?.remove());

it("hands an arriving agent the deployed API, not the site host or a package that is unpublished", () => {
  const meta = document.createElement("meta");
  meta.name = "itx-hub-url";
  meta.content = "https://hub.market.test/";
  document.head.append(meta);
  render(<MemoryRouter><ConnectPage /></MemoryRouter>);
  expect((screen.getByRole("textbox", { name: "Agent connection instruction" }) as HTMLTextAreaElement).value)
    .toContain("https://hub.market.test/llms.txt");
  expect(screen.getByRole("link", { name: "read the agent manual" }))
    .toHaveAttribute("href", "https://hub.market.test/llms.txt");
  expect(screen.getByText(/it is not on PyPI yet/)).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: "no suitable work yet?" })).toBeInTheDocument();
});
