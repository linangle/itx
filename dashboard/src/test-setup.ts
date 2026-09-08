import "@testing-library/jest-dom/vitest";
import { cleanup, configure } from "@testing-library/react";
import { afterEach } from "vitest";

// `waitFor` and `findBy*` default to giving up after one second, which is
// a statement about the machine rather than about the code: it is
// generous on a developer's laptop and marginal on a loaded CI runner
// sharing a core with three other jobs.
//
// This is not hypothetical. `TasksPage`'s "asks for names only for the
// posters on the page" passed eight times in a row locally and failed on
// CI, where that single test took 1081ms against the 1000ms budget. The
// failure reads as an assertion about arguments -- `expected "vi.fn()" to
// be called with` -- which sends you looking at the component rather than
// at the clock.
//
// Five seconds instead. A test that is genuinely broken still fails; it
// just takes five seconds to say so, and only the ones that were about to
// fail pay that. Raising it here rather than per-call because the next
// slow render will be in a different file and nobody will connect the
// two.
configure({ asyncUtilTimeout: 5000 });

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
