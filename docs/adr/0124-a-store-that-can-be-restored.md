# ADR-0124 — A store that can be restored: the shop seals its own archive

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-10
**Answers** [ADR-0046](0046-backups-and-restore.md)'s deferred store half, and with it the store leg of the restore drill
**Relates to** [ADR-0015](0015-sqlite-access.md) (the single-writer thread this snapshot deliberately does not queue behind) · [ADR-0107](0107-the-buyer-is-a-subject.md) (the personal data that makes the seal necessary) · [ADR-0035](0035-retention-and-pii-masking.md) / [ADR-0076](0076-subject-request-tooling.md) (the retention clock the archive's own retention is set under) · [ADR-0025](0025-receipt-number-authority.md) (the gapless counter the archive is mostly there to save) · [ADR-0123](0123-a-superseded-box-opens-nothing-new.md) (whose replacement procedure this exists to make safe) · [ADR-0117](0117-a-headless-store-keeps-a-log.md) (the outbox drain that bounds what an archive can be missing) · [ADR-0007](0007-in-house-vs-dependency.md) (why the cipher is bought and the format is not)

**Context.** [ADR-0046](0046-backups-and-restore.md) built the cloud half of durability — WAL archiving, daily `pg_dump`, the off-box `rclone` tier, the `.pre-update` snapshot, and a weekly drill that proves the dump restores. It deferred the other half in one sentence:

> the store half is edge WAL shipping, which lands with the machine-replacement work (`docs/roadmap.md` P9, spike A4), so this ADR builds the cloud-DB half now and the drill grows its store half there.

Spike A4 has not been run. So today **no copy of a store's database exists anywhere but the store**, and the cost of that is specific rather than theoretical. The event log is continuously published to the cloud, so most of a shop's trading is already off-box; what is *not*, and dies with the disk, is:

- the **outbox backlog** — every event committed and not yet published, which on a shop that has been offline since Friday is the weekend's revenue;
- the **gapless receipt counter** and its allocations ([ADR-0025](0025-receipt-number-authority.md)) — a per-store, on-box authority. Losing it restarts a shop's receipt numbering at one, mid-year;
- the **intake ledger**, which is what makes a relayed channel order idempotent, and the **daily queue counter**;
- the **subject store** ([ADR-0107](0107-the-buyer-is-a-subject.md)) — the buyer name, tax code and address a B2B invoice needs, held at the till and published nowhere;
- the paired devices and the print-agent bindings, which are re-doable at the shop and only at the shop.

[ADR-0123](0123-a-superseded-box-opens-nothing-new.md) sharpened this. Its replacement procedure — the one an operator follows with a dead till on the counter — instructs copying `store.sqlite` off the old disk, *because that is the only way to recover unpublished events*. That is a recovery story whose single premise is that the disk is readable, and the case a backup exists for is the case where it is not.

The **A4 question is still open, and this record does not answer it.** Litestream-style WAL shipping gives minutes of recovery point; a periodic whole-database archive gives one interval. This is the cheaper thing that can exist now and be proven now, and it is a strict improvement on nothing.

**Decision.**

1. **A periodic whole-database archive, not WAL shipping, and the record says which.** Every store takes a full, consistent, compressed, sealed copy of its database on a schedule and ships it off the box. The recovery point is one interval, not one WAL segment: a shop that loses its disk halfway between archives loses the sales since the last one. That is worse than what A4 would buy and enormously better than the current answer, which is everything. When A4 returns a verdict, continuous shipping *joins* this rather than replacing it — [ADR-0046](0046-backups-and-restore.md)'s own reasoning applies unchanged at the edge, that a WAL stream replays onto a base and without a periodic base the archive can only be accumulated, never restored.

2. **The copy is taken with `VACUUM INTO`, on its own connection, and never by copying the file.** The database is WAL ([ADR-0015](0015-sqlite-access.md)), so at any instant the committed truth is spread across `store.sqlite` and its `-wal` sidecar. Copying the main file yields a database that opens, reads, and is missing every sale since the last checkpoint — a backup that looks fine until somebody restores it, which is the exact failure ADR-0046 built a drill to catch. `VACUUM INTO` asks SQLite for the answer instead: one read transaction, one fully checkpointed output file.

   It runs on a **connection of its own**, not on the single writer thread, because a shop must not stop selling for the duration of its backup. ADR-0015 already permits extra connections for reading, and `VACUUM INTO` is documented as read-only with respect to the source, so a sale in progress is not waiting behind it. The one wrinkle is recorded where it bites: that connection is opened read-*write*, because SQLite refuses `VACUUM INTO` on a connection opened `SQLITE_OPEN_READ_ONLY` — the flag is checked before SQLite notices that the only file being written is the new one.

3. **The shop seals its own archive. The cloud relays bytes it cannot read.** This is the decision the rest follow from, and the reason is [ADR-0107](0107-the-buyer-is-a-subject.md): a store's `subjects` table is the one place at a till where personal data sits, and nothing publishes it — the log carries a `SubjectId`, and `pos_proto::pii` makes a name in an event payload a compile error. Shipping the database is therefore the **first time** buyer details would leave the shop at all. Sealing at the till means they leave as bytes nobody downstream can read: not the relay that carries them, not the object store that keeps them, and not the off-box tier ADR-0046 syncs them to, which is the leg most likely to be a third party and the leg most likely to be outside the country.

   Sealing in the cloud instead would have been less code and a smaller dependency at the edge. It was rejected because it puts store-collected personal data in cloud memory and cloud logs in the ordinary case, for no gain that the seal itself does not already provide.

4. **XChaCha20-Poly1305 over deflate, bound to the store it came from.** An archive is `"P4PSTORE"`, a format byte, a 24-byte nonce, and `XChaCha20Poly1305(deflate(snapshot))`. Three properties are load-bearing:

   - **Authenticated, not merely encrypted.** A flipped bit in a transfer or an altered object in a bucket fails to open, rather than restoring a subtly wrong shop.
   - **The store id is associated data**, so an archive is cryptographically bound to the shop it came from. The failure that guards against is not theft, it is a **mix-up**: a fleet of archives in one bucket and an operator restoring at midnight. One shop's trading cannot be restored onto another shop's till by accident.
   - **A 192-bit nonce**, so a nonce drawn from the OS CSPRNG for each archive needs no counter to stay unique across a fleet and across years — a counter is state, and state is the thing a backup system cannot rely on having.

   Deflate before the seal, because sealed bytes do not compress and the wire out of a shop is its uplink; a real store database compresses roughly ten to one. The order is safe here because an archive is a whole database written once, not a channel mixing attacker-chosen text with a secret.

   Wrong key, wrong store and altered bytes are **one** error on purpose. An authenticated cipher cannot tell them apart, and a message that guessed would be a message that misleads an operator into hunting for a key that was never the problem. "These are not archive bytes at all" *is* distinguishable, and gets its own message, because "you gave me the wrong file" is actionable.

   The cipher is bought, not written ([ADR-0007](0007-in-house-vs-dependency.md)); the framing is ours, because a magic number and a nonce are not the hard part. `chacha20` is already in the lock, so what enters the graph is the AEAD wrapper — `aead`, `poly1305`, `cipher` — and not a cipher implementation.

5. **The key is per store, minted by the cloud, revealed once in the console — and the cloud keeps a copy. What that buys, and what it does not, is written here rather than implied.** The cloud is already the authority for everything else about a store: its credential, its lease, its configuration. Making it the authority for the backup key too means a till can fetch its key over the channel it already has, and an operator who lost the printout can recover the archive rather than discovering at the worst moment that the backups were ciphertext all along.

   The honest consequence: **a compromise of the cloud reaches these archives.** What the seal buys is the tier beyond it — the object store and the off-box destination, which is where the data travels furthest and is guarded least. What it does not buy is secrecy from the operator's own cloud. An archive key the cloud never held would buy that, and would cost the ability to recover from a lost printout; for a chain running a thousand tills that is the wrong trade, and it is a trade rather than an oversight.

6. **The archive's own retention is set *below* the subject-retention window, so personal data ages out of the archive by itself.** [ADR-0035](0035-retention-and-pii-masking.md)'s sweep masks a subject after its retention period, at the store and in the cloud. An archive taken before that sweep holds the unmasked row for as long as the archive is kept, so keeping archives longer than subjects would quietly make the sweep decorative. Capping archive retention under it means the last copy of an unmasked buyer expires on its own, with no job to run and nothing to remember.

   An erasure request that must reach the archive before then is a **documented manual procedure**, not a promise the backups silently break: identify the archives inside the window, and either let them expire or destroy and re-take them. Written down and named as manual, in the same register [ADR-0076](0076-subject-request-tooling.md) uses. The alternative — rewriting sealed archives on request — would mean holding every archive open and re-sealing a fleet's history per request, and would make the archive mutable, which is most of what makes it trustworthy.

7. **Restoring is `pos-edge archive`, run on a till, and the key never travels as an argument.** The place a store archive is restored *to* is a store machine, and a store machine has this binary on it already, so a separate tool would be a second thing to ship and sign. Three verbs — `seal`, `open`, `verify` — and the key comes from `POS_EDGE_ARCHIVE_KEY` rather than the command line, because an argument is in `ps` output, in shell history, and in the terminal recording of the incident everybody is watching.

   `verify` is the drill. It opens the archive, runs SQLite's own `PRAGMA integrity_check` over what came out, and fails loudly if the database inside is damaged — which is the difference between a backup and a file, and the whole thesis of [ADR-0046](0046-backups-and-restore.md).

8. **An archive that will not fit in memory is a refusal that names the size, not a till that falls over.** `BlobStore` states plainly that an object must fit in memory on both sides, and `docs/architecture.md` §8 sizes a store backup in tens of megabytes. Past a fixed cap the answer is an error saying so. A store that has genuinely outgrown a whole-database backup is a decision for an operator — and the honest signal that A4's continuous shipping has become the necessary thing rather than the better one.

**Rejected.**

- **Copying `store.sqlite`, with or without its `-wal`.** Rejected as the specific trap this record exists to close: the one-file copy loses committed sales silently, and the two-file copy captures two moments that disagree. Neither fails at backup time; both fail at restore time, months later.
- **Taking the snapshot on the writer thread.** Rejected: it is the simplest change and it stalls every screen in the shop for the length of the backup. `VACUUM INTO` does not need the write lock, so paying for it would be paying for nothing.
- **Litestream now, ahead of A4.** Rejected as prejudging a spike whose whole purpose is to answer whether it survives Windows, power loss and a flaky uplink (`docs/roadmap.md` A4). Nothing here blocks that verdict, and a periodic base is required either way.
- **Sealing in the cloud.** Rejected: see decision 3. It would put buyer details that today never leave the shop into cloud memory and cloud logs, for no gain over sealing them a hop earlier.
- **A key the cloud never holds.** Rejected: see decision 5. It buys secrecy from the operator's own cloud at the cost of every unrecoverable archive after a lost printout, and it makes an automated drill impossible.
- **Leaving `subjects` out of the archive.** Rejected by the owner explicitly, and the reasoning is sound: a restore that yields a *working store* is the point, and a store that has lost its buyers cannot reissue the invoices it is legally obliged to be able to reissue. The seal plus a shorter retention is the mitigation, not omission.
- **A per-table logical export instead of a database file.** Rejected: it needs a writer per table and drifts from the schema on every migration, and what it produces is not something the edge can simply open.

**Consequences.**

- `store-sqlite` gains `snapshot_to` and `integrity_check` — two blocking free functions, deliberately not `async` methods on `SqliteStore`, because this crate carries `tokio` with only the `sync` feature and the caller that owns a runtime is the one that should decide where the blocking happens.
- `pos-edge` gains `backup` (the format, the key, seal and open) and an `archive` subcommand, and two dependencies: `chacha20poly1305` and `flate2`. Both are already represented in the lock — `chacha20` through the `rand` 0.10 line, `flate2` through the cloud's PNG decoder — so this adds direct-dependency lines and a small AEAD wrapper, not a subtree.
- A store's personal data now leaves the shop, sealed, for the first time. **Under Vietnam's PDPD (Decree 13/2023) that is a processing activity and, if the off-box tier is outside Vietnam, a cross-border transfer**: it needs a lawful basis on record, and either a transfer agreement or explicit consent. The archive is a new item on the store's data inventory, and the cap in decision 6 is what keeps its retention defensible. A fork that points the off-box tier at a third-party provider needs a processor agreement with them. This is stated as a requirement on the operator, not as something the code can assert on their behalf.
- The restore drill can finally grow its store leg: `verify` is runnable in CI against a synthetic archive and by a human against a real one, which is the same split `deploy/restore-drill.sh` already documents for the cloud half.

**What this deliberately does not do.**

- **It does not make the recovery point minutes.** One interval is the promise; A4 is where minutes come from.
- **It does not deduplicate or do incrementals.** Every archive is the whole database. For a shop-sized database that is the right trade against a format nobody can open by hand in an emergency.
- **It does not verify a restored store *trades*.** `integrity_check` proves the file is a database; that the edge comes up on it and reconciles every bill is the human drill, and it stays in `docs/gate-register.md` as one.

**Amendment 1 (2026-09-10) — the cloud's copy of the key is wrapped, or decision 5 is not true.**

Decision 5 said the cloud keeps a copy of each store's key, and that what the seal buys is *the
tier beyond the cloud*: the object store, and the off-box destination `rclone` syncs to. Building
the cloud half showed that the second half of that sentence was false as designed.

[`deploy/backup.sh`](../../deploy/backup.sh) ships a `pg_dump` of the cloud database off-box with
`rclone` ([ADR-0046](0046-backups-and-restore.md)) — to the **same tier** the sealed archives sync
to. A plaintext key column would therefore have put the key and the ciphertext it opens in one
bucket, and the seal would have bought nothing at exactly the tier it was bought for. Not a
weakness in the cipher; a weakness in where the key was going to live.

So a stored key is **wrapped** under `archive_key_secret`, a 32-byte value `bootstrap.sh` mints
into the box's `cloud.toml` and which is not in the dump — the same posture
`internal_shared_secret` already has ([ADR-0097](0097-internal-route-authentication.md),
[ADR-0044](0044-fork-and-deploy.md)). Three consequences worth stating:

- **The residue moves rather than disappearing.** The wrapping secret is on the same box as the
  database, so a compromise of the *box* still reaches both. What this buys is the tier beyond the
  box, which is where the data travels furthest and is guarded least — which is what decision 5
  claimed and, without this, would not have delivered.
- **Its absence turns store archives off, and does not weaken them.** A cloud with no
  `archive_key_secret` does not mount the archive routes and says so at boot, on the same
  principle as `[artifacts]`: a route that always answers `503` is worse than one honestly absent.
  A malformed value is a boot refusal, because a passphrase where a 32-byte key belongs would mint
  keys nobody could ever unwrap and nobody would find out until a restore.
- **The layering enforces it.** `store-postgres` moves the wrapped text and holds no secret that
  could open it; only `pos_cloud::archive` unwraps. "The database never sees a usable key" is
  therefore a property of which crate holds what, not a rule a reviewer has to remember.

**Rejected here too: rotating the secret re-wraps every key.** It would have to, and that is a
migration over every row with the old secret still available — worth building when a rotation is
actually needed, and not before. Until then, changing `archive_key_secret` orphans every stored
key, which is why `bootstrap.sh` mints it once and leaves it alone.

**Delivery.** In slices, each green on its own: **(1)** the snapshot and the sealed format, with the `pos-edge archive` tool; **(2)** the wire — the schedule at the edge, the `CloudSync` upload, the cloud's ciphertext sink over `BlobStore`, and the key the cloud mints; **(3)** retention, the console's view of when each store last archived, and the drill's store leg. Until slice 2 landed, an operator could seal and restore by hand with the tool, which was already more than existed.

**Delivery note (slice 2, complete).** Slices 1 and 2 are in the tree; slice 3 is not. Three things were decided while building the wire and are recorded here rather than in a commit message:

- **`archive-key` is a `POST`, not a `GET`.** The first call *mints*, and a `GET` with a side effect is one a caching proxy or a browser prefetch may perform unasked. The mint is insert-if-absent in one statement, so a prefetch could not have created a *second* key — but the verb would still have been describing the wrong thing, and the next reader would have believed it.
- **The plaintext working copy goes beside the database, never in the system temp directory.** `VACUUM INTO` writes a whole second database, and `/tmp` on a real box is frequently a tmpfs far smaller than a store — an archive loop pointed there fails on exactly the busy shops that most need it. The copy is removed on the succeeding *and* the failing path.
- **The loop does not consult the lease.** A box superseded under [ADR-0123](0123-a-superseded-box-opens-nothing-new.md) holds precisely the events its replacement does not — the ones committed after the cut-over began and never published. Stopping its backups stops the backups of the only copy, so the gate that closes its doors to new business deliberately leaves archiving open.

The interval is `config.toml`'s `backup_interval_hours` (default 24, `0` off with a start-up warning). The first archive is taken five minutes after boot rather than immediately or a whole interval later: immediately would put a snapshot inside every OTA restart and every crash-loop iteration, and a whole interval would mean a box restarted daily — an ordinary shop — never archives at all.

**Tested by.** `crates/adapters/store-sqlite/tests/snapshot.rs` — a snapshot of a *live* store carries every receipt number the WAL had not checkpointed; one taken while the writer is mid-burst is a gapless prefix rather than a torn read; vacuuming over an existing file is refused rather than merged. `crates/pos-edge/tests/store_archive.rs` — a till that has traded is sealed, opened on a different machine, and re-issues every bill its own receipt number with the B2B buyer's name intact; the wrong key, another shop's id, one flipped bit and half a download all refuse, and the refused restore leaves nothing on disk that looks like one. `crates/pos-edge/tests/backup_loop.rs` — one round of the loop against a database that is still open and trading: what reaches the cloud carries the archive magic and not the SQLite header, the working copy is gone afterwards, and the bytes the cloud received open back into a store with the same receipt numbers; a cloud that will not issue a key ends the round *before* the snapshot, so a box with a down link does not vacuum its whole database every interval to find out. `crates/pos-contract-tests/src/cloud_sync.rs` — every `CloudSync`, present and future, owes the same key twice and refuses unopenable bytes as `InvalidArgument` rather than as something retryable. `crates/pos-cloud/tests/cloud.rs` — the two routes are the store's own door, the stored column is not the key it wraps, and a `taken_at` that is missing or zero is refused rather than defaulted to now.
