//! How many cache files download at once. A good line takes 12 in parallel;
//! on a bad one, 12 streams starve each other into stall timeouts. So every
//! stall or timeout drops one level (12 → 6 → 3 → 1) and a long run of
//! successes climbs one back.

/// Files in flight at each level.
const LEVELS: [usize; 4] = [1, 3, 6, 12];
/// Successes in a row before climbing a level.
const CLIMB_AFTER: usize = 24;
/// Each slowdown doubles the run needed to climb again (up to this), so a
/// line that only copes with 3 doesn't bounce between 3 and 6 forever.
const CLIMB_MAX: usize = 384;
/// Fewest files failing in a row that mean the connection is gone.
const GIVE_UP_MIN: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pace {
    level: usize,
    /// Bumped on every change of level.
    epoch: u64,
    run: usize,
    climb_after: usize,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            level: LEVELS.len() - 1,
            epoch: 0,
            run: 0,
            climb_after: CLIMB_AFTER,
        }
    }
}

impl Pace {
    /// Files to keep in flight now.
    pub fn limit(&self) -> usize {
        LEVELS[self.level]
    }

    /// Stamp a request with this when it starts.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// A file arrived.
    pub fn success(&mut self) {
        self.run += 1;
        if self.run >= self.climb_after && self.level + 1 < LEVELS.len() {
            self.level += 1;
            self.run = 0;
            self.epoch += 1;
        }
    }

    /// A request that started at `started` timed out or stalled. When the
    /// line chokes every request in flight fails at once: only those started
    /// at the current level slow it down, so they cost one level, not four.
    pub fn slow(&mut self, started: u64) {
        self.run = 0;
        if started != self.epoch {
            return;
        }
        self.climb_after = (self.climb_after * 2).min(CLIMB_MAX);
        if self.level > 0 {
            self.level -= 1;
            self.epoch += 1;
        }
    }

    /// `failures` files in a row failed every attempt: the connection is
    /// gone. Two rounds at the current level, and never fewer than
    /// `GIVE_UP_MIN` (24 at full speed, as before; 8 one at a time, where
    /// each failure can take minutes).
    pub fn give_up(&self, failures: usize) -> bool {
        failures >= (2 * self.limit()).max(GIVE_UP_MIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_full_speed_and_halves_on_stalls() {
        let mut pace = Pace::default();
        assert_eq!(pace.limit(), 12);
        let mut seen = vec![pace.limit()];
        for _ in 0..5 {
            let epoch = pace.epoch();
            pace.slow(epoch);
            seen.push(pace.limit());
        }
        assert_eq!(seen, vec![12, 6, 3, 1, 1, 1]);
    }

    #[test]
    fn a_burst_of_stalls_from_one_moment_costs_one_level() {
        let mut pace = Pace::default();
        let started = pace.epoch();
        // Twelve requests in flight all stall together.
        for _ in 0..12 {
            pace.slow(started);
        }
        assert_eq!(pace.limit(), 6);
        // A request started after the slowdown does count.
        let later = pace.epoch();
        pace.slow(later);
        assert_eq!(pace.limit(), 3);
    }

    #[test]
    fn climbs_back_on_sustained_success_more_slowly_each_time() {
        let mut pace = Pace::default();
        pace.slow(pace.epoch()); // 6, next climb needs 48
        let mut successes = 0;
        while pace.limit() == 6 {
            pace.success();
            successes += 1;
        }
        assert_eq!((pace.limit(), successes), (12, 48));
        // A stall in the middle of a run resets it.
        pace.slow(pace.epoch()); // 6, next climb needs 96
        for _ in 0..95 {
            pace.success();
        }
        pace.slow(pace.epoch() + 1); // stale request: no level change
        assert_eq!(pace.limit(), 6);
        for _ in 0..95 {
            pace.success();
        }
        assert_eq!(pace.limit(), 6);
        pace.success();
        assert_eq!(pace.limit(), 12);
        // Never above the top level, never below one.
        for _ in 0..10_000 {
            pace.success();
        }
        assert_eq!(pace.limit(), 12);
    }

    #[test]
    fn the_climb_requirement_is_capped() {
        let mut pace = Pace::default();
        for _ in 0..20 {
            pace.slow(pace.epoch());
            pace.success();
        }
        assert_eq!(pace.limit(), 1);
        assert_eq!(pace.climb_after, CLIMB_MAX);
    }

    #[test]
    fn gives_up_sooner_at_low_parallelism() {
        let mut pace = Pace::default();
        assert!(!pace.give_up(23));
        assert!(pace.give_up(24));
        pace.slow(pace.epoch()); // 6
        assert!(!pace.give_up(11) && pace.give_up(12));
        pace.slow(pace.epoch()); // 3
        assert!(!pace.give_up(7) && pace.give_up(8));
        pace.slow(pace.epoch()); // 1
        assert!(!pace.give_up(7) && pace.give_up(8));
    }
}
