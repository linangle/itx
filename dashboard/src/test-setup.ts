import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// `@testing-library/react`'s auto-cleanup only self-registers when it
// detects the test framework's globals on `globalThis`. This project
// deliberately doesn't enable Vitest's `globals: true`, so that detection
// never fires and DOM from one test leaks into the next.
afterEach(() => {
  cleanup();
});

// jsdom implements no CSS media query engine and so ships no
// `matchMedia`. `Shell` calls it on first render to pick a starting
// theme, which makes every test that mounts a terminal page throw.
// Stubbed rather than mocked per-file: it's a gap in the environment, not
// behaviour under test. `matches: false` is the same branch a real browser
// takes when the user has never expressed a preference.
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

// Same kind of gap: jsdom does no layout, so it ships no `ResizeObserver`
// -- and `useElementWidth`, which the market chart uses, constructs one on
// mount.
//
// It deliberately never fires. There is no layout to observe, so a stub
// that invoked its callback would be reporting a measurement jsdom never
// made; leaving it silent keeps the measured width at 0, which is the
// "not ready to draw" state the chart already handles.
if (!globalThis.ResizeObserver) {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}
