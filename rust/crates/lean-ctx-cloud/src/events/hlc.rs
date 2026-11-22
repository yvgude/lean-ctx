// SPDX-License-Identifier: Apache-2.0
//! Hybrid logical clock of an engine (SPEC-ENGINE §3.3.2, C0 2.0
//! `vectors/hlc.vectors.json`).

use serde::{Deserialize, Serialize};

/// A clock reading.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Stamp {
    pub wall_ms: u64,
    pub counter: u16,
}

/// A counter above 65 535 moves the wall one millisecond on.
fn bump(wall_ms: u64, counter: u32) -> Stamp {
    match u16::try_from(counter) {
        Ok(counter) => Stamp { wall_ms, counter },
        Err(_) => Stamp {
            wall_ms: wall_ms + 1,
            counter: 0,
        },
    }
}

/// Stamp an own event: `wall = max(local, last.wall)`; `counter =
/// last.counter + 1` if the wall did not move, else 0.
pub fn send(last: Stamp, local_wall_ms: u64) -> Stamp {
    let wall = local_wall_ms.max(last.wall_ms);
    if wall == last.wall_ms {
        bump(wall, u32::from(last.counter) + 1)
    } else {
        Stamp {
            wall_ms: wall,
            counter: 0,
        }
    }
}

/// Merge a received event's stamp into the clock.
pub fn receive(last: Stamp, remote: Stamp, local_wall_ms: u64) -> Stamp {
    let wall = local_wall_ms.max(last.wall_ms).max(remote.wall_ms);
    let counter = match (wall == last.wall_ms, wall == remote.wall_ms) {
        (true, true) => u32::from(last.counter.max(remote.counter)) + 1,
        (true, false) => u32::from(last.counter) + 1,
        (false, true) => u32::from(remote.counter) + 1,
        (false, false) => 0,
    };
    bump(wall, counter)
}

/// The server's skew clamp: accepted iff `wall_ms ≤ now + 120 s` and
/// `wall_ms ≥ place_created_at − 300 s` (else `hlc_skew`).
pub fn server_accepts(wall_ms: u64, server_now_ms: u64, place_created_at_ms: u64) -> bool {
    wall_ms <= server_now_ms + 120_000 && wall_ms >= place_created_at_ms.saturating_sub(300_000)
}
