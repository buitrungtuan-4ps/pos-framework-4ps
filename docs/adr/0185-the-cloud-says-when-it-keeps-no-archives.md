# ADR-0185 — The cloud says when it keeps no archives

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0124](0124-a-store-that-can-be-restored.md) (the `CloudSync::archive_key` errors)

Accepted by the owner on 2026-11-03 (instruction to finish the five remaining items).

## Context

A cloud runs without store archives unless both an `[artifacts]` object store and
`archive_key_secret` are configured. Then it does not mount `POST /sync/stores/{store_id}/archive-key`,
and the route answers `404`. The `CloudSync::archive_key` port documents two causes as the same
`PortError::unavailable`: a cloud that could not be reached, and a cloud with no archive storage.
`cloud-sync-http` maps the `404` to `unavailable`, and the edge's backup loop logs every failed
round at WARN. So a store on such a cloud logs the WARN "the cloud keeps no store archives: its
archive storage is not configured" once per interval for as long as it runs (pilot run, N11).
That is a configuration the operator chose, not a fault. A WARN that never clears teaches people
to ignore the WARN that matters: the cloud really is unreachable and the store is not backed up.

## Options considered

1. **Keep `unavailable`, and have the edge match the message text.** No port change, but it
   branches on prose that any adapter may reword. Rejected.
2. **Map the `404` to `not_found`.** `NotFound` reads as "no such store", which the edge would then
   have to tell apart from the cloud keeping no archives at all.
3. **Map the `404` to `failed_precondition`.** The cloud is reachable and answering. It is in a
   state where this call cannot succeed until someone changes it, which is the meaning
   `PortError::failed_precondition` already has. It is not retryable, and that matches.
4. **A new `ErrorStatus` or a typed result for "no archives".** A `pos-proto` change for one caller.
   Rejected as speculative.

## Decision

Option 3.

- `CloudSync::archive_key` documents `PortError::failed_precondition` for "the cloud keeps no store
  archives". It keeps `PortError::unavailable` for "the cloud could not be reached, or would not
  answer now".
- `cloud-sync-http` maps the archive-key route's `404` to `failed_precondition`. Every other
  failure maps as before. The shared `CloudSync` contract suite gains a case: a cloud without
  archive storage refuses the key as `FailedPrecondition`.
- The edge's backup loop skips the round as before. It logs this refusal at INFO the first time per
  process start, and at DEBUG after that. It keeps asking each interval, so a cloud that gains
  archive storage later starts receiving archives without a restart. Every other failed round,
  `unavailable` included, still logs WARN each round.

## Consequences accepted

- A store on a cloud without archives says so once per start, at INFO, and then stays quiet.
- Any `404` from that route reads as "no archives". That includes an older cloud without the route,
  and a proxy that does not forward it. The INFO line names the status, and the operator acts on it
  the same way: the store is not being archived.
- An adapter written against the old documentation still returns `unavailable` for this case. The
  edge then logs WARN as before, so nothing is lost. The contract case catches it.
- No wire, `pos-proto` or protocol-version change. The cloud's route is untouched.
