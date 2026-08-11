//! Human-readable names for agents.
//!
//! An agent is a 66-hex-character public key. That is the right identity
//! for the protocol and the wrong one for a person reading a
//! leaderboard: `02a4f1...9c3b` and `03a4e8...9c3b` are different agents
//! that look identical at a glance, and truncating them (see
//! `truncatePubkey` in the dashboard) makes that worse, not better. So
//! every agent the hub knows about also gets a name -- a descriptor and
//! a subject glued together in CamelCase, `SwiftWarlock`, `AmberOtter`.
//!
//! The name is a **display label, not an identity**. Nothing
//! authenticates against it, no route accepts one in place of a pubkey,
//! and the hub never lets an agent choose its own. That is deliberate:
//! a name an agent could pick is a name an agent can use to impersonate
//! another, and this registry exists to make the leaderboard readable,
//! not to introduce a second namespace anyone has to trust. The pubkey
//! remains the only thing that means anything.
//!
//! Names are drawn from `wordlist/` at the repository root, compiled
//! into the binary with `include_str!` so a deployed hub has no runtime
//! file dependency.
//!
//! ## Uniqueness
//!
//! Assignment is random over the unused portion of the pool and the
//! registry holds every name it has handed out, so two agents can never
//! share one. Assignment is also permanent: `assign` is idempotent per
//! pubkey, and a name is never returned to the pool, because an agent
//! whose name changed between two page loads would be worse than no
//! name at all.

use btclib::crypto::PublicKey;
use rand::Rng;
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

/// The hard cap on a rendered name, in characters.
///
/// Set by the owner. It is what makes a name fit a fixed-width table
/// column next to a number without wrapping, and it is why a portion of
/// the wordlist is unreachable: a descriptor and a subject that are both
/// long simply never pair. Nothing enforces this at the type level, so
/// `pool` filters on it and a test asserts no generated name exceeds it.
pub const MAX_NAME_LEN: usize = 15;

/// Curated adjectives -- a hand-picked subset of the full
/// `descriptors/adjectives.txt`, which is a WordNet-style dictionary
/// dump and contains demonyms, anatomical terms, and grammatical
/// participles alongside the usable words. Drawing from the raw file
/// would give roughly two million combinations of which most read as
/// nonsense (`AbdominalWorm`, `ZapotecValley`); the curated file is
/// every word in it that reads as a name. The original file is left
/// untouched and unused.
const CURATED_DESCRIPTORS: &str = include_str!("../../wordlist/descriptors/curated.txt");
const COLOR_DESCRIPTORS: &str = include_str!("../../wordlist/descriptors/colors.txt");

/// Every subject file, listed explicitly because `include_str!` cannot
/// glob a directory. **A new file under `wordlist/subjects/` must be
/// added here or it contributes nothing** -- there is no runtime
/// directory read that would pick it up silently.
const SUBJECT_FILES: &[&str] = &[
    include_str!("../../wordlist/subjects/astronomy.txt"),
    include_str!("../../wordlist/subjects/birds.txt"),
    include_str!("../../wordlist/subjects/creatures.txt"),
    include_str!("../../wordlist/subjects/edgy_creatures.txt"),
    include_str!("../../wordlist/subjects/insects_misc.txt"),
    include_str!("../../wordlist/subjects/landscapes.txt"),
    include_str!("../../wordlist/subjects/mammals.txt"),
    include_str!("../../wordlist/subjects/reptiles.txt"),
    include_str!("../../wordlist/subjects/sea.txt"),
    include_str!("../../wordlist/subjects/water.txt"),
    include_str!("../../wordlist/subjects/weather.txt"),
];

/// Splits one wordlist file into unique, sorted, lowercase words.
///
/// Deduplicating matters: the subject files overlap on purpose
/// (`vampire` is in both `creatures` and `edgy_creatures`, `sun` in both
/// `astronomy` and `weather`), and without this a duplicated subject
/// would appear twice in the pool and be twice as likely to be drawn.
/// Sorting is what makes the pool's order independent of filesystem
/// order, so the same build always has the same pool.
fn words(sources: &[&'static str]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = sources
        .iter()
        .flat_map(|source| source.lines())
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Every descriptor/subject pair that fits inside `MAX_NAME_LEN`.
///
/// Built once and shared. At the wordlist's current size this is 347
/// descriptors against 233 subjects, of which 79,369 pairs fit -- far
/// more than a testnet will ever need, which is the point: assignment
/// stays a cheap random draw rather than a search, right up until the
/// pool is nearly exhausted.
fn pool() -> &'static [(&'static str, &'static str)] {
    static POOL: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    POOL.get_or_init(|| {
        let descriptors = words(&[CURATED_DESCRIPTORS, COLOR_DESCRIPTORS]);
        let subjects = words(SUBJECT_FILES);
        let mut pairs = Vec::new();
        for descriptor in &descriptors {
            for subject in &subjects {
                if descriptor.len() + subject.len() <= MAX_NAME_LEN {
                    pairs.push((*descriptor, *subject));
                }
            }
        }
        pairs
    })
}

/// `("swift", "warlock")` -> `"SwiftWarlock"`.
///
/// The wordlists are ASCII lowercase, so uppercasing the leading byte of
/// each half is enough and the rendered length always equals the sum of
/// the two word lengths -- which is what lets `pool` filter on
/// `MAX_NAME_LEN` by summing lengths rather than by rendering.
fn render(descriptor: &str, subject: &str) -> String {
    let mut name = String::with_capacity(descriptor.len() + subject.len());
    for word in [descriptor, subject] {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            name.extend(first.to_uppercase());
            name.push_str(chars.as_str());
        }
    }
    name
}

