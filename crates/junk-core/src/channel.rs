//! Channels: opaque handles for declared characteristics, and fixed-size sets of them.

use core::fmt;
use core::iter::FusedIterator;

/// Opaque handle for "a characteristic the device family declared".
///
/// The protocol layer speaks in channels; only the [`Link`](crate::Link) knows UUIDs. The
/// id is whatever index the device crate assigned in its [`GattMap`](crate::GattMap).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct Channel(pub u8);

/// A set of [`Channel`]s over all 256 ids, stored inline. Never allocates.
#[derive(Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct ChannelSet {
    words: [u64; 4],
}

impl ChannelSet {
    /// The set containing no channels.
    pub const EMPTY: ChannelSet = ChannelSet { words: [0; 4] };

    /// Word index and bit mask of `chan`.
    const fn slot(chan: Channel) -> (usize, u64) {
        ((chan.0 >> 6) as usize, 1u64 << (chan.0 & 63))
    }

    /// Adds `chan`. Returns `true` if it was not already present.
    pub const fn insert(&mut self, chan: Channel) -> bool {
        let (word, mask) = Self::slot(chan);
        let was_absent = self.words[word] & mask == 0;
        self.words[word] |= mask;
        was_absent
    }

    /// Removes `chan`. Returns `true` if it was present.
    pub const fn remove(&mut self, chan: Channel) -> bool {
        let (word, mask) = Self::slot(chan);
        let was_present = self.words[word] & mask != 0;
        self.words[word] &= !mask;
        was_present
    }

    /// Whether `chan` is in the set.
    #[must_use]
    pub const fn contains(&self, chan: Channel) -> bool {
        let (word, mask) = Self::slot(chan);
        self.words[word] & mask != 0
    }

    /// Whether the set has no channels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        let mut i = 0;
        while i < self.words.len() {
            if self.words[i] != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Number of channels in the set.
    #[must_use]
    pub const fn len(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < self.words.len() {
            n += self.words[i].count_ones() as usize;
            i += 1;
        }
        n
    }

    /// The channels in ascending id order.
    pub const fn iter(&self) -> ChannelSetIter {
        ChannelSetIter {
            words: self.words,
            word: 0,
        }
    }

    /// The channels in `self` or `other`.
    #[must_use]
    pub const fn union(&self, other: &ChannelSet) -> ChannelSet {
        let mut words = self.words;
        let mut i = 0;
        while i < words.len() {
            words[i] |= other.words[i];
            i += 1;
        }
        ChannelSet { words }
    }

    /// The channels in `self` that are not in `other`.
    #[must_use]
    pub const fn difference(&self, other: &ChannelSet) -> ChannelSet {
        let mut words = self.words;
        let mut i = 0;
        while i < words.len() {
            words[i] &= !other.words[i];
            i += 1;
        }
        ChannelSet { words }
    }

    /// Whether every channel in `other` is also in `self`.
    #[must_use]
    pub const fn is_superset(&self, other: &ChannelSet) -> bool {
        let mut i = 0;
        while i < self.words.len() {
            if self.words[i] & other.words[i] != other.words[i] {
                return false;
            }
            i += 1;
        }
        true
    }
}

impl fmt::Debug for ChannelSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl FromIterator<Channel> for ChannelSet {
    fn from_iter<I: IntoIterator<Item = Channel>>(iter: I) -> Self {
        let mut set = Self::EMPTY;
        set.extend(iter);
        set
    }
}

impl Extend<Channel> for ChannelSet {
    fn extend<I: IntoIterator<Item = Channel>>(&mut self, iter: I) {
        for chan in iter {
            self.insert(chan);
        }
    }
}

impl IntoIterator for ChannelSet {
    type Item = Channel;
    type IntoIter = ChannelSetIter;

    fn into_iter(self) -> ChannelSetIter {
        self.iter()
    }
}

impl IntoIterator for &ChannelSet {
    type Item = Channel;
    type IntoIter = ChannelSetIter;

    fn into_iter(self) -> ChannelSetIter {
        self.iter()
    }
}

/// Iterator over the channels of a [`ChannelSet`], ascending by id.
#[derive(Clone, Debug)]
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct ChannelSetIter {
    words: [u64; 4],
    /// Index of the word currently being drained; past the end once exhausted.
    word: u8,
}

impl Iterator for ChannelSetIter {
    type Item = Channel;

