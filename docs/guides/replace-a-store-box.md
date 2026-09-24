# Replace a store box, or take over with a spare

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-24

The store PC is not a single point of failure, because the shop's identity lives in the cloud
([ADR-0003](../adr/0003-cattle-not-pets.md)) and a replacement takes over **by hand**
([ADR-0149](../adr/0149-a-replacement-box-numbers-above-what-the-cloud-has-seen.md)). Nothing takes
over automatically: a box that looks dead is usually still selling, and two boxes selling at once
print the same receipt numbers.

This page is the short procedure. The long one, with every recovery route for a dead disk, is
[Bring a store online → Replacing the machine](bring-a-store-online.md#replacing-the-machine).

## A spare, stood up in advance

A spare is an ordinary box that is installed and not yet the store.

1. **Install it** from the console's **Move to a new box** drawer, or leave it store-less and claim
   it when needed ([ADR-0148](../adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)).
2. **Keep it switched off**, or at least unactivated, until the day. An activated spare that is left
   running is a second box that could sell.

## The takeover

1. **Take the old box off the network**, if it still answers. It keeps finishing the tables it holds
   ([ADR-0123](../adr/0123-a-superseded-box-opens-nothing-new.md)), so unplugging is what stops it.
2. **Restore the newest archive onto the spare**, if there is one
   ([ADR-0124](../adr/0124-a-store-that-can-be-restored.md)). Without one the spare starts with an
   empty log, and the cloud still has everything that was synced.
3. **Bump the lease**: in the console, **Fleet** → the store → **Issue a new lease**. The bump
   publishes the new generation **and the receipt floor** together.
4. **Activate or claim the spare.** On its first config pull it learns it is the store, and raises
   its receipt counter above the highest number the cloud has seen.
5. **Pair the tills** to the spare.

Bump before you activate: in that order the spare takes the current generation on first sight.

## What a takeover can lose, and what it cannot

| | After a takeover |
|---|---|
| Sales the old box synced | Safe in the cloud. |
| Receipt numbers the cloud has seen | Never reused: the spare numbers above them. |
| Sales made after the newest archive and not yet synced | Lost with the old box, unless its disk can be read. The archive interval bounds this; a store that wants a tighter bound sets `backup_interval_hours = 1`. |
| Receipt numbers the old box issued **offline and never synced** | Unknown to the cloud, so the spare **can** reuse them. Closing this needs disjoint number ranges per lease generation, which changes the printed number and waits for legal to confirm the format (ADR-0149). |

A warm standby that follows the old box's writes continuously is not built; ADR-0149 records why.
