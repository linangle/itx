import { afterEach, describe, expect, it, vi } from "vitest";
import { hubUrl } from "./hub";

/** Where the site looks for its hub.
 *
 * A build is one artifact and a deployment is many domains, so this is
 * resolved at run time from a meta tag the operator edits in the shipped
 * `index.html`. Baking it in with `VITE_HUB_URL` would pin a release
 * tarball to whatever host built it.
 */
/** The repo ships a `.env.local` pointing the dev server at the mock
 * fixture, and vitest loads it -- so every test about the *tag* has to
 * clear the build-time override first, or it is testing the override. */
function noBuildOverride() {
  vi.stubEnv("VITE_HUB_URL", "");
}

function setTag(content: string | null) {
  document.head.querySelector('meta[name="itx-hub-url"]')?.remove();
  if (content === null) return;
  const meta = document.createElement("meta");
  meta.setAttribute("name", "itx-hub-url");
  meta.setAttribute("content", content);
  document.head.appendChild(meta);
}

afterEach(() => {
  setTag(null);
  vi.unstubAllEnvs();
});

describe("hubUrl", () => {
  it("uses the deploy-time tag when the operator has set one", () => {
    noBuildOverride();
    setTag("https://hub.example.org");
    expect(hubUrl()).toBe("https://hub.example.org");
  });

  it("ignores an unedited placeholder rather than querying someone else", () => {
    // The shipped index.html carries the example host. Trusting it would
    // point a real deployment at a domain that is not theirs, which is a
    // worse failure than not reaching a hub at all: it fails silently,
    // and against a third party.
    noBuildOverride();
    setTag("https://hub.itx.example.com");
    expect(hubUrl()).toBe("http://127.0.0.1:9100");
  });

  it("falls back to loopback when there is no tag at all", () => {
    noBuildOverride();
    setTag(null);
    expect(hubUrl()).toBe("http://127.0.0.1:9100");
  });

  it("trims a trailing slash, so a path is never doubled", () => {
    // Every caller appends a path beginning with `/`.
    noBuildOverride();
    setTag("https://hub.example.org/");
    expect(hubUrl()).toBe("https://hub.example.org");
  });

  it("uses the build-time variable in dev, because that is what dev uses", () => {
    // Vitest runs with DEV true, which is the case this covers: the dev
    // server and the mock fixture.
    vi.stubEnv("VITE_HUB_URL", "http://127.0.0.1:9101");
    setTag(null);
    expect(hubUrl()).toBe("http://127.0.0.1:9101");
  });

  it("lets an edited tag beat the build-time variable", () => {
    // The regression this pins: `.env.local` is loaded by `vite build`
    // too, so building the site on a developer's checkout -- which §5.1
    // tells the operator to do -- produced a bundle hard-wired to that
    // machine's mock hub, with the tag compiled out as dead code. The
    // page loaded, the board was empty, and it looked like a quiet day.
    vi.stubEnv("VITE_HUB_URL", "http://127.0.0.1:9101");
    setTag("https://hub.example.org");
    expect(hubUrl()).toBe("https://hub.example.org");
  });

  it("ignores the build-time variable outside dev", () => {
    // A production bundle must not be able to inherit a private hub from
    // whichever machine ran `npm run build`.
    vi.stubEnv("DEV", false);
    vi.stubEnv("VITE_HUB_URL", "http://127.0.0.1:9101");
    setTag(null);
    expect(hubUrl()).toBe("http://127.0.0.1:9100");
  });
});

describe("the legacy pages' client", () => {
  it("resolves the hub through the same function, tag included", async () => {
    // `src/api.ts` used to carry a second copy that read only the env
    // var. The three pages under /legacy/* therefore ignored the meta
    // tag entirely and talked to loopback on a deployed site.
    const legacy = await import("../api");
    noBuildOverride();
    setTag("https://hub.example.org");
    expect(legacy.hubUrl()).toBe("https://hub.example.org");
    expect(legacy.hubUrl).toBe(hubUrl);
  });
});
