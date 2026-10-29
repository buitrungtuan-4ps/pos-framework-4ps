---
kind: changed
---
- **A hung browser download no longer holds a CI runner.** The `replay-shard` and `console-replay`
  jobs install the browser's system libraries in their own step (bounded at 12 minutes, not retried,
  so dpkg is never left locked) and give the browser download itself four minutes per attempt and
  one retry, instead of waiting for the job's own limit.
