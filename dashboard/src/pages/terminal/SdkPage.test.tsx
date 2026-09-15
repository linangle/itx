import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import SdkPage from "./SdkPage";
import { AGENT, COMMAND, INSTALL, MCP_CLAUDE_CODE, README_HUB, forThisHub } from "./sdkSamples";
// The README itself, as text: Vite's `?raw` rather than `node:fs`, since
// the app's tsconfig knows Vite's module types and not Node's.
import readme from "../../../../agent-sdk-py/README.md?raw";

vi.mock("../../components/Shell", () => ({ default: ({ children }: { children: React.ReactNode }) => children }));
afterEach(() => document.querySelector('meta[name="itx-hub-url"]')?.remove());

/** Every fenced block in the SDK README, by the line it opens with. */
function readmeBlocks(): Map<string, string> {
  const blocks = new Map<string, string>();
  for (const match of readme.matchAll(/```\w+\n([\s\S]*?)```/g)) {
    const body = match[1].replace(/\n$/, "");
    blocks.set(body.split("\n")[0], body);
  }
  return blocks;
}

describe("the samples are the README's", () => {
  const blocks = readmeBlocks();
  it.each([
    ["install", INSTALL],
    ["worked agent", AGENT],
    ["itx-agent command", COMMAND],
    ["claude code", MCP_CLAUDE_CODE],
  ])("%s", (_name, sample) => {
    expect(blocks.get(sample.split("\n")[0])).toBe(sample);
  });
});

it("names this deployment's hub in every sample, not the README's default", () => {
  const meta = document.createElement("meta");
  meta.name = "itx-hub-url";
  meta.content = "https://hub.market.test/";
  document.head.append(meta);
  render(<MemoryRouter><SdkPage /></MemoryRouter>);

  const shown = Array.from(document.querySelectorAll("pre")).map((pre) => pre.textContent);
  expect(shown).toContain(forThisHub(AGENT, "https://hub.market.test"));
  expect(shown).toContain(forThisHub(COMMAND, "https://hub.market.test"));
  expect(shown.join("\n")).not.toContain(README_HUB);
  expect(screen.getByRole("link", { name: "https://hub.market.test/llms.txt" }))
    .toHaveAttribute("href", "https://hub.market.test/llms.txt");
  expect(screen.getByRole("button", { name: "copy the worked agent" })).toBeInTheDocument();
});
