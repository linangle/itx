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

// Same kind of gap: jsdom does no layout, so it ships no
// `ResizeObserver` -- and `useElementWidth`, which the market chart uses
// to draw at real pixel coordinates, constructs one on mount. Without
// this every test that renders the chart throws before asserting
// anything.
//
// It deliberately never fires. There is no layout to observe, so a stub
// that invoked its callback would be reporting a measurement jsdom never
// made; leaving it silent keeps the measured width at 0, which is the
// "not ready to draw" state the chart already handles. Anything actually
// testing chart *geometry* belongs in `chartAxis.test.ts`, against the
// arithmetic, rather than against a fake ruler.
if (!globalThis.ResizeObserver) {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}
