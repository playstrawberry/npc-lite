//! Who is sent which NPC, and where an NPC looks. Pure functions: the plugin's tasks call them with
//! positions they read once per run.
//!
//! An NPC is sent to a player who may see it when he comes within its range, and taken away when he
//! goes beyond its range plus [`HYSTERESIS`] (a little farther, so a player walking on the edge does
//! not make it flicker).

use std::collections::BTreeSet;

/// A position.
pub type Pos = (f64, f64, f64);

/// One NPC as one player's scan sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub num: u32,
    /// The squared distance from the player.
    pub dist2: f64,
    /// Whether this player may see it (permission, hidden list).
    pub allowed: bool,
    /// The NPC's range in blocks.
    pub range: f64,
}

/// How much farther than its range an NPC is taken away.
pub const HYSTERESIS: f64 = 4.0;

/// Clicks of one player on one NPC closer together than this are one physical click (a client can
/// send one for each hand).
pub const SAME_CLICK_MS: u64 = 50;

/// What a click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickVerdict {
    Run,
    /// The same physical click again: nothing, silently.
    Duplicate,
    /// Inside the cooldown: this many milliseconds are left.
    Wait(u64),
}

/// The verdict on a click at `now` (ms) after the last run at `last`.
#[must_use]
pub fn click_verdict(last: Option<u64>, now: u64, cooldown_ms: u32) -> ClickVerdict {
    let Some(last) = last else {
        return ClickVerdict::Run;
    };
    // A clock that went back counts as no time passed.
    let since = now.saturating_sub(last);
    if since < SAME_CLICK_MS {
        ClickVerdict::Duplicate
    } else if since < u64::from(cooldown_ms) {
        ClickVerdict::Wait(u64::from(cooldown_ms) - since)
    } else {
        ClickVerdict::Run
    }
}

/// Whether a player whose eyes are at `eye` reaches the box of an entity standing at `feet` with
/// this `width` and `height`: the distance from the eyes to the box is at most `reach` (vanilla's
/// entity interaction check).
#[must_use]
pub fn reaches(eye: Pos, feet: Pos, width: f32, height: f32, reach: f64) -> bool {
    if ![eye.0, eye.1, eye.2, feet.0, feet.1, feet.2]
        .iter()
        .all(|v| v.is_finite())
    {
        return false;
    }
    let half = f64::from(width) / 2.0;
    // The nearest point of the box to the eyes, per axis.
    let gap = |v: f64, low: f64, high: f64| {
        if v < low {
            low - v
        } else if v > high {
            v - high
        } else {
            0.0
        }
    };
    let dx = gap(eye.0, feet.0 - half, feet.0 + half);
    let dy = gap(eye.1, feet.1, feet.1 + f64::from(height));
    let dz = gap(eye.2, feet.2 - half, feet.2 + half);
    dx * dx + dy * dy + dz * dz <= reach * reach
}

/// The NPCs to send and to take away: `(spawn, despawn)`, each in ascending order.
#[must_use]
pub fn diff(visible: &BTreeSet<u32>, candidates: &[Candidate]) -> (Vec<u32>, Vec<u32>) {
    let mut spawn = Vec::new();
    let mut keep = BTreeSet::new();
    for candidate in candidates {
        let shown = visible.contains(&candidate.num);
        let limit = if shown {
            candidate.range + HYSTERESIS
        } else {
            candidate.range
        };
        if candidate.allowed && candidate.dist2 <= limit * limit {
            if shown {
                keep.insert(candidate.num);
            } else {
                spawn.push(candidate.num);
            }
        }
    }
    spawn.sort_unstable();
    spawn.dedup();
    let despawn = visible
        .iter()
        .copied()
        .filter(|num| !keep.contains(num))
        .collect();
    (spawn, despawn)
}

/// The squared distance of two positions.
#[must_use]
pub fn dist2(a: Pos, b: Pos) -> f64 {
    let (x, y, z) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    x * x + y * y + z * z
}

/// The yaw and pitch (degrees, as the game counts them: yaw 0 faces +Z, 90 faces -X; a negative
/// pitch looks up) of an NPC at `from` with eyes `eye` above its feet, looking at the point `at`.
#[must_use]
pub fn look_angles(from: Pos, eye: f32, at: Pos) -> (f32, f32) {
    let dx = at.0 - from.0;
    let dy = at.1 - (from.1 + f64::from(eye));
    let dz = at.2 - from.2;
    let flat = dx.hypot(dz);
    let yaw = (-dx).atan2(dz).to_degrees();
    let pitch = -dy.atan2(flat).to_degrees();
    (yaw as f32, pitch as f32)
}

/// The index of the viewer nearest to `npc` within `range`, if any.
#[must_use]
pub fn nearest(npc: Pos, viewers: &[Pos], range: f64) -> Option<usize> {
    viewers
        .iter()
        .enumerate()
        .map(|(i, at)| (i, dist2(npc, *at)))
        .filter(|(_, d)| *d <= range * range)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Ticks until the next scan: often while a player is near an NPC, less often otherwise.
#[must_use]
pub fn next_delay(near: bool) -> u64 {
    if near { 10 } else { 20 }
}

/// What an NPC shows: head yaw, body yaw and pitch, as protocol angles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub head: u8,
    pub body: u8,
    pub pitch: u8,
}

