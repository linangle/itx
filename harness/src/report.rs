//! What a run produces.
//!
//! The point of the report format is re-runnability. `hub/src/node_client.rs`
//! keeps its pooling benchmark in the tree with the *old code path* as its
//! baseline, so the comparison that chose the pool size can be repeated
//! rather than re-argued. The same idea applies here, one level up: a drill
//! writes a JSON report with a stable shape, the report is checked in under
//! `harness/baselines/`, and after the code it measures changes, the same
//! drill is re-run and the two files are compared. A number in prose is an
//! opinion by the time anyone reads it.
//!
//! Every report therefore records what it ran against, not just what
//! happened. A drill result with no commit and no binary fingerprint is
//! not evidence -- `target/` is shared by every worktree on this machine,
//! so "the hub" is a path, not a version.

use crate::stats::Summary;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// Whether the measurement agreed with what the plan predicted.
///
/// `Refuted` is a first-class outcome, not an error. It has happened once
/// already on this codebase -- §6.2's recommended connection pooling
/// measured slower than the churn it was supposed to replace -- and the
/// handoff for this work says plainly that when the numbers disagree with
/// the plan, the numbers win.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The plan said this would happen, and it did.
    Confirmed,
    /// The plan said this would happen, and it did not.
    Refuted,
    /// The drill ran but could not decide. Distinct from a drill that
    /// failed to run at all, which is an error and not a verdict.
    Inconclusive,
}

/// The code and machine a run happened on.
#[derive(Debug, Clone, Serialize)]
pub struct Environment {
    pub commit: String,
    /// Whether the worktree's *code* differed from `commit`.
    ///
    /// Scoped to sources and manifests rather than the whole tree, and
    /// that is not laziness. A run writes its own report into the repo,
    /// so a whole-tree check reports every run after the first as dirty
    /// on the strength of the previous run's output — which says nothing
    /// about whether the binaries under test matched the commit, and
    /// trains a reader to ignore the flag. Prose edits are excluded for
    /// the same reason: they cannot change a measurement.
    pub dirty: bool,
    /// `debug` or `release`. Load numbers from a debug build are worth
    /// recording and worth nothing as an absolute, so this is reported
    /// rather than assumed.
    pub profile: &'static str,
    pub host: String,
    pub cpus: usize,
    /// Every binary the run launched, by path, with its SHA256.
    pub binaries: BTreeMap<String, String>,
}

impl Environment {
    pub fn capture(repo: &Path) -> Self {
        let commit = git(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
        let dirty = git(repo, &["status", "--porcelain", "--", "*.rs", "*.toml"])
            .map(|out| !out.is_empty())
            // Unknown is reported as dirty: a flag that cannot be
            // established should not read as a clean bill of health.
            .unwrap_or(true);
        Self {
            commit,
            dirty,
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            host: std::process::Command::new("uname")
                .arg("-sm")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            cpus: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0),
            binaries: BTreeMap::new(),
        }
    }

