# Dependency audit policy

What `cargo audit` and `npm audit` are allowed to do to a build, what is
ignored, and what would change each decision.

Plan §3.8 asks for the audit. This is the operating policy behind it, and
it is meant to be edited rather than obeyed — every entry below records
the reasoning so a future reader can decide it differently on purpose,
instead of discovering an unexplained flag and either trusting or
deleting it.

Written 2026-09-08, when CI first ran these at all.

## The gate

Both audits **fail the build**. That is a reversal: the job was first
written as `continue-on-error`, reasoning that advisories appear without
anyone pushing and so should not turn a green commit red retroactively.

That reasoning is true and still led to the wrong shape. A job that can
never fail is a job nobody reads, and the badge was already red for a
permanent reason — so the *next* advisory, the one that does reach the
hub, would have arrived invisibly. The retroactive-red problem is real
but small: an advisory landing overnight blocks the next merge until
somebody looks, which is a morning's inconvenience against the
alternative of never looking at all.

**If this becomes intolerable** — a run of unfixable advisories in
dependencies that cannot be dropped — the escape is a second ignored ID
with its own entry below, not a return to `continue-on-error`.

## Ignored: RUSTSEC-2022-0040 (`owning_ref`)

**Ignored indefinitely.** There is no version to move to.

*What it is.* `owning_ref` builds self-referential structs: a value
bundled with a reference into itself. Rust cannot express that, so the
crate fakes it with `unsafe` and a lifetime that is a lie —
`OwningHandle<RcRef<RefCell<V>>, RefMut<'static, V>>` claims a `'static`
borrow that is really bounded by the `Rc` beside it.

*Why unfixable.* The soundness holes are in the API, not the
implementation: safe code can extract that reference and outlive the
owner, producing a dangling `&mut` and undefined behaviour. Closing it
means changing the API in ways that break every caller, so no version
bump can be the fix, and the crate is unmaintained. The real remedy is
replacement (`ouroboros`, `yoke`) by whoever depends on it — which is
not us.

*Why it does not matter here.* Traced 2026-09-08:

```
owning_ref v0.4.1 -> cursive_core 0.3.7 -> cursive 0.20.0 -> wallet
```

`cargo tree -p hub`, `-p node`, `-p miner` and `-p console` return **zero
paths**. It reaches only the wallet's terminal UI, which is not in the
release artifact and never runs on a server. Both cursive call sites
(`views/named_view.rs:46`, `views/text_view.rs:159`) use `OwningHandle`
in its intended pattern — owner and guard bundled, guard never escaping —
so the hole is what the API permits, not what cursive does.

*What would change this.* Any of:

- **`cursive` migrates off `owning_ref`.** CI already checks for this:
  the audit step re-runs without the ignore and fails if the advisory
  stops firing, so a stale ignore cannot sit there hiding the next one.
  The fix is to delete the `--ignore`, delete that check, and delete this
  section.
- **Anything on the server path picks up `owning_ref`.** The trace above
  is the whole argument; if a hub dependency ever pulls it in, the
  ignore is wrong that day. Re-run the `cargo tree -i owning_ref` check
  before trusting this section.
- **The wallet TUI starts being shipped or run somewhere exposed.** It is
  currently a local operator tool. If it becomes something a stranger's
  input reaches, reassess.

## Not ignored: everything else

`npm audit --audit-level=high` runs with no exclusions, and needs none.
The five advisories it found on its first run (`react-router`, `nanoid`,
`postcss`, `undici` ×2 and their transitives) all had fixes available and
the fixes were taken on 2026-09-08 — `npm audit fix`, then the suite and
the production build re-run to confirm nothing broke.

Only one of those actually shipped to a browser: `react-router`'s RSC-mode
CSRF bypass. The rest reached the tree through `jsdom` (the test DOM) and
vite's build tooling, and never leave a developer's machine. That
distinction is worth keeping in mind when the next batch arrives —
**where a dependency runs decides how much a high severity means**, and
`npm audit` does not make that distinction for you.

## A note on yanked crates

`cargo audit` also reports yanked versions — `spin 0.9.8` at the time of
writing. Yanked means withdrawn from crates.io, not vulnerable; it does
not fail the build and should not. It is worth clearing when a dependency
update makes it convenient, and not worth chasing on its own.

## How to check the current state by hand

```bash
cargo audit                          # everything, ignores included
cargo audit --ignore RUSTSEC-2022-0040   # what CI gates on
cargo tree -i owning_ref             # who still pulls it in
cd dashboard && npm audit            # the JS side
```
