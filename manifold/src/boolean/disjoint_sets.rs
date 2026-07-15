//! Union-find (`disjoint_sets.h`) — a SERIAL port of Wenzel Jakob's lock-free `DisjointSets`.
//!
//! The C++ packs `(rank << 32) | parent` into an atomic `u64` per node and uses `compare_exchange`
//! to stay correct under concurrent `unite`. Single-threaded there's no contention, so the CAS retry
//! loops collapse to a single unconditional write — but the RESULT is bit-identical because the
//! union-by-rank policy and its tie-break are preserved exactly:
//! - Attach the smaller-rank root under the larger-rank one; equal ranks bump the survivor's rank.
//! - On a rank TIE the LOWER-indexed root wins (becomes the representative). This determinism matters:
//!   the representative of each component is where `Winding03` samples the winding number, so a
//!   different tie rule could sample a different point.
//!
//! `find` does path HALVING (rewire each node to its grandparent) exactly as the C++ does — this only
//! shortens paths, never changes which root a node resolves to, so it's determinism-neutral.

/// Union-find over `[0, size)`. `data[i] = (rank << 32) | parent`, matching the C++ bit layout so the
/// tie-break reproduces bit-for-bit.
#[derive(Clone, Debug)]
pub struct DisjointSets {
    data: Vec<u64>,
}

impl DisjointSets {
    /// `size` singletons, each its own parent with rank 0.
    pub fn new(size: usize) -> Self {
        Self {
            data: (0..size as u64).collect(),
        }
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Is it empty?
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    fn parent(&self, id: u32) -> u32 {
        self.data[id as usize] as u32
    }

    #[inline]
    fn rank(&self, id: u32) -> u32 {
        ((self.data[id as usize] >> 32) as u32) & 0x7FFF_FFFF
    }

    /// The root of `id`, applying path halving as it climbs (verbatim `findImpl`).
    fn find_impl(&mut self, mut id: u32) -> u32 {
        while id != self.parent(id) {
            let value = self.data[id as usize];
            let new_parent = self.parent(value as u32); // grandparent
            let new_value = (value & 0xFFFF_FFFF_0000_0000) | new_parent as u64;
            if value != new_value {
                self.data[id as usize] = new_value;
            }
            id = new_parent;
        }
        id
    }

    /// The representative (root) of `id`.
    pub fn find(&mut self, id: usize) -> usize {
        self.find_impl(id as u32) as usize
    }

    /// Merge the sets containing `id1` and `id2`; return the surviving representative. Union by rank,
    /// lower-index wins on a tie.
    pub fn unite(&mut self, id1: usize, id2: usize) -> usize {
        let mut id1 = self.find_impl(id1 as u32);
        let mut id2 = self.find_impl(id2 as u32);
        if id1 == id2 {
            return id1 as usize;
        }
        let mut r1 = self.rank(id1);
        let mut r2 = self.rank(id2);
        // Ensure id1 is the one attached UNDER id2: swap when id1 has the larger rank, or on a tie the
        // smaller index (so the lower index becomes the surviving parent).
        if r1 > r2 || (r1 == r2 && id1 < id2) {
            core::mem::swap(&mut id1, &mut id2);
            core::mem::swap(&mut r1, &mut r2);
        }
        // Point id1 at id2, keeping id1's own rank in the high bits.
        self.data[id1 as usize] = ((r1 as u64) << 32) | id2 as u64;
        if r1 == r2 {
            // Equal ranks ⇒ the tree grew a level; bump the survivor's rank.
            self.data[id2 as usize] = (((r2 + 1) as u64) << 32) | id2 as u64;
        }
        id2 as usize
    }

    /// Are `id1` and `id2` in the same set?
    pub fn same(&mut self, id1: usize, id2: usize) -> bool {
        self.find(id1) == self.find(id2)
    }

    /// Label every element with a compact component index and return `(labels, count)`
    /// (`disjoint_sets.h` `connectedComponents`). Labels are assigned in first-seen-root order over
    /// `0..len`, so the numbering is deterministic (the exact index order may differ from C++, but
    /// [`crate::mesh::Mesh::decompose`] compares parts as a SET, so it doesn't matter).
    pub fn connected_components(&mut self) -> (Vec<usize>, usize) {
        let n = self.len();
        let mut root2label = vec![usize::MAX; n];
        let mut labels = vec![0usize; n];
        let mut count = 0;
        for (v, label) in labels.iter_mut().enumerate() {
            let r = self.find(v);
            if root2label[r] == usize::MAX {
                root2label[r] = count;
                count += 1;
            }
            *label = root2label[r];
        }
        (labels, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singletons_are_their_own_roots() {
        let mut d = DisjointSets::new(5);
        assert_eq!(d.len(), 5);
        assert!(!d.is_empty());
        for i in 0..5 {
            assert_eq!(d.find(i), i);
        }
        assert!(!d.same(0, 1));
    }

    #[test]
    fn unite_merges_and_finds_agree() {
        let mut d = DisjointSets::new(6);
        d.unite(0, 1);
        d.unite(2, 3);
        d.unite(1, 3); // now {0,1,2,3} together; {4},{5} apart
        assert!(d.same(0, 3));
        assert!(d.same(0, 2));
        assert!(!d.same(0, 4));
        // One root across the whole merged set.
        let r = d.find(0);
        for v in [1, 2, 3] {
            assert_eq!(d.find(v), r);
        }
    }

    #[test]
    fn rank_tie_lower_index_wins() {
        // Two rank-0 singletons united: equal ranks ⇒ the LOWER index becomes the representative.
        let mut d = DisjointSets::new(2);
        let root = d.unite(1, 0);
        assert_eq!(root, 0, "lower index wins the rank-0 tie");
        assert_eq!(d.find(1), 0);
        // Symmetric: order of arguments doesn't change the winner.
        let mut d2 = DisjointSets::new(2);
        assert_eq!(d2.unite(0, 1), 0);
    }

    #[test]
    fn union_by_rank_attaches_shorter_under_taller() {
        // Build a rank-1 tree {0,1} (root 0) and a rank-0 singleton {2}. Uniting attaches the
        // singleton under the taller tree's root, keeping the rank-1 root as representative.
        let mut d = DisjointSets::new(3);
        assert_eq!(d.unite(0, 1), 0); // rank(0) becomes 1
        let root = d.unite(2, 0);
        assert_eq!(
            root, 0,
            "the rank-1 root survives over the rank-0 singleton"
        );
        assert_eq!(d.find(2), 0);
        assert_eq!(d.find(1), 0);
    }

    #[test]
    fn idempotent_unite_of_same_set() {
        let mut d = DisjointSets::new(3);
        d.unite(0, 1);
        let r = d.find(0);
        assert_eq!(d.unite(0, 1), r); // re-uniting returns the existing root, no-op
        assert_eq!(d.find(1), r);
    }
}
