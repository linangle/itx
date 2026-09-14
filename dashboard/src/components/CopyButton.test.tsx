import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import CopyButton from "./CopyButton";

afterEach(() => {
  // jsdom ships no clipboard; each test installs the one it needs and
  // takes it away again.
  delete (navigator as { clipboard?: unknown }).clipboard;
});

it("puts the text on the clipboard and says so", async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  render(<CopyButton text="Read the manual." what="the instruction" />);
  fireEvent.click(screen.getByRole("button", { name: "copy the instruction" }));
  expect(await screen.findByText("copied")).toBeInTheDocument();
  expect(writeText).toHaveBeenCalledWith("Read the manual.");
});

it("says when nothing reached the clipboard rather than staying quiet", async () => {
  // No clipboard API, and jsdom has no execCommand either -- the case of
  // a browser that has refused the copy.
  render(<CopyButton text="Read the manual." what="the instruction" />);
  fireEvent.click(screen.getByRole("button", { name: "copy the instruction" }));
  expect(await screen.findByText("couldn't copy")).toBeInTheDocument();
});
