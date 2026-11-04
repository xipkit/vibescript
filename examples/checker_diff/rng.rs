//! A small deterministic generator, so a seed names one program everywhere.

/// SplitMix64's output function.
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A SplitMix64 stream.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(mix(seed ^ 0x5eed_c0de_0bad_f00d))
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.0)
    }

    /// A number below `n`, which must be positive.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True with probability `percent` in 100.
    pub fn chance(&mut self, percent: u32) -> bool {
        self.next() % 100 < u64::from(percent)
    }

    /// A number in `low..=high`; `low <= high` and `high - low + 1` must fit in `i64`.
    pub fn range(&mut self, low: i64, high: i64) -> i64 {
        low.wrapping_add_unsigned(self.next() % (high - low + 1) as u64)
    }

    /// Chooses an item, returning an error if the slice is empty.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> std::io::Result<&'a T> {
        if items.is_empty() {
            return Err(std::io::Error::other("cannot choose from an empty slice"));
        }
        items.at(self.below(items.len()))
    }

    /// An index chosen with the given nonempty weights, whose sum must fit in `u32`.
    /// If all weights are zero, returns the last index.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u32 = weights.iter().sum();
        let mut roll = (self.next() % u64::from(total.max(1))) as u32;
        for (index, &weight) in weights.iter().enumerate() {
            if roll < weight {
                return index;
            }
            roll -= weight;
        }
        weights.len() - 1
    }
}

/// Fallible access to the generator's indexed model.
pub trait Checked<T> {
    /// Returns the selected item or an error naming the invalid index.
    fn at(&self, index: usize) -> std::io::Result<&T>;
}
impl<T> Checked<T> for [T] {
    fn at(&self, index: usize) -> std::io::Result<&T> {
        self.get(index).ok_or_else(|| {
            std::io::Error::other(format!(
                "generator index {index} outside length {}",
                self.len()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Checked, Rng};

    #[test]
    fn invalid_model_indexes_are_errors() {
        let values = [42];
        assert_eq!(*values.at(0).unwrap(), 42);
        assert!(
            values
                .at(1)
                .unwrap_err()
                .to_string()
                .contains("index 1 outside length 1")
        );
        assert!(values.at(usize::MAX).is_err());
    }

    #[test]
    fn choosing_from_an_empty_slice_is_an_error() {
        assert!(Rng::new(0).pick::<u8>(&[]).is_err());
    }
}
