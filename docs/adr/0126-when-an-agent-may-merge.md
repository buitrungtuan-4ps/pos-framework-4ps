# ADR-0126 — When an agent may press merge

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-20
**Amends** [`AGENTS.md`](../../AGENTS.md) §6, whose rule was *"A human merges. Always."*
**Relates to** [ADR-0109](0109-counting-the-taps-an-operator-makes.md) (the browser gate that makes "green" mean a flow really works, not that a unit test passed)

**Context.** `AGENTS.md` §6 has said *"A human merges. Always."* since the file was written, and §8 says a rule in it beats a request that conflicts with it. Both are right about the thing they were protecting: **somebody accountable looks at what lands on `main`.** A machine that can open a change and also land it can walk the whole distance alone, and the first anyone hears of a mistake is a shop that will not sell.

What the rule did not anticipate is the shape the work actually took. Five pull requests sat green, mergeable and unreviewed-because-there-was-nothing-to-review for nineteen hours, while the one person who could press the button was asleep. The rule was not protecting anything during those nineteen hours; it was queueing.

It is also worth stating the arithmetic that forced this record to exist, because it is not obvious: **a rule that lives in the repository can only be changed by a merge, and under the old rule that merge must be human.** There is no sequence of events in which an agent grants itself this. The first press is a person's, always — the question is only whether it is one press or one per pull request.

**Decision.**

1. **An agent may merge a pull request it opened itself, when a human has authorised that session to merge, and every one of these holds:**
   - every required check is green on the pull request's **current head**, and it is mergeable with no conflict;
   - no review thread is unresolved, and no review requests changes;
   - it touches none of `crates/pos-core/`, `crates/pos-ports/`, `crates/pos-proto/`, `.github/`, or `deploy/` — the four §6 already sends to an owner, plus deployment;
   - it touches neither `AGENTS.md` nor `docs/adr/`, so an agent can never widen its own authority or retire a decision;
   - it needs no ADR under §7;
   - it is squash-merged, like everything else.

   **Anything failing any one of those is a human's to merge**, and so is any pull request the agent did not open. An agent merging somebody else's work is not review, it is a rubber stamp with no one behind it.

2. **The authorisation is per session, and it is not inherited.** A new session starts without it. This is deliberate: the grant is a person deciding about a specific body of work in front of them, not a setting somebody flips once and forgets. An agent that cannot point at the moment it was authorised has not been.

3. **Nothing here lowers the review bar.** The `ai-assisted` label, the mandatory template fields and the owner review on the backbone crates are all unchanged. What changes is who presses a button on a change that has already satisfied every one of them.

**Consequences accepted.**

- **A bad change can now reach `main` without a second pair of eyes** — but only a bad change that is green across the full gate, conflict-free, outside the backbone, and needing no ADR. That is the class of change CI was built to judge, and if CI cannot be trusted for it, the gate is the thing to fix, not the merge button.
- **The blast radius is a revert.** Squash merge keeps history linear, so backing out an agent merge is one commit. This is the main reason the risk is acceptable and the main reason it would not be if history were not linear.
- **The exclusions will chafe.** A one-line `pos-proto` doc fix will sit waiting for a person. That is the intended cost: the boundary is drawn by directory rather than by judgement precisely so an agent cannot argue itself across it.
- **"Authorised session" is a weaker record than a signed approval.** It lives in a conversation, not in the repository. Tightening it — a label, a check, a bot that verifies the grant — is left until this has been lived with, rather than designed in advance for a problem nobody has had yet.