    fn next(&mut self) -> Option<Channel> {
        while let Some(&w) = self.words.get(usize::from(self.word)) {
            if w == 0 {
                self.word += 1;
                continue;
            }
            let bit = w.trailing_zeros();
            self.words[usize::from(self.word)] = w & (w - 1);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "`bit` indexes a set bit of a u64, so it is below 64"
            )]
            let id = (self.word << 6) | bit as u8;
            return Some(Channel(id));
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = ChannelSet { words: self.words }.len();
        (n, Some(n))
    }
}

impl ExactSizeIterator for ChannelSetIter {}

impl FusedIterator for ChannelSetIter {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn insert_contains_remove() {
        let mut set = ChannelSet::EMPTY;
        assert!(set.is_empty());
        assert!(!set.contains(Channel(7)));

        assert!(set.insert(Channel(7)));
        assert!(!set.insert(Channel(7)));
        assert!(set.contains(Channel(7)));
        assert!(!set.is_empty());
        assert_eq!(set.len(), 1);

        assert!(set.remove(Channel(7)));
        assert!(!set.remove(Channel(7)));
        assert!(!set.contains(Channel(7)));
        assert!(set.is_empty());
        assert_eq!(set, ChannelSet::default());
    }

    #[test]
    fn extreme_ids() {
        let mut set = ChannelSet::EMPTY;
        assert!(set.insert(Channel(0)));
        assert!(set.insert(Channel(255)));
        assert!(set.contains(Channel(0)));
        assert!(set.contains(Channel(255)));
        assert!(!set.contains(Channel(1)));
        assert!(!set.contains(Channel(254)));
        assert_eq!(set.len(), 2);
        assert_eq!(set.iter().collect::<Vec<_>>(), [Channel(0), Channel(255)]);
        assert!(set.remove(Channel(0)));
        assert!(set.remove(Channel(255)));
        assert!(set.is_empty());
    }

    #[test]
    fn iter_is_ascending_and_exact() {
        let set: ChannelSet = [200, 3, 64, 63, 128, 3].into_iter().map(Channel).collect();
        assert_eq!(set.len(), 5);
        let iter = set.iter();
        assert_eq!(iter.len(), 5);
        assert_eq!(
            iter.collect::<Vec<_>>(),
            [
                Channel(3),
                Channel(63),
                Channel(64),
                Channel(128),
                Channel(200)
            ]
        );
        assert_eq!((&set).into_iter().count(), 5);
        assert_eq!(set.into_iter().count(), 5);
        assert_eq!(ChannelSet::EMPTY.iter().next(), None);
    }

    #[test]
    fn union_and_superset() {
        let a: ChannelSet = [Channel(1), Channel(2)].into_iter().collect();
        let b: ChannelSet = [Channel(2), Channel(255)].into_iter().collect();
        let both = a.union(&b);
        assert_eq!(
            both.iter().collect::<Vec<_>>(),
            [Channel(1), Channel(2), Channel(255)]
        );
        assert!(both.is_superset(&a));
        assert!(both.is_superset(&b));
        assert!(both.is_superset(&ChannelSet::EMPTY));
        assert!(!a.is_superset(&b));
        assert!(!b.is_superset(&a));
        assert!(a.is_superset(&a));
        assert!(ChannelSet::EMPTY.is_superset(&ChannelSet::EMPTY));
    }

    #[test]
    fn difference() {
        let a: ChannelSet = [Channel(1), Channel(2), Channel(255)].into_iter().collect();
        let b: ChannelSet = [Channel(2), Channel(7)].into_iter().collect();
        assert_eq!(
            a.difference(&b).iter().collect::<Vec<_>>(),
            [Channel(1), Channel(255)]
        );
        assert_eq!(b.difference(&a).iter().collect::<Vec<_>>(), [Channel(7)]);
        assert_eq!(a.difference(&a), ChannelSet::EMPTY);
        assert_eq!(a.difference(&ChannelSet::EMPTY), a);
    }

    #[test]
    fn extend_and_debug() {
        let mut set = ChannelSet::EMPTY;
        set.extend([Channel(5), Channel(1)]);
        assert_eq!(set.len(), 2);
        assert_eq!(alloc::format!("{set:?}"), "{Channel(1), Channel(5)}");
    }
}
