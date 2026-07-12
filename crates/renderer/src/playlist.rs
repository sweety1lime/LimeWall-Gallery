//! Pure playlist rotation: given a length and a shuffle flag, decide which item
//! index shows next. No filesystem, mpv or platform dependency, so it is fully
//! unit-tested. The daemon owns the timing, the item paths and the "skip a
//! missing file" retry loop; this only tracks order and position.

/// Order and current position of a monitor's playlist.
pub struct Rotation {
    /// Item indices in play order (a permutation of `0..len`).
    order: Vec<usize>,
    /// Index into `order` of the item currently showing.
    position: usize,
    shuffle: bool,
    /// Running xorshift state for reshuffles.
    seed: u64,
}

impl Rotation {
    /// New rotation over `len` items (`len >= 1`). When shuffling, the initial
    /// order is randomized from `seed`.
    pub fn new(len: usize, shuffle: bool, seed: u64) -> Self {
        let mut seed = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        let mut order: Vec<usize> = (0..len.max(1)).collect();
        if shuffle {
            shuffle_in_place(&mut order, &mut seed);
        }
        Self {
            order,
            position: 0,
            shuffle,
            seed,
        }
    }

    /// Item index of the wallpaper currently showing.
    pub fn current(&self) -> usize {
        self.order[self.position]
    }

    /// Advance to the next slot — wrapping to the start (reshuffled, without an
    /// immediate repeat) at the end of a cycle — and return the new current
    /// item index.
    pub fn advance(&mut self) -> usize {
        if self.order.len() <= 1 {
            return self.current();
        }
        self.position += 1;
        if self.position >= self.order.len() {
            if self.shuffle {
                let last = self.order[self.order.len() - 1];
                shuffle_in_place(&mut self.order, &mut self.seed);
                if self.order[0] == last {
                    let end = self.order.len() - 1;
                    self.order.swap(0, end);
                }
            }
            self.position = 0;
        }
        self.current()
    }

    /// Position the rotation so `item` (an item index) shows now, if present.
    /// Used to resume a persisted playlist at its saved wallpaper.
    pub fn seek_to_item(&mut self, item: usize) {
        if let Some(pos) = self.order.iter().position(|&i| i == item) {
            self.position = pos;
        }
    }
}

/// In-place Fisher–Yates shuffle driven by `xorshift64`.
fn shuffle_in_place(order: &mut [usize], seed: &mut u64) {
    for i in (1..order.len()).rev() {
        let j = (xorshift64(seed) % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
}

/// A tiny, dependency-free PRNG. `state` must be non-zero.
pub fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_cycles_in_order_and_wraps() {
        let mut r = Rotation::new(3, false, 1);
        assert_eq!(r.current(), 0);
        assert_eq!(r.advance(), 1);
        assert_eq!(r.advance(), 2);
        assert_eq!(r.advance(), 0); // wrapped
        assert_eq!(r.advance(), 1);
    }

    #[test]
    fn single_item_stays_put() {
        let mut r = Rotation::new(1, false, 1);
        assert_eq!(r.current(), 0);
        assert_eq!(r.advance(), 0);
        assert_eq!(r.advance(), 0);
        let mut s = Rotation::new(1, true, 42);
        assert_eq!(s.advance(), 0);
    }

    #[test]
    fn shuffle_covers_every_item_once_per_cycle() {
        let len = 6;
        let mut r = Rotation::new(len, true, 12345);
        let mut seen = vec![r.current()];
        for _ in 1..len {
            seen.push(r.advance());
        }
        seen.sort_unstable();
        assert_eq!(
            seen,
            (0..len).collect::<Vec<_>>(),
            "one full cycle hits each item once"
        );
    }

    #[test]
    fn reshuffle_avoids_an_immediate_repeat() {
        // Across many cycle boundaries, the wrap item must differ from the last.
        let len = 4;
        let mut r = Rotation::new(len, true, 999);
        let mut prev = r.current();
        for _ in 0..200 {
            let next = r.advance();
            assert_ne!(next, prev, "no back-to-back repeat");
            prev = next;
        }
    }

    #[test]
    fn deterministic_for_a_fixed_seed() {
        let seq = |seed| {
            let mut r = Rotation::new(5, true, seed);
            let mut out = vec![r.current()];
            for _ in 0..12 {
                out.push(r.advance());
            }
            out
        };
        assert_eq!(seq(7), seq(7));
    }

    #[test]
    fn seek_to_item_resumes_at_the_saved_wallpaper() {
        let mut r = Rotation::new(4, false, 1);
        r.seek_to_item(2);
        assert_eq!(r.current(), 2);
        assert_eq!(r.advance(), 3);
    }
}