/// The next turn of an NPC that was last sent `last` and wants to look along `yaw` and `pitch`;
/// `home` is its own rotation. `None` when nothing is worth sending; else the new turn, whether
/// the head packet is needed, and whether the body packet (body yaw and pitch) is needed.
///
/// The head turns when it moved at least two angle steps (2.8 degrees); the body follows only when
/// the head is more than 45 degrees away from it, as a player's body lags behind his head; the
/// pitch is sent when it moved two steps. The way home is always sent exactly.
#[must_use]
pub fn next_turn(last: Turn, yaw: u8, pitch: u8, home: Turn) -> Option<(Turn, bool, bool)> {
    // Steps around the circle: 250 and 0 are six steps apart.
    let steps = |a: u8, b: u8| a.wrapping_sub(b).min(b.wrapping_sub(a));
    if yaw == home.head && pitch == home.pitch {
        let head = last.head != home.head;
        let body = last.body != home.body || last.pitch != home.pitch;
        return (head || body).then_some((home, head, body));
    }
    let head = steps(last.head, yaw) >= 2;
    let new_head = if head { yaw } else { last.head };
    let follow = steps(new_head, last.body) > 32;
    let tilt = steps(last.pitch, pitch) >= 2;
    if !head && !tilt {
        return None;
    }
    let turn = Turn {
        head: new_head,
        body: if follow { new_head } else { last.body },
        pitch: if tilt { pitch } else { last.pitch },
    };
    Some((turn, head, follow || tilt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(num: u32, dist: f64, allowed: bool) -> Candidate {
        Candidate {
            num,
            dist2: dist * dist,
            allowed,
            range: 48.0,
        }
    }

    #[test]
    fn npcs_are_sent_inside_the_show_distance_and_taken_beyond_the_hide_distance() {
        let none = BTreeSet::new();
        let (spawn, despawn) = diff(
            &none,
            &[c(1, 10.0, true), c(2, 50.0, true), c(3, 47.9, true)],
        );
        assert_eq!(spawn, [1, 3]);
        assert!(despawn.is_empty());

        let shown: BTreeSet<u32> = [1, 2, 3, 4].into();
        let (spawn, despawn) = diff(
            &shown,
            &[
                c(1, 50.0, true),
                c(2, 52.5, true),
                c(3, 10.0, false),
                c(5, 1.0, true),
            ],
        );
        // 1 stays between the two distances, 2 went beyond, 3 is no longer allowed, 4 is gone
        // (removed, or in another world), 5 is new.
        assert_eq!(spawn, [5]);
        assert_eq!(despawn, [2, 3, 4]);
    }

    #[test]
    fn a_player_not_allowed_is_never_sent_the_npc() {
        let none = BTreeSet::new();
        let (spawn, despawn) = diff(&none, &[c(7, 1.0, false)]);
        assert!(spawn.is_empty());
        assert!(despawn.is_empty());
    }

    #[test]
    fn each_npc_has_its_own_range() {
        let far = |num, dist, range| Candidate {
            num,
            dist2: dist * dist,
            allowed: true,
            range,
        };
        let none = BTreeSet::new();
        let (spawn, _) = diff(&none, &[far(1, 100.0, 112.0), far(2, 100.0, 64.0)]);
        assert_eq!(spawn, [1]);
        let shown: BTreeSet<u32> = [1, 2].into();
        let (_, despawn) = diff(&shown, &[far(1, 115.0, 112.0), far(2, 68.5, 64.0)]);
        assert_eq!(despawn, [2]);
    }

    #[test]
    fn one_physical_click_runs_once_and_the_cooldown_says_how_long() {
        assert_eq!(click_verdict(None, 1000, 0), ClickVerdict::Run);
        assert_eq!(click_verdict(None, 1000, 500), ClickVerdict::Run);
        // Two packets of one click (one per hand) count once, even without a cooldown.
        assert_eq!(click_verdict(Some(1000), 1020, 0), ClickVerdict::Duplicate);
        assert_eq!(
            click_verdict(Some(1000), 1049, 500),
            ClickVerdict::Duplicate
        );
        assert_eq!(click_verdict(Some(1000), 1050, 0), ClickVerdict::Run);
        assert_eq!(
            click_verdict(Some(1000), 1300, 1000),
            ClickVerdict::Wait(700)
        );
        assert_eq!(click_verdict(Some(1000), 2000, 1000), ClickVerdict::Run);
        // A clock that went back is a wait, never a run.
        assert_eq!(
            click_verdict(Some(5000), 4000, 1000),
            ClickVerdict::Duplicate
        );
    }

    #[test]
    fn reach_is_measured_from_the_eyes_to_the_box() {
        let feet = (0.0, 64.0, 0.0);
        // Eyes 6 blocks away along x from the box's side (box half width 0.3).
        assert!(reaches((6.29, 65.0, 0.0), feet, 0.6, 1.8, 6.0));
        assert!(!reaches((6.4, 65.0, 0.0), feet, 0.6, 1.8, 6.0));
        // Above the head: measured to the top of the box.
        assert!(reaches((0.0, 71.79, 0.0), feet, 0.6, 1.8, 6.0));
        assert!(!reaches((0.0, 71.9, 0.0), feet, 0.6, 1.8, 6.0));
        // Inside the box.
        assert!(reaches((0.1, 65.0, 0.1), feet, 0.6, 1.8, 0.0));
        assert!(!reaches((f64::NAN, 65.0, 0.0), feet, 0.6, 1.8, 6.0));
    }

    #[test]
    fn distances_are_squared() {
        assert_eq!(dist2((0.0, 0.0, 0.0), (3.0, 4.0, 12.0)), 169.0);
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn an_npc_turns_to_the_point_it_looks_at() {
        let at_eye = |x: f64, z: f64| (x, 1.62, z);
        let (yaw, pitch) = look_angles((0.0, 0.0, 0.0), 1.62, at_eye(0.0, 5.0));
        assert!(close(yaw, 0.0) && close(pitch, 0.0), "{yaw} {pitch}");
        let (yaw, _) = look_angles((0.0, 0.0, 0.0), 1.62, at_eye(-5.0, 0.0));
        assert!(close(yaw, 90.0), "{yaw}");
        let (yaw, _) = look_angles((0.0, 0.0, 0.0), 1.62, at_eye(5.0, 0.0));
        assert!(close(yaw, -90.0), "{yaw}");
        let (yaw, _) = look_angles((0.0, 0.0, 0.0), 1.62, at_eye(0.0, -5.0));
        assert!(close(yaw.abs(), 180.0), "{yaw}");
        let (_, pitch) = look_angles((0.0, 0.0, 0.0), 1.62, (0.0, 6.62, 5.0));
        assert!(close(pitch, -45.0), "{pitch}");
        let (_, pitch) = look_angles((0.0, 0.0, 0.0), 1.62, (0.0, -3.38, 5.0));
        assert!(close(pitch, 45.0), "{pitch}");
        // A point straight above or at the eyes gives finite angles.
        let (yaw, pitch) = look_angles((0.0, 0.0, 0.0), 1.62, (0.0, 1.62, 0.0));
        assert!(yaw.is_finite() && pitch.is_finite());
    }

    #[test]
    fn the_nearest_viewer_inside_the_range_is_chosen() {
        let viewers = [(10.0, 0.0, 0.0), (3.0, 0.0, 0.0), (0.0, 0.0, 4.0)];
        assert_eq!(nearest((0.0, 0.0, 0.0), &viewers, 8.0), Some(1));
        assert_eq!(nearest((0.0, 0.0, 0.0), &viewers, 2.0), None);
        assert_eq!(nearest((0.0, 0.0, 0.0), &[], 8.0), None);
    }

    #[test]
    fn the_head_turns_first_and_the_body_follows_past_45_degrees() {
        let home = Turn {
            head: 0,
            body: 0,
            pitch: 0,
        };
        let at = |head, body, pitch| Turn { head, body, pitch };
        // Nothing moved, or less than two steps.
        assert_eq!(next_turn(at(10, 0, 0), 10, 0, home), None);
        assert_eq!(next_turn(at(10, 0, 0), 11, 1, home), None);
        // The head alone.
        assert_eq!(
            next_turn(at(10, 0, 0), 20, 0, home),
            Some((at(20, 0, 0), true, false))
        );
        // Past 45 degrees (32 steps) the body follows to the head.
        assert_eq!(
            next_turn(at(20, 0, 0), 40, 0, home),
            Some((at(40, 40, 0), true, true))
        );
        assert_eq!(
            next_turn(at(20, 0, 0), 32, 0, home),
            Some((at(32, 0, 0), true, false))
        );
        // Yaw wraps: 250 is 6 steps from 0.
        assert_eq!(
            next_turn(at(0, 0, 0), 250, 0, home),
            Some((at(250, 0, 0), true, false))
        );
        assert_eq!(
            next_turn(at(250, 0, 0), 200, 0, home),
            Some((at(200, 200, 0), true, true))
        );
        // A pitch change sends the body packet with the body where it is.
        assert_eq!(
            next_turn(at(20, 0, 0), 20, 5, home),
            Some((at(20, 0, 5), false, true))
        );
        assert_eq!(
            next_turn(at(20, 0, 0), 21, 250, home),
            Some((at(20, 0, 250), false, true))
        );
        // Home is sent exactly, even one step away, with the body only when it moved.
        assert_eq!(
            next_turn(at(1, 0, 0), 0, 0, home),
            Some((home, true, false))
        );
        assert_eq!(
            next_turn(at(40, 40, 0), 0, 0, home),
            Some((home, true, true))
        );
        assert_eq!(next_turn(home, 0, 0, home), None);
    }

    #[test]
    fn scans_are_more_frequent_only_while_someone_is_near() {
        assert_eq!(next_delay(true), 10);
        assert_eq!(next_delay(false), 20);
    }
}
