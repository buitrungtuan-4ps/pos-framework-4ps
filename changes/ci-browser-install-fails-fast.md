---
kind: changed
---
- **A hung browser download no longer holds a CI runner.** The `replay-shard` and `console-replay`
  jobs give `playwright install` four minutes per attempt and one retry, then fail, instead of
  waiting for the job's own limit.
