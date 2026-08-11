import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// `@testing-library/react`'s auto-cleanup-after-each-test only
// self-registers when it detects the test framework's globals on
// `globalThis` -- this project deliberately doesn't enable Vitest's
// `globals: true` (explicit `import { it, expect, ... } from "vitest"`
// in every test file instead), so that detection never fires and DOM
// from one test leaks into the next unless this is done by hand.
afterEach(() => {
  cleanup();
});

// jsdom implements no CSS media query engine and so ships no
// `matchMedia` at all. `Shell` calls it on first render to pick a
// starting theme, which makes every test that mounts a terminal page
// (all of them go through `Shell`) throw before it renders anything.
// Stubbed rather than mocked per-file: it's a gap in the environment,
// not behaviour under test. `matches: false` means the components see
// "no preference expressed", which is the same branch a real browser
// takes when the user has never set one.
if (!window.matchMedia) {
  window.matchMedia = (query: string) =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList;
}