    /// Records the SHA256 of a binary this run is about to launch.
    ///
    /// This is the mechanical form of the rule every handoff in this wave
    /// repeats: `target/debug/hub` is one path for four worktrees, so a
    /// measurement that does not name the bytes it ran against may be
    /// measuring another session's work in progress.
    pub fn fingerprint(&mut self, path: &Path) -> Result<()> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading {} to fingerprint it", path.display()))?;
        let digest = Sha256::digest(&bytes);
        self.binaries
            .insert(path.display().to_string(), hex::encode(digest));
        Ok(())
    }
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// One drill, or one phase of the load profile.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub title: String,
    /// The plan section this speaks to, e.g. `"§6.4b"`. Present so a
    /// report can be walked back into `docs/agent-ecosystem-plan.md`
    /// mechanically rather than from memory.
    pub plan_item: Option<String>,
    pub verdict: Option<Verdict>,
    /// The numbers the drill actually measured, named. This is the part
    /// that gets compared across runs, so keys should stay stable even
    /// when the prose around them changes.
    pub facts: BTreeMap<String, Value>,
    /// What a reader needs in order to interpret the facts, in sentences.
    pub notes: Vec<String>,
    /// A bug the drill found. Separate from `notes` because these are the
    /// deliverable: the handoff asks for findings written up whether or
    /// not they get fixed.
    pub findings: Vec<String>,
    /// Something the drill reliably observes, has been looked at, and is
    /// agreed not to be a defect.
    ///
    /// Kept apart from `findings` rather than demoted to a `note`,
    /// because the two say different things: a note is context, an
    /// accepted finding is "we know, we checked, it is fine." It does not
    /// fail a run and `compare` does not treat it as new.
    ///
    /// This exists because `escrow-restart` reports one dropped
    /// confirmation on `SIGTERM` in most runs, which §6.5b measured
    /// against a pre-fix control at the same rate and so identified as
    /// pre-existing drain variance rather than anything a fix
    /// introduced. As an ordinary finding it failed that drill's
    /// comparison on nearly every run, for a reason everybody had
    /// already agreed was harmless -- which is how a red signal stops
    /// being read.
    pub accepted: Vec<String>,
    /// Which verdict means "nothing to act on" for this section.
    ///
    /// The verdict answers a question about the *plan* -- did the
    /// predicted thing happen -- and that is not the same question as
    /// whether the hub is healthy. Where the plan predicted safety
    /// ("a crash leaves consistent state") the healthy answer is
    /// `Confirmed`, which is the common case and the default. Where it
    /// predicted a failure ("killing the node mid-payout destroys money
    /// the hub still reports as paid") the healthy answer is `Refuted`,
    /// and once §6.5 landed that is exactly what `node-crash` reports.
    ///
    /// Until this field existed, `needs_attention` read every `Refuted`
    /// as a failure, so `node-crash` exited non-zero on a *healthy* hub
    /// and `compare` read a §6.5 regression -- refuted back to confirmed
    /// -- as somebody's fix landing. Three of eight drills were red when
    /// nothing was wrong, which made `harness drill all` permanently red
    /// and the exit code worthless.
    pub healthy_verdict: Verdict,
    pub latency: Vec<Summary>,
}