/// How many random draws to make before giving up and scanning.
///
/// While the pool is mostly empty every draw succeeds first try. The
/// scan only earns its keep once the pool is nearly full, where random
/// probing degenerates -- at 99% occupancy a probe has a 1% hit rate and
/// 32 of them still miss two thirds of the time. Rather than loop
/// forever on a shrinking target, fall through to a deterministic sweep
/// that either finds the free slot or proves there isn't one.
const RANDOM_PROBES: usize = 32;

/// Every name the hub has handed out, and to whom.
///
/// Keyed by `BTreeMap` rather than `HashMap` because `PublicKey`
/// implements `Ord` but not `Hash` -- the same reason `TaskBoard` keys
/// its reputation map that way.
#[derive(Default)]
pub struct NameRegistry {
    by_pubkey: BTreeMap<PublicKey, String>,
    taken: HashSet<String>,
    /// The candidate pairs this registry draws from. Always `pool()` in
    /// production; tests substitute a deliberately tiny one to exercise
    /// the exhaustion path, which is unreachable against 79,369 names.
    candidates: &'static [(&'static str, &'static str)],
}

impl NameRegistry {
    pub fn new() -> Self {
        NameRegistry {
            by_pubkey: BTreeMap::new(),
            taken: HashSet::new(),
            candidates: pool(),
        }
    }

    /// Reinstates a name read back from the store at startup.
    ///
    /// Deliberately takes whatever the store holds without validating it
    /// against the current wordlist. A name that was legal when it was
    /// assigned must keep working even if the word it came from is later
    /// edited out of `wordlist/` -- the alternative is an agent silently
    /// being renamed by an unrelated commit. It still occupies its slot
    /// in `taken`, so nothing else can be handed the same string.
    pub fn restore(&mut self, pubkey: PublicKey, name: String) {
        self.taken.insert(name.clone());
        self.by_pubkey.insert(pubkey, name);
    }

    /// This agent's name, if it has one. Never assigns.
    ///
    /// The read-only half of the API, for callers that must not mint a
    /// name as a side effect of being asked a question -- see
    /// `handlers::get_reputation`, which is an unauthenticated GET that
    /// accepts *any* pubkey and would otherwise let an anonymous caller
    /// drain the pool one request at a time.
    pub fn get(&self, pubkey: &PublicKey) -> Option<&str> {
        self.by_pubkey.get(pubkey).map(String::as_str)
    }

    /// This agent's name, assigning one if it doesn't have it yet.
    ///
    /// Idempotent: the same pubkey always gets the same name back, for
    /// the lifetime of the store. Returns `None` only if every name in
    /// the pool is taken, which at the current wordlist size means
    /// 79,369 agents have been named -- the caller renders the pubkey
    /// alone in that case rather than failing the request.
    ///
    /// The bool is whether this call is what created the name, so the
    /// caller knows whether it has something new to persist. Nothing
    /// here writes to disk; a registry that quietly did I/O behind a
    /// lock would be a much worse thing to hold across an `await`.
    pub fn assign(&mut self, pubkey: &PublicKey) -> Option<(String, bool)> {
        if let Some(name) = self.by_pubkey.get(pubkey) {
            return Some((name.clone(), false));
        }
        let name = self.draw()?;
        self.taken.insert(name.clone());
        self.by_pubkey.insert(pubkey.clone(), name.clone());
        Some((name, true))
    }

