//! Lane generations (migration 0033): the lanes each interface's messages were stored in.
//!
//! A message's lane is worked out once, at intake: `first_lane + hash(key) % lanes` of the
//! interface's newest generation. A change of `ordering` (the lane count or the key) adds a
//! generation numbered after the last one's lanes, so old and new lanes never share a number and
//! the queue (its rows, indexes and reads) needs nothing new: a worker reads the lanes it is given.
//! What the engine does with them (which lanes wait for which) is the runtime's `lanes` module.

use super::Store;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// One generation of an interface's lanes.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LaneGeneration {
    pub interface: String,
    /// Its lanes are `first_lane .. first_lane + lanes`.
    pub first_lane: i32,
    pub lanes: i32,
    /// `ordering.key` as configured (`""` without one); None: not known (made from the lane count
    /// an older version recorded).
    pub key: Option<String>,
    /// Its messages' lanes don't follow a key (an older binary's lanes, found at a start).
    pub mixed: bool,
    pub since: DateTime<Utc>,
}

impl LaneGeneration {
    /// One past its last lane: where the next generation starts.
    pub fn end(&self) -> i32 {
        self.first_lane + self.lanes
    }
}

/// A generation to add.
#[derive(Debug, Clone, PartialEq)]
pub struct NewGeneration {
    pub interface: String,
    pub first_lane: i32,
    pub lanes: i32,
    pub key: String,
    pub mixed: bool,
}

/// What a start writes about lanes, in one transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaneWrites {
    /// Interfaces whose generations are all replaced by those in `added` (an older binary ran,
    /// or none were recorded).
    pub replaced: Vec<String>,
    pub added: Vec<NewGeneration>,
    /// `(interface, first_lane, key)`: the key of a generation that had none recorded.
    pub keys: Vec<(String, i32, String)>,
    /// `(interface, end)`: `interface_applied`, which older binaries read (see the migration).
    pub applied: Vec<(String, i32)>,
}

impl LaneWrites {
    pub fn is_empty(&self) -> bool {
        self.replaced.is_empty() && self.added.is_empty() && self.keys.is_empty() && self.applied.is_empty()
    }
}

/// The lane of a message whose ordering key is `key` (`""` without one) in a generation of `lanes`
/// lanes starting at `first_lane`. The same hash as `ductus_core::partition_of` (FNV-1a), which
/// intake uses: the store works lanes out itself only to put a resent message back in the newest
/// generation (the runtime checks the two agree).
pub fn lane_of(key: &str, first_lane: i32, lanes: i32) -> i32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    first_lane + (hash % u64::from(lanes.max(1) as u32)) as i32
}

impl Store {
    /// The lane generations of `interfaces`, by interface, oldest first.
    pub async fn lane_generations(&self, interfaces: &[String]) -> Result<Vec<LaneGeneration>> {
        sqlx::query_as(
            "SELECT interface, first_lane, lanes, key, mixed, since FROM lane_generations
             WHERE interface = ANY($1) ORDER BY interface, first_lane",
        )
        .bind(interfaces)
        .fetch_all(&self.pool)
        .await
        .context("reading the interfaces' lanes")
    }

    /// The highest lane anything waits in, per interface of `interfaces` (only those with
    /// something waiting). One probe per destination down the pending index.
    pub async fn highest_waiting_lanes(&self, interfaces: &[String]) -> Result<Vec<(String, i32)>> {
        sqlx::query_as(
            "SELECT t.interface, max(h.lane) FROM destinations t
             CROSS JOIN LATERAL (SELECT d.partition_key AS lane FROM deliveries d
                                 WHERE d.destination_id = t.id AND d.status = 0
                                 ORDER BY d.partition_key DESC LIMIT 1) h
             WHERE t.interface = ANY($1)
             GROUP BY 1 ORDER BY 1",
        )
        .bind(interfaces)
        .fetch_all(&self.pool)
        .await
        .context("reading the lanes messages wait in")
    }

    /// Write what a start decided about lanes (`LaneWrites`), in one transaction.
    pub async fn write_lanes(&self, w: &LaneWrites) -> Result<()> {
        if w.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await.context("starting transaction")?;
        if !w.replaced.is_empty() {
            sqlx::query("DELETE FROM lane_generations WHERE interface = ANY($1)")
                .bind(&w.replaced)
                .execute(&mut *tx)
                .await
                .context("replacing lanes")?;
        }
        if !w.added.is_empty() {
            let interfaces: Vec<&str> = w.added.iter().map(|g| g.interface.as_str()).collect();
            let firsts: Vec<i32> = w.added.iter().map(|g| g.first_lane).collect();
            let lanes: Vec<i32> = w.added.iter().map(|g| g.lanes).collect();
            let keys: Vec<&str> = w.added.iter().map(|g| g.key.as_str()).collect();
            let mixed: Vec<bool> = w.added.iter().map(|g| g.mixed).collect();
            sqlx::query(
                "INSERT INTO lane_generations (interface, first_lane, lanes, key, mixed)
                 SELECT * FROM unnest($1::text[], $2::int[], $3::int[], $4::text[], $5::bool[])",
            )
            .bind(&interfaces)
            .bind(&firsts)
            .bind(&lanes)
            .bind(&keys)
            .bind(&mixed)
            .execute(&mut *tx)
            .await
            .context("adding lanes")?;
        }
        if !w.keys.is_empty() {
            let interfaces: Vec<&str> = w.keys.iter().map(|k| k.0.as_str()).collect();
            let firsts: Vec<i32> = w.keys.iter().map(|k| k.1).collect();
            let keys: Vec<&str> = w.keys.iter().map(|k| k.2.as_str()).collect();
            sqlx::query(
                "UPDATE lane_generations g SET key = c.key
                 FROM unnest($1::text[], $2::int[], $3::text[]) AS c(interface, first_lane, key)
                 WHERE g.interface = c.interface AND g.first_lane = c.first_lane AND g.key IS NULL",
            )
            .bind(&interfaces)
            .bind(&firsts)
            .bind(&keys)
            .execute(&mut *tx)
            .await
            .context("recording the lanes' keys")?;
        }
        if !w.applied.is_empty() {
            let (names, ends): (Vec<&str>, Vec<i32>) = w.applied.iter().map(|(n, e)| (n.as_str(), *e)).unzip();
            sqlx::query(
                "INSERT INTO interface_applied (interface, lanes)
                 SELECT * FROM unnest($1::text[], $2::int[]) ORDER BY 1
                 ON CONFLICT (interface) DO UPDATE SET lanes = excluded.lanes, updated_at = now()
                 WHERE interface_applied.lanes <> excluded.lanes",
            )
            .bind(&names)
            .bind(&ends)
            .execute(&mut *tx)
            .await
            .context("recording the lanes interfaces run with")?;
        }
        tx.commit().await.context("committing lanes")
    }

    /// Delete an interface's generations older than the one starting at `newest` (nothing waits in
    /// them any more). Returns how many went.
    pub async fn retire_lanes(&self, interface: &str, newest: i32) -> Result<u64> {
        let done = sqlx::query("DELETE FROM lane_generations WHERE interface = $1 AND first_lane < $2")
            .bind(interface)
            .bind(newest)
            .execute(&self.pool)
            .await
            .context("retiring old lanes")?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_follow_the_generation() {
        // The empty key's FNV-1a offset basis: 0xcbf29ce484222325 % 8 = 5.
        assert_eq!(lane_of("", 0, 8), 5);
        assert_eq!(lane_of("", 64, 8), 69);
        assert_eq!(lane_of("MRN1", 10, 1), 10);
    }
}
