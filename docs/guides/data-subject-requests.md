# Fulfilling a data-subject request, including in the backups

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-10
**Relates to** [ADR-0035](../adr/0035-retention-and-pii-masking.md) · [ADR-0076](../adr/0076-subject-request-tooling.md) · [ADR-0107](../adr/0107-the-buyer-is-a-subject.md) · [ADR-0124](../adr/0124-a-store-that-can-be-restored.md)

A person asks to see, take, or erase the personal data this system holds about them. This guide is
the operator's path for that — specifically for the part every other document skips, which is
**what happens to the copies**.

> **Read this first.** Deciding *whether* to act, on what lawful basis, and whether the requester is
> who they say they are, is a human judgement and not a step in a runbook. An EU-resident rights
> request (access, erasure, portability) is escalated to the Data Protection contact and fulfilled
> deliberately by them — never automatically, and never by a cron. This guide is the instrument;
> the process around it is theirs.

## Where personal data actually is

Only two places, and knowing that is most of the work:

| Where | What | Who removes it |
|---|---|---|
| The cloud's `subjects` table | The person's fields, keyed by a `SubjectId`. Events carry only the id ([ADR-0035](../adr/0035-retention-and-pii-masking.md)) | The subject-request tooling ([ADR-0076](../adr/0076-subject-request-tooling.md)), or the masking cron once the retention period passes |
| A **store's** `subjects` table | The buyer name, tax code and address a B2B invoice needs ([ADR-0107](../adr/0107-the-buyer-is-a-subject.md)) | The store's own hourly retention sweep, on the same period |
| **A store's sealed archives** | The store's whole database as it was on the day it was taken — including the buyer rows the sweeps have since redacted ([ADR-0124](../adr/0124-a-store-that-can-be-restored.md)) | The archive-retention sweep, automatically, within `archive_retention_days` |

That third row is the one this guide exists for. An archive taken on Monday preserves exactly what
Tuesday's masking removed.

## The ordinary case: erasure that does not need to touch an archive

Nearly every request resolves this way, and it is the outcome to aim for.

1. Erase through the console's subject tooling. The live row is masked: values become `[REDACTED]`,
   the id and timestamps survive so invoices still reference a subject and the books still
   reconcile. The act is audited — who, which subject, when.
2. **Tell the requester the archive window.** Copies of the erased data survive in that store's
   sealed archives until they age out, and that is a period you can state exactly: it is
   `archive_retention_days` in `cloud.toml`, and `pos_cloud` refuses to start unless it is strictly
   below the subject retention period. Past it, the last copy holding their details is gone with
   nobody having had to remember.
3. Record the date the window closes alongside the request.

This is a real answer, not a deferral: the erasure is complete in the live system immediately and
complete everywhere by a date you can name. Say the date.

## The exceptional case: an erasure that must reach the archive now

Use this only when waiting out the window is not available — a regulator's order with a deadline
inside it, or a documented risk that makes the delay itself the harm. It is deliberate work with a
real cost, and the Data Protection contact authorises it.

**Delete the affected archives. Do not edit them.**

That is the recommendation and the reason matters. An archive is a byte-for-byte record of what a
store held at an instant; a restore from one is a restore of *that shop, that day*. Opening an
archive, removing a row and re-sealing it produces a file that looks like a store archive and is
not one — it is a database the store never had, and a later restore from it would reconcile against
nothing, with the discrepancy surfacing as a receipt-number gap or an invoice referencing a subject
that is not there. Deleting is honest: the cost is a shorter recovery reach for that store, and it
is visible in the console rather than hidden in a file.

### The procedure

1. **Find which archives could hold them.** Their data entered the store's database when it was
   collected and left when the store's sweep masked it. Any archive with a `taken_at` in that window
   is in scope. The console's per-store archive list (`Fleet → the store → Backups`, or
   `GET /admin/stores/{store_id}/archives`) gives `taken_at` for every archive that still exists;
   anything older than the window is already gone.
2. **Confirm the store.** Buyer details belong to the shop that took them. Do not sweep the fleet:
   a store that never served this person has nothing of theirs, and deleting its archives costs
   recoverability for no benefit.
3. **Delete the objects.** Remove each in-scope object from the archive bucket, and its row from
   `store_archives` so the console stops offering a restore that will fail. Deleting the bytes
   *first* is the order the retention sweep uses and the order to use by hand: a row pointing at
   nothing is found and cleaned up next sweep, while an object nothing names is invisible.
4. **Sweep the tier beyond the bucket.** `deploy/backup.sh` syncs off-box with `rclone`
   ([ADR-0046](../adr/0046-backups-and-restore.md)), and an object store may keep versions or a
   soft-delete grace period. **No code in this repository can assert that those copies are gone** —
   the destination is the operator's, and its retention settings are too. Check them explicitly and
   write down what you found.
5. **Record the fulfilment.** Which store, which archives by `taken_at`, who authorised it, and the
   answer to step 4. The console's subject tooling audits the live erasure; this part is manual, so
   it is only recorded if somebody records it.
6. **Say what the recovery point is now.** Deleting archives shortens how far back that store can
   be restored. The operations owner needs to know, and the next restore drill
   (`deploy/store-restore-drill.sh`) should be run against that store to confirm what remains still
   opens.

### What this does not reach

Named plainly, because a procedure that quietly leaves something out is worse than one that says so:

- **A restore that already happened.** If an archive was restored onto a machine before the erasure,
  that machine holds the data. Find it the same way you found the archives, and treat the restored
  database as a live store.
- **The store's own disk.** A shop's `store.sqlite` is masked by its own sweep on the retention
  period, not by anything in the cloud. A store that has been offline since before the erasure
  masks it when it next runs, which it will.
- **Cross-border copies.** If the archive tier is outside the country whose residents' data it
  holds, the transfer is a separate obligation with separate paperwork
  ([ADR-0124](../adr/0124-a-store-that-can-be-restored.md)'s Consequences say so, and no schema can
  assert it for you). Deleting the copies does not retroactively make the transfer lawful.

## Why the automatic path is built the way it is

The archive window is capped **strictly below** the subject retention window, and `pos_cloud`
refuses to boot if the two are the wrong way round. That refusal is the whole design: without it,
the masking cron would look finished while a copy of the same data sat in a bucket for another
year, and nothing would ever have said so. With it, "erased" becomes true everywhere on a schedule,
and this guide's exceptional case stays exceptional.
