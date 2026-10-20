// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The chain walk: one bounded chunk of a store's chained event log, checked for the first break
//! ([ADR-0131](../../../../../docs/adr/0131-a-chained-event-log.md)).

use pos_ports::{PortError, PortName};
use pos_proto::chain::{ChainBreak, ChainHash, ChainLink, ChainStatus};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::StoreId;
use rusqlite::{Connection, params};
use sha2::{Digest as _, Sha256};

use super::{ChainChunk, ChainCursor, db_error, json_error};

/// Walks up to `limit` records of a store's chain from `cursor`, and reports the first break.
///
/// Reads by `seq`, not by `event_id` ([ADR-0131](../../../../docs/adr/0131-a-chained-event-log.md)
/// decision 6): a walk ordered by ULID would re-link itself the moment two transactions
/// interleaved, and report a break that never happened.
///
/// Rows written before the migration have no `seq` and are skipped, not counted as broken —
/// `unchained` reports how many, so a store that upgraded mid-life can tell "history I cannot
/// vouch for" from "history somebody edited". It is counted on the first chunk only, because it
/// is a property of the whole log rather than of the window being walked.
pub(super) fn verify_chain_chunk(
    conn: &Connection,
    store_id: StoreId,
    cursor: &ChainCursor,
    limit: u32,
) -> Result<ChainChunk, PortError> {
    let port = PortName::EventStore;
    // Counted on the first chunk, which is the only one that has checked nothing: a chunk that
    // hands the thread back has always walked a full window. Not `after_seq == 0`, because a walk
    // resumed from a retention checkpoint starts past zero.
    let unchained: u64 = if cursor.checked == 0 {
        // Through the partial index, for the reason `read_chain_anchor` gives.
        let counted: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events INDEXED BY idx_events_store_id_unchained
                 WHERE store_id = ?1 AND seq IS NULL",
                params![store_id.to_string()],
                |row| row.get(0),
            )
            .map_err(|error| db_error(port, error))?;
        u64::try_from(counted).unwrap_or(0)
    } else {
        cursor.unchained
    };

    // One row beyond the limit, so the chunk can tell "the log continues" from "the log ends
    // here" without a second query. The extra row is inspected for neither hash nor content; it
    // only answers whether to come back.
    let window = i64::from(limit).saturating_add(1);
    let mut statement = conn
        .prepare(
            "SELECT seq, prev_hash, hash, envelope FROM events
             WHERE store_id = ?1 AND seq IS NOT NULL AND seq > ?2
             ORDER BY seq
             LIMIT ?3",
        )
        .map_err(|error| db_error(port, error))?;
    let mut rows = statement
        .query(params![
            store_id.to_string(),
            i64::try_from(cursor.after_seq).unwrap_or(i64::MAX),
            window,
        ])
        .map_err(|error| db_error(port, error))?;

    // Anchored on the first chained row rather than on one, because a store that upgraded
    // mid-life starts its chain wherever its first post-migration event landed. Deleting from
    // the *head* is still caught: the first chained row must link to genesis, and a later row
    // promoted into its place carries the hash of the record that used to precede it.
    //
    // On a resumed chunk the anchor is the previous chunk's last `seq`, so the gap check spans
    // the boundary: a record deleted either side of it still leaves the counter and the stored
    // position disagreeing.
    let mut expected_seq: Option<u64> = if cursor.after_seq == 0 {
        None
    } else {
        Some(cursor.after_seq)
    };
    let mut previous = cursor.expect_prev.clone();
    let mut checked: u64 = cursor.checked;
    let mut walked: u32 = 0;
    let mut last_seq = cursor.after_seq;
    while let Some(row) = rows.next().map_err(|error| db_error(port, error))? {
        if walked == limit {
            // The look-ahead row: the log continues past this chunk. Hand the thread back.
            return Ok(ChainChunk::More(ChainCursor {
                after_seq: last_seq,
                expect_prev: previous,
                checked,
                unchained,
            }));
        }
        let seq = u64::try_from(row.get::<_, i64>(0).map_err(|e| db_error(port, e))?).unwrap_or(0);
        let prev_hash: String = row.get(1).map_err(|error| db_error(port, error))?;
        let stored_hash: String = row.get(2).map_err(|error| db_error(port, error))?;
        let envelope_json: String = row.get(3).map_err(|error| db_error(port, error))?;

        let expected = expected_seq.map_or(seq, |previous_seq| previous_seq.saturating_add(1));
        expected_seq = Some(expected);
        if seq != expected {
            // A gap. A record was removed from the middle of the log, so the counter and the
            // stored position no longer agree.
            return Ok(ChainChunk::Finished(ChainStatus::Broken {
                at_seq: expected,
                reason: ChainBreak::MissingRecord,
            }));
        }
        if prev_hash != previous.as_str() {
            return Ok(ChainChunk::Finished(ChainStatus::Broken {
                at_seq: seq,
                reason: ChainBreak::LinkMismatch,
            }));
        }

        // Recompute from the stored bytes. This is what catches an edit: the row still links to
        // its neighbour, but its content no longer hashes to the hash beside it.
        let stored: EventEnvelope<RawPayload> =
            serde_json::from_str(&envelope_json).map_err(|error| json_error(port, error))?;
        let link = ChainLink {
            seq,
            prev_hash: ChainHash::from_hex(&prev_hash),
        };
        let recomputed = stored
            .chain_hash(&link, |bytes| Sha256::digest(bytes).into())
            .map_err(|error| json_error(port, error))?;
        if recomputed.as_str() != stored_hash {
            return Ok(ChainChunk::Finished(ChainStatus::Broken {
                at_seq: seq,
                reason: ChainBreak::ContentEdited,
            }));
        }

        previous = ChainHash::from_hex(&stored_hash);
        checked = checked.saturating_add(1);
        walked = walked.saturating_add(1);
        last_seq = seq;
    }

    // Fewer rows than the window, so this is the end of the log rather than the end of a chunk.
    Ok(ChainChunk::Finished(ChainStatus::Intact {
        checked,
        unchained,
        head: if checked == 0 { None } else { Some(previous) },
    }))
}
