//! Noticing that the user took the mouse back.
//!
//! After each action the agent's pointer position is remembered. If the
//! pointer is somewhere else by the next call, the user moved it: that ends
//! the agent's control until they allow it again. No global hotkey is
//! grabbed, so no other application loses a key.

use std::sync::Mutex;

use crate::backend::Point;

/// Pointer drift tolerated before it counts as the user moving the mouse.
pub const TOLERANCE_PX: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TookOver {
    pub expected: Point,
    pub found: Point,
}

#[derive(Debug, Default)]
pub struct TakeoverGuard {
    last: Mutex<Option<Point>>,
}

impl TakeoverGuard {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Point>> {
        self.last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Remember where the agent left the pointer.
    pub fn expect(&self, point: Point) {
        *self.lock() = Some(point);
    }

    /// Compare the pointer now with where the agent left it. Nothing to
    /// compare before the agent's first action.
    pub fn check(&self, current: Point) -> Result<(), TookOver> {
        match *self.lock() {
            Some(expected)
                if (expected.x - current.x).abs() > TOLERANCE_PX
                    || (expected.y - current.y).abs() > TOLERANCE_PX =>
            {
                Err(TookOver {
                    expected,
                    found: current,
                })
            }
            _ => Ok(()),
        }
    }

    /// Forget the last position (control ended).
    pub fn reset(&self) {
        *self.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_compare_before_the_first_action() {
        let guard = TakeoverGuard::default();
        assert_eq!(guard.check(Point::new(500, 500)), Ok(()));
    }

    #[test]
    fn the_pointer_where_the_agent_left_it_is_fine() {
        let guard = TakeoverGuard::default();
        guard.expect(Point::new(100, 100));
        assert_eq!(guard.check(Point::new(100, 100)), Ok(()));
        assert_eq!(guard.check(Point::new(102, 98)), Ok(()), "within tolerance");
    }

    #[test]
    fn a_moved_pointer_is_a_takeover_until_reset() {
        let guard = TakeoverGuard::default();
        guard.expect(Point::new(100, 100));
        assert_eq!(
            guard.check(Point::new(400, 120)),
            Err(TookOver {
                expected: Point::new(100, 100),
                found: Point::new(400, 120)
            })
        );
        guard.reset();
        assert_eq!(guard.check(Point::new(400, 120)), Ok(()));
    }
}
