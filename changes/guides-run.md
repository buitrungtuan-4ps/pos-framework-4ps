---
kind: added
---
- **`just guides` does what each guide says.** `cargo xtask guides --run` makes a scratch worktree of
  the last commit for each guide with marked steps, runs the guide's `cargo xtask new` commands and
  then its verify steps there, without a shell, and reports the first that fails. It catches a
  generated crate that no longer builds, which the golden files cannot. It is slow, so it is not part
  of `just preflight`; `guides --check` there remains the gate until a CI job runs it.
