//! Recognises a double tap of Shift, Copper's capture gesture, from raw key presses.
//!
//! A tap is Shift pressed and released quickly with nothing else in between. Two taps of the
//! same Shift close together fire. Any other key cancels, so typing a capital (Shift+A), a
//! Shift-click or a held Shift never fires.

use std::time::Duration;

/// Longest a Shift may be held and still count as a tap.
const MAX_HOLD: Duration = Duration::from_millis(300);
/// Longest wait between the first tap's release and the second press.
const MAX_GAP: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// A key event, reduced to what the gesture needs. Key repeats should not be fed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    ShiftDown(Side),
    ShiftUp(Side),
    /// Escape pressed: any other key to the gesture, and the picker's way out.
    Escape,
    /// Any other key pressed.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Idle,
    FirstDown {
        side: Side,
        at: Duration,
    },
    FirstUp {
        side: Side,
        at: Duration,
    },
    SecondDown {
        side: Side,
        at: Duration,
    },
}

/// Feed it presses with a monotonic timestamp; it says when the gesture completes.
#[derive(Debug, Default)]
pub struct DoubleTap {
    state: State,
}

impl DoubleTap {
    /// True on the release that completes a double tap.
    pub fn feed(&mut self, press: Press, now: Duration) -> bool {
        let quick = |since: Duration, limit: Duration| now.saturating_sub(since) <= limit;
        let (next, fired) = match (self.state, press) {
            (_, Press::Other | Press::Escape) => (State::Idle, false),
            (State::FirstUp { side, at }, Press::ShiftDown(s)) if s == side && quick(at, MAX_GAP) => {
                (State::SecondDown { side, at: now }, false)
            }
            (State::FirstDown { side, at }, Press::ShiftUp(s)) if s == side && quick(at, MAX_HOLD) => {
                (State::FirstUp { side, at: now }, false)
            }
            (State::SecondDown { side, at }, Press::ShiftUp(s)) if s == side && quick(at, MAX_HOLD) => {
                (State::Idle, true)
            }
            // Both Shifts held together is a chord, not a tap.
            (State::FirstDown { .. } | State::SecondDown { .. }, Press::ShiftDown(_)) => (State::Idle, false),
            // Otherwise a Shift press starts over as a possible first tap.
            (_, Press::ShiftDown(side)) => (State::FirstDown { side, at: now }, false),
            (_, Press::ShiftUp(_)) => (State::Idle, false),
        };
        self.state = next;
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Press::*;
    use Side::*;

    fn run(events: &[(Press, u64)]) -> Vec<bool> {
        let mut tap = DoubleTap::default();
        events.iter().map(|&(p, ms)| tap.feed(p, Duration::from_millis(ms))).collect()
    }

    #[test]
    fn escape_between_taps_cancels_like_any_other_key() {
        let fired = run(&[
            (ShiftDown(Left), 0),
            (ShiftUp(Left), 80),
            (Escape, 120),
            (ShiftDown(Left), 200),
            (ShiftUp(Left), 260),
        ]);
        assert_eq!(fired.last(), Some(&false));
    }

    #[test]
    fn two_quick_taps_of_the_same_shift_fire_on_the_second_release() {
        let fired = run(&[(ShiftDown(Left), 0), (ShiftUp(Left), 80), (ShiftDown(Left), 200), (ShiftUp(Left), 260)]);
        assert_eq!(fired, [false, false, false, true]);
        let right = run(&[(ShiftDown(Right), 0), (ShiftUp(Right), 50), (ShiftDown(Right), 120), (ShiftUp(Right), 170)]);
        assert_eq!(right.last(), Some(&true));
    }

    #[test]
    fn typing_capitals_never_fires() {
        let fired = run(&[
            (ShiftDown(Left), 0),
            (Other, 40),
            (ShiftUp(Left), 90),
            (ShiftDown(Left), 150),
            (Other, 180),
            (ShiftUp(Left), 220),
        ]);
        assert!(fired.iter().all(|f| !f));
    }

    #[test]
    fn a_key_between_the_taps_cancels() {
        let fired = run(&[
            (ShiftDown(Left), 0),
            (ShiftUp(Left), 60),
            (Other, 100),
            (ShiftDown(Left), 150),
            (ShiftUp(Left), 200),
        ]);
        assert!(fired.iter().all(|f| !f));
    }

    #[test]
    fn slow_taps_long_holds_and_mixed_sides_dont_fire() {
        let slow_gap = run(&[(ShiftDown(Left), 0), (ShiftUp(Left), 60), (ShiftDown(Left), 500), (ShiftUp(Left), 550)]);
        let long_hold =
            run(&[(ShiftDown(Left), 0), (ShiftUp(Left), 600), (ShiftDown(Left), 700), (ShiftUp(Left), 750)]);
        let mixed = run(&[(ShiftDown(Left), 0), (ShiftUp(Left), 60), (ShiftDown(Right), 120), (ShiftUp(Right), 170)]);
        let chord = run(&[(ShiftDown(Left), 0), (ShiftDown(Right), 30), (ShiftUp(Right), 60), (ShiftUp(Left), 90)]);
        for fired in [slow_gap, long_hold, mixed, chord] {
            assert!(fired.iter().all(|f| !f), "{fired:?}");
        }
    }

    #[test]
    fn a_slow_first_tap_can_still_start_a_double_tap() {
        // The late second press becomes a new first tap.
        let fired = run(&[
            (ShiftDown(Left), 0),
            (ShiftUp(Left), 50),
            (ShiftDown(Left), 900),
            (ShiftUp(Left), 950),
            (ShiftDown(Left), 1050),
            (ShiftUp(Left), 1100),
        ]);
        assert_eq!(fired, [false, false, false, false, false, true]);
    }

    #[test]
    fn a_triple_tap_fires_once() {
        let fired = run(&[
            (ShiftDown(Left), 0),
            (ShiftUp(Left), 50),
            (ShiftDown(Left), 100),
            (ShiftUp(Left), 150),
            (ShiftDown(Left), 200),
            (ShiftUp(Left), 250),
        ]);
        assert_eq!(fired.iter().filter(|f| **f).count(), 1);
    }
}