impl Section {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            plan_item: None,
            verdict: None,
            facts: BTreeMap::new(),
            notes: Vec::new(),
            findings: Vec::new(),
            accepted: Vec::new(),
            healthy_verdict: Verdict::Confirmed,
            latency: Vec::new(),
        }
    }

    pub fn plan_item(mut self, item: &str) -> Self {
        self.plan_item = Some(item.to_string());
        self
    }

    pub fn verdict(mut self, verdict: Verdict) -> Self {
        self.verdict = Some(verdict);
        self
    }

    pub fn fact(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.facts.insert(key.to_string(), value.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn finding(mut self, finding: impl Into<String>) -> Self {
        self.findings.push(finding.into());
        self
    }

    /// See `Section::accepted`. Say *why* it is accepted in the text --
    /// an accepted finding with no reasoning is indistinguishable from
    /// one somebody silenced to get a green run.
    pub fn accepted_finding(mut self, finding: impl Into<String>) -> Self {
        self.accepted.push(finding.into());
        self
    }

    /// Declares that this section is healthy when it reports `verdict`.
    /// Defaults to `Confirmed`; see `Section::healthy_verdict` for when
    /// it is not.
    pub fn healthy_when(mut self, verdict: Verdict) -> Self {
        self.healthy_verdict = verdict;
        self
    }

    /// Whether this section's verdict is one that wants a human.
    ///
    /// `Inconclusive` never is, on its own. It means the drill could not
    /// decide, which is an absence of an answer rather than a bad one,
    /// and on a run with no baseline to compare against there is nothing
    /// else to say. A drill that wants an inconclusive run to be loud
    /// says so with a finding, the way `escrow-refund` does when no sweep
    /// pass completed inside its observation window.
    ///
    /// This used to cite `escrow-restart` as the example of a drill for
    /// which inconclusive was the expected post-fix outcome. That stopped
    /// being true on 2026-09-07, when the race it hunts was closed by
    /// making the commit atomic, and its SIGKILL phase can now confirm.
    ///
    /// Note the asymmetry with `compare`, which since 2026-09-09 *does*
    /// treat a slide out of the healthy verdict as a regression, this one
    /// included. It can: it has a baseline, so it knows the section used
    /// to do better. Here there is no such evidence.
    pub fn verdict_needs_attention(&self) -> bool {
        match self.verdict {
            None | Some(Verdict::Inconclusive) => false,
            Some(verdict) => verdict != self.healthy_verdict,
        }
    }

    pub fn latency(mut self, summaries: Vec<Summary>) -> Self {
        self.latency = summaries;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub environment: Environment,
    pub sections: Vec<Section>,
}

impl Report {
    pub fn new(name: impl Into<String>, environment: Environment) -> Self {
        Self {
            name: name.into(),
            started_at: Utc::now(),
            environment,
            sections: Vec::new(),
        }
    }

    pub fn push(&mut self, section: Section) {
        self.sections.push(section);
    }

    /// True when any section reported a verdict other than its healthy
    /// one, or found something not already accepted. The process exit
    /// code keys off this, so a drill run in CI fails loudly rather than
    /// leaving a file for someone to read.
    ///
    /// It used to key off `Refuted` and any finding at all, which made a
    /// healthy `node-crash`, `escrow-restart` and `signed-write-cost`
    /// all exit non-zero -- so `harness drill all` was red whatever the
    /// hub did, and the "put it in front of a change and be told"
    /// property the README claims did not hold. See
    /// `Section::healthy_verdict` and `Section::accepted`.
    pub fn needs_attention(&self) -> bool {
        self.sections
            .iter()
            .any(|s| s.verdict_needs_attention() || !s.findings.is_empty())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("=== {} ===\n", self.name));
        out.push_str(&format!(
            "{}  commit {}{}  {} build  {}  {} cpus\n",
            self.started_at.format("%Y-%m-%d %H:%M:%SZ"),
            &self.environment.commit.chars().take(12).collect::<String>(),
            if self.environment.dirty {
                " (dirty)"
            } else {
                ""
            },
            self.environment.profile,
            self.environment.host,
            self.environment.cpus,
        ));
        for (path, digest) in &self.environment.binaries {
            out.push_str(&format!(
                "  {path}  sha256:{}\n",
                &digest.chars().take(16).collect::<String>()
            ));
        }

        for section in &self.sections {
            out.push_str(&format!("\n--- {} ", section.title));
            if let Some(item) = &section.plan_item {
                out.push_str(&format!("(plan {item}) "));
            }
            match section.verdict {
                Some(Verdict::Confirmed) => out.push_str("[CONFIRMED]"),
                Some(Verdict::Refuted) => out.push_str("[REFUTED]"),
                Some(Verdict::Inconclusive) => out.push_str("[INCONCLUSIVE]"),
                None => {}
            }
            out.push('\n');

            for (key, value) in &section.facts {
                out.push_str(&format!("  {key}: {value}\n"));
            }
            for note in &section.notes {
                out.push_str(&format!("  note: {note}\n"));
            }
            for finding in &section.accepted {
                out.push_str(&format!("  ACCEPTED: {finding}\n"));
            }
            for finding in &section.findings {
                out.push_str(&format!("  FINDING: {finding}\n"));
            }
            if !section.latency.is_empty() {
                out.push('\n');
                out.push_str(&crate::stats::render_table(&section.latency));
            }
        }
        out
    }

    pub fn write_json(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing the report to {}", path.display()))?;
        Ok(())
    }
}

/// Compares a fresh report against a checked-in baseline.
///
/// This is what makes the baselines more than a file nobody opens. The
/// pooling benchmark in `hub/src/node_client.rs` keeps the old code path
/// beside the new one so the comparison can be *run*; a report on disk
/// only gets that property if something reads it. Sections are matched by
/// title and facts by key, which is why both are meant to stay stable
/// across runs even when the prose around them changes.
///
/// Returns the rendered comparison and whether anything got worse -- a
/// verdict that went from confirmed to refuted, or a finding that was not
/// there before.
pub fn compare(baseline: &serde_json::Value, current: &serde_json::Value) -> (String, bool) {
    let empty = Vec::new();
    let sections = |report: &serde_json::Value| -> Vec<Value> {
        report
            .get("sections")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
            .clone()
    };
    let by_title = |report: &serde_json::Value| -> BTreeMap<String, Value> {
        sections(report)
            .into_iter()
            .filter_map(|section| Some((section.get("title")?.as_str()?.to_string(), section)))
            .collect()
    };

    let before = by_title(baseline);
    let after = by_title(current);
    let mut out = String::new();
    let mut worse = false;

    out.push_str(&format!(
        "baseline: {} at {}\ncurrent:  {} at {}\n",
        baseline
            .pointer("/environment/commit")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        baseline
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        current
            .pointer("/environment/commit")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        current
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or("?"),
    ));

    for (title, new_section) in &after {
        out.push_str(&format!("\n--- {title}\n"));
        let Some(old_section) = before.get(title) else {
            out.push_str("  (not in the baseline)\n");
            continue;
        };

        let old_verdict = old_section.get("verdict").and_then(Value::as_str);
        let new_verdict = new_section.get("verdict").and_then(Value::as_str);
        // Which verdict is healthy is read from the **current** report,
        // and applied to both sides. Taking it from the baseline would
        // mean every baseline written before the field existed defaults
        // to `confirmed` and quietly mis-scores the pessimistic-claim
        // drills it was added for. The current run is the one produced
        // by today's code, so it is the one that knows.
        let healthy = new_section
            .get("healthy_verdict")
            .and_then(Value::as_str)
            .unwrap_or("confirmed");
        // `inconclusive` is not a bad answer, it is the absence of one --
        // see `Section::verdict_needs_attention`.
        let is_a_problem = |verdict: Option<&str>| {
            !matches!(verdict, None | Some("inconclusive")) && verdict != Some(healthy)
        };
        // Whether a section is delivering the answer it exists to give.
        let reaches_healthy = |verdict: Option<&str>| verdict == Some(healthy);
        if old_verdict != new_verdict {
            out.push_str(&format!(
                "  verdict: {} -> {}\n",
                old_verdict.unwrap_or("none"),
                new_verdict.unwrap_or("none")
            ));
            // Two ways a section gets worse, and it used to check only
            // the second.
            //
            // First: it reached its healthy verdict and no longer does.
            // That is a regression whatever it became, `inconclusive`
            // included -- which is not a bad answer but *is* the loss of
            // a good one, and a drill that has stopped being able to
            // decide has stopped covering what it covered yesterday.
            //
            // Second: it became an outright bad answer even though the
            // baseline was not healthy either. `inconclusive -> refuted`
            // is the bug coming back, and no healthy verdict was lost
            // because none was held.
            //
            // Which direction is which depends on the drill -- for
            // `node-crash` the plan predicted the failure, so `refuted`
            // is healthy and `confirmed` is the regression. Both rules
            // are written against `healthy` rather than against a
            // hardcoded verdict for that reason.
            //
            // The gap this closes is not hypothetical: `escrow-restart`'s
            // SIGKILL phase reported `inconclusive` for two days after
            // the race it hunts was closed, its baseline recorded that
            // shrug three hours after the fix landed, and every
            // comparison since passed without comment.
            if (reaches_healthy(old_verdict) && !reaches_healthy(new_verdict))
                || (is_a_problem(new_verdict) && !is_a_problem(old_verdict))
            {
                worse = true;
            }
        }

        let facts = |section: &Value| -> BTreeMap<String, Value> {
            section
                .get("facts")
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
        let old_facts = facts(old_section);
        for (key, new_value) in facts(new_section) {
            match old_facts.get(&key) {
                Some(old_value) if old_value == &new_value => {}
                Some(old_value) => out.push_str(&format!("  {key}: {old_value} -> {new_value}\n")),
                None => out.push_str(&format!("  {key}: (new) {new_value}\n")),
            }
        }

        let findings_at = |section: &Value, key: &str| -> Vec<String> {
            section
                .get(key)
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|f| f.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        // An accepted finding on either side counts as known, so a
        // baseline predating the `accepted` field does not turn every
        // later run's accepted text into a new finding.
        let mut known = findings_at(old_section, "findings");
        known.extend(findings_at(old_section, "accepted"));
        known.extend(findings_at(new_section, "accepted"));
        let old_findings = findings_at(old_section, "findings");
        let new_findings = findings_at(new_section, "findings");
        for finding in &new_findings {
            if !known.contains(finding) {
                out.push_str(&format!("  NEW FINDING: {finding}\n"));
                worse = true;
            }
        }
        for finding in &old_findings {
            if !new_findings.contains(finding) {
                out.push_str(&format!("  gone: {finding}\n"));
            }
        }
    }

    (out, worse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(verdict: &str, findings: Vec<&str>, count: i64) -> Value {
        report_healthy_when("confirmed", verdict, findings, count)
    }

    /// `report`, for a drill whose healthy verdict is not `confirmed` --
    /// see `Section::healthy_verdict`. Deliberately no `accepted` key on
    /// either, so these also stand in for a baseline written before that
    /// field existed.
    fn report_healthy_when(healthy: &str, verdict: &str, findings: Vec<&str>, count: i64) -> Value {
        json!({
            "environment": {"commit": "abc"},
            "started_at": "2026-09-06T00:00:00Z",
            "sections": [{
                "title": "A drill",
                "verdict": verdict,
                "healthy_verdict": healthy,
                "facts": {"itx_lost": count},
                "findings": findings,
            }],
        })
    }

    #[test]
    fn a_new_finding_is_a_regression() {
        let (rendered, worse) = compare(
            &report("confirmed", vec![], 0),
            &report("confirmed", vec!["money vanished"], 5),
        );
        assert!(worse);
        assert!(rendered.contains("NEW FINDING: money vanished"));
        assert!(rendered.contains("itx_lost: 0 -> 5"));
    }

    #[test]
    fn a_finding_going_away_is_not() {
        // The whole point of a baseline here is that somebody lands a fix
        // and the drill stops finding the bug. That must not fail.
        let (rendered, worse) = compare(
            &report("confirmed", vec!["money vanished"], 5),
            &report("confirmed", vec![], 0),
        );
        assert!(!worse);
        assert!(rendered.contains("gone: money vanished"));
    }

    /// The direction of a regression is per drill, and `node-crash` is
    /// the one that made this matter: §6.5 predicted the failure, so
    /// refuting it is the fix and *confirming* it again is the
    /// regression. The old rule was hardcoded the other way and read a
    /// re-broken payout path as somebody's fix landing.
    #[test]
    fn for_a_pessimistic_claim_confirmed_is_the_regression() {
        let (rendered, worse) = compare(
            &report_healthy_when("refuted", "refuted", vec![], 0),
            &report_healthy_when("refuted", "confirmed", vec![], 6_000_000),
        );
        assert!(
            worse,
            "the plan's predicted failure happening again is the regression"
        );
        assert!(rendered.contains("verdict: refuted -> confirmed"));

        // And the fix landing must not fail: the same drill going the
        // other way is an improvement.
        let (_, worse) = compare(
            &report_healthy_when("refuted", "confirmed", vec![], 6_000_000),
            &report_healthy_when("refuted", "refuted", vec![], 0),
        );
        assert!(!worse);
    }

    /// `escrow-restart`'s post-fix SIGKILL verdict, and the reason
    /// re-baselining it closes §6.5b's refuted-to-refuted gap: from an
    /// inconclusive baseline, the bug coming back reads as a problem
    /// where the baseline was not one.
    #[test]
    fn inconclusive_is_not_a_problem_but_regressing_out_of_it_is() {
        let (_, worse) = compare(
            &report("inconclusive", vec![], 0),
            &report("inconclusive", vec![], 0),
        );
        assert!(!worse, "a drill that still cannot decide has not got worse");

        let (rendered, worse) = compare(
            &report("inconclusive", vec![], 0),
            &report("refuted", vec![], 1),
        );
        assert!(
            worse,
            "and the bug returning must fail, which refuted-to-refuted never could"
        );
        assert!(rendered.contains("verdict: inconclusive -> refuted"));
    }

    /// An accepted finding is known, so it must not read as new -- even
    /// against a baseline written before the field existed, which is
    /// every baseline currently checked in.
    #[test]
    fn an_accepted_finding_is_not_a_new_finding() {
        let mut current = report("confirmed", vec![], 0);
        current["sections"][0]["accepted"] = json!(["known drain variance"]);
        let (rendered, worse) = compare(&report("confirmed", vec![], 0), &current);
        assert!(
            !worse,
            "accepting a known observation must not fail a comparison"
        );
        assert!(!rendered.contains("NEW FINDING"));

        // But a real finding alongside an accepted one still fails.
        let mut current = report("confirmed", vec!["money vanished"], 0);
        current["sections"][0]["accepted"] = json!(["known drain variance"]);
        let (_, worse) = compare(&report("confirmed", vec![], 0), &current);
        assert!(
            worse,
            "accepting one thing must not silence everything else"
        );
    }

    /// The gap that let a drill stop covering something in silence.
    ///
    /// `escrow-restart`'s SIGKILL phase confirmed, then the code changed
    /// underneath it and it went inconclusive, and nothing failed --
    /// because the old rule only asked whether the NEW verdict was bad,
    /// and inconclusive is not bad. It is, however, the loss of an answer
    /// that used to be there, which is what a baseline exists to notice.
    #[test]
    fn losing_the_healthy_verdict_to_inconclusive_is_a_regression() {
        let (rendered, worse) = compare(
            &report("confirmed", vec![], 0),
            &report("inconclusive", vec![], 0),
        );
        assert!(
            worse,
            "a section that used to confirm and now cannot decide has regressed"
        );
        assert!(rendered.contains("verdict: confirmed -> inconclusive"));
    }

    /// The same rule read against a drill whose healthy verdict is
    /// `refuted`, so this cannot be passing by treating one literal
    /// verdict as special.
    #[test]
    fn a_pessimistic_drill_also_regresses_into_inconclusive() {
        let (_, worse) = compare(
            &report_healthy_when("refuted", "refuted", vec![], 0),
            &report_healthy_when("refuted", "inconclusive", vec![], 0),
        );
        assert!(
            worse,
            "node-crash going undecided is the same loss of coverage"
        );
    }

    /// And the improving direction must stay quiet, or every drill that
    /// gets fixed fails its own gate on the run that fixes it.
    #[test]
    fn climbing_out_of_inconclusive_is_not_a_regression() {
        let (rendered, worse) = compare(
            &report("inconclusive", vec![], 0),
            &report("confirmed", vec![], 0),
        );
        assert!(
            !worse,
            "inconclusive -> confirmed is the fix landing, not a regression"
        );
        assert!(rendered.contains("verdict: inconclusive -> confirmed"));
    }

    #[test]
    fn confirmed_turning_into_refuted_is_a_regression() {
        let (_, worse) = compare(
            &report("confirmed", vec![], 0),
            &report("refuted", vec![], 0),
        );
        assert!(worse);
    }

    #[test]
    fn a_section_missing_from_the_baseline_is_reported_not_ignored() {
        let baseline = json!({"sections": []});
        let (rendered, worse) = compare(&baseline, &report("confirmed", vec![], 0));
        assert!(!worse);
        assert!(rendered.contains("(not in the baseline)"));
    }
}