    /// Picks an unused name: random probes first, then one wrapping scan
    /// from a random offset so a nearly-full pool still resolves without
    /// always handing out the same low-index leftovers.
    fn draw(&self) -> Option<String> {
        if self.candidates.is_empty() {
            return None;
        }
        let mut rng = rand::thread_rng();
        for _ in 0..RANDOM_PROBES {
            let (descriptor, subject) = self.candidates[rng.gen_range(0..self.candidates.len())];
            let name = render(descriptor, subject);
            if !self.taken.contains(&name) {
                return Some(name);
            }
        }
        let start = rng.gen_range(0..self.candidates.len());
        for offset in 0..self.candidates.len() {
            let (descriptor, subject) = self.candidates[(start + offset) % self.candidates.len()];
            let name = render(descriptor, subject);
            if !self.taken.contains(&name) {
                return Some(name);
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.by_pubkey.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_pubkey.is_empty()
    }

    /// How many distinct names the wordlist can still produce. Read by
    /// the startup log so an operator can see the headroom rather than
    /// discovering it's gone when assignment starts failing.
    pub fn remaining(&self) -> usize {
        self.candidates.len().saturating_sub(self.taken.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;

    fn pubkey() -> PublicKey {
        PrivateKey::new_key().public_key()
    }

    fn tiny_registry(candidates: &'static [(&'static str, &'static str)]) -> NameRegistry {
        NameRegistry { candidates, ..Default::default() }
    }

    #[test]
    fn the_pool_is_large_and_every_name_fits_the_length_cap() {
        let pool = pool();
        assert!(
            pool.len() > 10_000,
            "expected a pool with real headroom, got {}",
            pool.len()
        );
        for (descriptor, subject) in pool {
            let name = render(descriptor, subject);
            assert!(
                name.chars().count() <= MAX_NAME_LEN,
                "{name} exceeds {MAX_NAME_LEN} characters"
            );
            assert!(
                name.chars().all(|c| c.is_ascii_alphabetic()),
                "{name} is not plain ASCII letters"
            );
        }
    }

    #[test]
    fn renders_camel_case() {
        assert_eq!(render("swift", "warlock"), "SwiftWarlock");
        assert_eq!(render("amber", "otter"), "AmberOtter");
    }

    #[test]
    fn duplicated_subjects_appear_in_the_pool_only_once() {
        // `vampire` is listed in both creatures.txt and
        // edgy_creatures.txt; the pool must not double-count it.
        let vampires = pool().iter().filter(|(d, s)| *d == "swift" && *s == "vampire").count();
        assert_eq!(vampires, 1);
    }

    #[test]
    fn assignment_is_idempotent_per_agent() {
        let mut registry = NameRegistry::new();
        let agent = pubkey();

        let (first, fresh) = registry.assign(&agent).unwrap();
        assert!(fresh, "the first assignment is a new name");
        let (second, fresh_again) = registry.assign(&agent).unwrap();
        assert!(!fresh_again, "re-asking must not mint a second name");
        assert_eq!(first, second);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn many_agents_all_get_distinct_names() {
        let mut registry = NameRegistry::new();
        let mut seen = HashSet::new();
        for _ in 0..2_000 {
            let (name, fresh) = registry.assign(&pubkey()).unwrap();
            assert!(fresh);
            assert!(seen.insert(name.clone()), "{name} was handed out twice");
        }
        assert_eq!(registry.len(), 2_000);
    }

    #[test]
    fn get_never_assigns() {
        let mut registry = NameRegistry::new();
        let agent = pubkey();
        assert!(registry.get(&agent).is_none());
        assert_eq!(registry.len(), 0);

        registry.assign(&agent).unwrap();
        assert!(registry.get(&agent).is_some());
    }

    #[test]
    fn a_restored_name_is_kept_and_cannot_be_handed_to_anyone_else() {
        const CANDIDATES: &[(&str, &str)] = &[("swift", "owl"), ("amber", "owl")];
        let mut registry = tiny_registry(CANDIDATES);

        let restored = pubkey();
        registry.restore(restored.clone(), "SwiftOwl".to_string());
        assert_eq!(registry.get(&restored), Some("SwiftOwl"));

        let (other, _) = registry.assign(&pubkey()).unwrap();
        assert_eq!(other, "AmberOwl", "the only name left");
    }

    #[test]
    fn a_restored_name_survives_its_word_leaving_the_wordlist() {
        const CANDIDATES: &[(&str, &str)] = &[("swift", "owl")];
        let mut registry = tiny_registry(CANDIDATES);
        let agent = pubkey();
        registry.restore(agent.clone(), "RetiredWord".to_string());
        assert_eq!(registry.get(&agent), Some("RetiredWord"));
        assert_eq!(
            registry.assign(&agent).unwrap(),
            ("RetiredWord".to_string(), false)
        );
    }

    #[test]
    fn an_exhausted_pool_returns_none_rather_than_reusing_a_name() {
        const CANDIDATES: &[(&str, &str)] = &[("swift", "owl"), ("amber", "owl")];
        let mut registry = tiny_registry(CANDIDATES);

        let mut names = HashSet::new();
        for _ in 0..2 {
            let (name, _) = registry.assign(&pubkey()).unwrap();
            assert!(names.insert(name));
        }
        assert_eq!(registry.remaining(), 0);
        assert!(
            registry.assign(&pubkey()).is_none(),
            "a full pool must refuse rather than duplicate"
        );
        // an already-named agent still resolves after exhaustion
        let existing = registry.by_pubkey.keys().next().cloned().unwrap();
        assert!(registry.assign(&existing).is_some());
    }

    #[test]
    fn an_empty_pool_is_not_a_panic() {
        const NONE: &[(&str, &str)] = &[];
        let mut registry = tiny_registry(NONE);
        assert!(registry.assign(&pubkey()).is_none());
    }
}
