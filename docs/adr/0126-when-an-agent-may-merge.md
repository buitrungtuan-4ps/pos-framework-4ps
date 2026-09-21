# ADR-0126 — When an agent may press merge

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-20
**Amends** [`AGENTS.md`](../../AGENTS.md) §6, whose rule was *"A human merges. Always."*

**Context.** `AGENTS.md` §6 has said *"A human merges. Always."* since the file was written, and §8 says a rule in it beats a request that conflicts with it. Both were right about the thing they protected: **somebody accountable looks at what lands on `main`.** A machine that can open a change and also land it walks the whole distance alone, and the first anyone hears of a mistake is a shop that will not sell.

What the rule did not anticipate is the shape the work took. Five pull requests sat green, mergeable, and unreviewed-because-there-was-nothing-left-to-review for **nineteen hours**, while the one person who could press the button was asleep. The rule was not protecting anything during those hours. It was queueing.

Worth recording, because it is not obvious and it decided the sequence: **a rule that lives in the repository can only be changed by a merge, and under the old rule that merge had to be human.** There was no sequence of events in which an agent granted itself this. The first press was a person's.

**Decision.** The repo owner may permit an agent to merge. §6 now reads:

> A human merges. Always. Exception for Repo Owner can allow AI auto merged

The permission is the owner's to give and to withdraw, and it is given to a working session rather than configured once — an agent that cannot point at the moment it was authorised has not been.

**What the exception does not touch.** Three things in §6 are unchanged and still bind every merge an agent makes:

- **`pos-core`, `pos-ports`, `pos-proto` and `.github/` require an owner review.** The exception is about who presses the button, not about who reviews the backbone.
- **Squash merge only.** This is what keeps the blast radius at one revert, and it is the main reason the risk is acceptable.
- **The `ai-assisted` label, and the mandatory template fields.** §6 already says the label does not lower the review bar.

**The discipline an agent applies inside the permission** — practice, not rule, and therefore the part to argue with rather than the part to obey. An agent merging under this record should merge only a pull request that is **green on its current head**, **mergeable with no conflict**, and has **no unresolved review thread**; should resolve a conflict by merging `main` into the branch and re-running the gates rather than trusting a clean-looking diff; and should not merge a pull request it did not open, because an agent approving somebody else's work is a rubber stamp with no one behind it.

**Consequences accepted.**

- **A bad change can reach `main` without a second pair of eyes.** That is the cost, stated plainly. What bounds it is that CI here is unusually broad — 122 test suites, a browser gate that drives a real edge, `cargo deny`, and nine `xtask` checks — so the class of defect that passes all of it and still breaks a shop is small, and the class that a tired human would have caught by eye is smaller still.
- **The blast radius is a revert.** Squash merge keeps history linear, so backing out an agent's merge is one commit.
- **The permission is broad as written.** It names no conditions, so the discipline above is the agent's own and a future reader cannot tell the two apart from `AGENTS.md` alone — which is exactly why this record exists and says which is which.
- **Nothing here lets an agent widen its own authority.** Changing this rule means changing `AGENTS.md`, and the harness an agent runs under refuses to let it modify the file that governs it. That refusal is not part of this decision and cannot be relied on by it; it is simply the reason the owner wrote the line by hand.
