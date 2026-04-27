//! In-process priority-prediction decision tree (Frame32-resident).
//!
//! Replaces the old `feature = "model"` external `tren-model` fork-exec
//! mechanism. The whole tree fits in a single 128-byte [`crate::Frame32`]
//! and inference is **branchless** (a 4-iteration loop with no early
//! returns and no allocations).
//!
//! ## Layout (`Tree32` packed inside one Frame32)
//!
//! - **slot 0**: header `[magic:8 | reserved:8 | n_samples:7 | gen:8]`
//!   (low 31 bits; bit 31 is the diagonal parity owned by Frame32).
//! - **slot 1**: low 31 bits hold internal-node feature IDs `0..7` (each
//!   is a 4-bit feature index `0..15`; that is `4 × 8 = 32` bits, but
//!   we only use 7 internal nodes here so the upper 4 bits are 0).
//! - **slot 2**: low 31 bits hold internal-node feature IDs `8..14`
//!   (the remaining 7 internal nodes; 7 × 4 = 28 bits used).
//! - **slot 3**: low 16 bits = leaf class bits (one bit per leaf, 16
//!   leaves total).
//! - **slot 4**: low 16 bits = leaf-confidence bits (one bit per leaf;
//!   1 = pure split achieved, 0 = majority-vote fallback).
//!
//! Slots 5..31 are zero in this version (room for future ensembles).
//!
//! ## Tree shape
//!
//! Depth-4 complete binary tree:
//! - 15 internal nodes (heap indices 0..14) — each holds a 4-bit
//!   feature index `f ∈ 0..16`. Decision at the node: take left if
//!   `(features >> f) & 1 == 0`, right if `1`.
//! - 16 leaves (heap indices 15..30) — each holds a 1-bit class
//!   (0 = "low priority" / 1 = "high priority").
//!
//! ## Features (16-bit vector)
//!
//! Each bit is an independently learnable threshold so the tree can
//! pick whichever cuts cleanly. See [`encode_features`].

use crate::Frame32;

/// Magic value stored in the high byte of slot-0 to identify a Frame32
/// that holds a priority tree (vs. a SUBMIT/STATE/etc. frame).
pub const TREE_MAGIC: u32 = 0x5A;

/// Number of internal nodes in a depth-4 complete binary tree.
pub const N_INTERNAL: usize = 15;
/// Number of leaves in a depth-4 complete binary tree.
pub const N_LEAVES: usize = 16;
/// Maximum depth (root descends 4 times to reach a leaf).
pub const TREE_DEPTH: usize = 4;
/// Width of the feature vector in bits.
pub const FEATURE_BITS: u8 = 16;

/// Raw, human-meaningful inputs collected at SUBMIT time.
///
/// Kept compact so the wrapper can build it without extra syscalls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    /// Number of declared dependencies (clipped at 7).
    pub dep_count: u8,
    /// Estimated transitive-descendant count for this submission's
    /// owning chain (clipped at 65 535; we only consult log2 thresholds).
    pub subtree_size: u32,
    /// Depth in the DAG (deepest dep depth + 1; clipped at 7).
    pub depth: u8,
    /// First byte of the cmd body (used as a coarse cmd-class signal).
    pub cmd_first_byte: u8,
}

impl Features {
    /// Convenience constructor used in tests.
    pub fn new(dep_count: u8, subtree_size: u32, depth: u8, cmd_first_byte: u8) -> Self {
        Self { dep_count, subtree_size, depth, cmd_first_byte }
    }
}

/// Encode raw [`Features`] into a 16-bit feature vector. Each bit is an
/// independent threshold so the decision tree can pick the cut it wants.
///
/// Bit map (LSB = bit 0):
/// - 0..2:  `dep_count >= {1, 2, 4}`
/// - 3..6:  `subtree_size >= {2, 4, 8, 16}`
/// - 7..9:  `depth >= {1, 2, 4}`
/// - 10:    `cmd starts with 'e'` (echo / env / exit / …)
/// - 11:    `cmd starts with 't'` (true / touch / time …)
/// - 12:    `cmd starts with 'f'` (false / find / for …)
/// - 13:    `cmd_first_byte` is alphabetic
/// - 14:    `dep_count == 0` (root-of-chain marker)
/// - 15:    `subtree_size >= 32` (very-large-chain marker)
pub fn encode_features(f: Features) -> u16 {
    let mut v: u16 = 0;
    let dc = f.dep_count.min(255);
    let ss = f.subtree_size;
    let dp = f.depth.min(255);

    if dc >= 1 { v |= 1 << 0; }
    if dc >= 2 { v |= 1 << 1; }
    if dc >= 4 { v |= 1 << 2; }

    if ss >= 2  { v |= 1 << 3; }
    if ss >= 4  { v |= 1 << 4; }
    if ss >= 8  { v |= 1 << 5; }
    if ss >= 16 { v |= 1 << 6; }

    if dp >= 1 { v |= 1 << 7; }
    if dp >= 2 { v |= 1 << 8; }
    if dp >= 4 { v |= 1 << 9; }

    let c = f.cmd_first_byte | 0x20; // ascii-tolower for letters
    if c == b'e' { v |= 1 << 10; }
    if c == b't' { v |= 1 << 11; }
    if c == b'f' { v |= 1 << 12; }
    if (b'a'..=b'z').contains(&c) { v |= 1 << 13; }

    if f.dep_count == 0 { v |= 1 << 14; }
    if ss >= 32         { v |= 1 << 15; }

    v
}

/// Frame32-packed priority tree.
///
/// Construct from a fitted set of `(features, label)` samples via
/// [`Tree32::train`]; serialise to a [`Frame32`] via [`Tree32::to_frame`];
/// recover via [`Tree32::from_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tree32 {
    /// Feature index (0..15) tested at each internal node, indexed by
    /// heap position 0..14.
    pub internals: [u8; N_INTERNAL],
    /// Class bit (0/1) for each leaf, indexed by leaf number 0..15
    /// (heap position `15 + leaf`).
    pub leaves: u16,
    /// Confidence mask: bit `i` == 1 iff leaf `i` was a pure split
    /// (no entropy in its training set).
    pub leaf_pure: u16,
    /// Generation counter — bumped each time we re-train. Wraps at 256.
    pub gen: u8,
}

impl Tree32 {
    /// Build an "everything is class 1" baseline tree (used as the
    /// initial state before any samples have arrived).
    pub fn baseline() -> Self {
        Tree32 {
            internals: [0u8; N_INTERNAL],
            leaves:    0xFFFF,
            leaf_pure: 0,
            gen:       0,
        }
    }

    /// Branchless inference: walk the depth-4 tree and return the leaf
    /// class bit (0 or 1).
    #[inline]
    pub fn infer(&self, features: u16) -> u8 {
        let mut idx: usize = 0;
        for _ in 0..TREE_DEPTH {
            let f = self.internals[idx] as u32;
            let bit = ((features as u32) >> (f & 0x0F)) & 1;
            idx = idx * 2 + 1 + (bit as usize);
        }
        // idx is now in 15..=30; leaf number = idx - 15.
        let leaf = idx - N_INTERNAL;
        ((self.leaves >> leaf) & 1) as u8
    }

    /// Confidence (0/1) at the leaf reached for `features`.
    #[inline]
    pub fn confidence(&self, features: u16) -> u8 {
        let mut idx: usize = 0;
        for _ in 0..TREE_DEPTH {
            let f = self.internals[idx] as u32;
            let bit = ((features as u32) >> (f & 0x0F)) & 1;
            idx = idx * 2 + 1 + (bit as usize);
        }
        let leaf = idx - N_INTERNAL;
        ((self.leaf_pure >> leaf) & 1) as u8
    }

    /// Pack this tree into a [`Frame32`].
    pub fn to_frame(&self) -> Frame32 {
        let mut f = Frame32::new();

        // slot 0: magic + gen
        let s0 = (TREE_MAGIC << 23) | (self.gen as u32);
        f.set(0, s0 & 0x7FFF_FFFF);

        // slots 1, 2: internal feature IDs (4 bits each, 8 nodes per slot
        // capacity, but we only fill 8 then 7).
        let mut s1: u32 = 0;
        for i in 0..8 {
            s1 |= ((self.internals[i] as u32) & 0xF) << (i * 4);
        }
        f.set(1, s1 & 0x7FFF_FFFF);

        let mut s2: u32 = 0;
        for i in 0..(N_INTERNAL - 8) {
            s2 |= ((self.internals[8 + i] as u32) & 0xF) << (i * 4);
        }
        f.set(2, s2 & 0x7FFF_FFFF);

        // slot 3: leaf classes (16 bits)
        f.set(3, self.leaves as u32);
        // slot 4: leaf-confidence mask
        f.set(4, self.leaf_pure as u32);

        f.update_parity();
        f
    }

    /// Recover a tree previously serialised via [`Self::to_frame`].
    /// Returns `None` if the magic byte does not match.
    pub fn from_frame(f: &Frame32) -> Option<Self> {
        let s0 = f.get(0);
        let magic = (s0 >> 23) & 0xFF;
        if magic != TREE_MAGIC {
            return None;
        }
        let gen = (s0 & 0xFF) as u8;

        let s1 = f.get(1);
        let s2 = f.get(2);
        let mut internals = [0u8; N_INTERNAL];
        for i in 0..8 {
            internals[i] = ((s1 >> (i * 4)) & 0xF) as u8;
        }
        for i in 0..(N_INTERNAL - 8) {
            internals[8 + i] = ((s2 >> (i * 4)) & 0xF) as u8;
        }

        let leaves    = (f.get(3) & 0xFFFF) as u16;
        let leaf_pure = (f.get(4) & 0xFFFF) as u16;

        Some(Tree32 { internals, leaves, leaf_pure, gen })
    }

    /// Greedy ID3-style training: at each internal node, pick the
    /// feature index that minimises **conditional Shannon entropy** of
    /// the label given the split, recurse to depth [`TREE_DEPTH`], and
    /// take a majority vote at any leaf that did not converge to a
    /// single label.
    ///
    /// Empty samples produce the [`Tree32::baseline`] tree with the
    /// supplied generation counter.
    pub fn train(samples: &[(u16, bool)], gen: u8) -> Self {
        if samples.is_empty() {
            let mut t = Self::baseline();
            t.gen = gen;
            return t;
        }

        let mut internals = [0u8; N_INTERNAL];
        let mut leaves: u16 = 0;
        let mut leaf_pure: u16 = 0;

        // For each heap node we keep the row indices it owns. Start
        // with all rows at node 0.
        let mut node_rows: Vec<Vec<u32>> = vec![Vec::new(); N_INTERNAL + N_LEAVES];
        node_rows[0] = (0..samples.len() as u32).collect();

        for idx in 0..N_INTERNAL {
            let rows = std::mem::take(&mut node_rows[idx]);
            if rows.is_empty() {
                // No data here — pick feature 0 arbitrarily; both children
                // inherit empty sets and will resolve to a 0 leaf below.
                internals[idx] = 0;
                continue;
            }

            // Pure node: no need to split further. Replicate the rows
            // into BOTH children so every reachable leaf below this
            // subtree records the same correct class — otherwise the
            // half of inference paths that don't go through "left"
            // would land on empty leaves defaulting to class 0.
            let (n0, n1) = count_labels(samples, &rows);
            if n0 == 0 || n1 == 0 {
                internals[idx] = 0;
                let left  = idx * 2 + 1;
                let right = idx * 2 + 2;
                if left  < node_rows.len() { node_rows[left]  = rows.clone(); }
                if right < node_rows.len() { node_rows[right] = rows; }
                continue;
            }

            let best = best_split(samples, &rows);
            internals[idx] = best;

            let (left_rows, right_rows): (Vec<u32>, Vec<u32>) = rows.into_iter()
                .partition(|&r| ((samples[r as usize].0 >> best) & 1) == 0);

            let left  = idx * 2 + 1;
            let right = idx * 2 + 2;
            node_rows[left]  = left_rows;
            node_rows[right] = right_rows;
        }

        for leaf_id in 0..N_LEAVES {
            let heap_idx = N_INTERNAL + leaf_id;
            let rows = &node_rows[heap_idx];
            let (n0, n1) = count_labels(samples, rows);
            let total = n0 + n1;
            let class_bit = if total == 0 {
                // No samples landed here — default to "low priority" (0)
                // so unseen feature combinations receive a conservative
                // prior.
                0
            } else if n1 >= n0 {
                1
            } else {
                0
            };
            if class_bit == 1 {
                leaves |= 1 << leaf_id;
            }
            // Pure iff at least one sample arrived AND all share a class.
            if total > 0 && (n0 == 0 || n1 == 0) {
                leaf_pure |= 1 << leaf_id;
            }
        }

        Tree32 { internals, leaves, leaf_pure, gen }
    }
}

fn count_labels(samples: &[(u16, bool)], rows: &[u32]) -> (u32, u32) {
    let mut n0 = 0u32;
    let mut n1 = 0u32;
    for &r in rows {
        if samples[r as usize].1 { n1 += 1; } else { n0 += 1; }
    }
    (n0, n1)
}

fn entropy(p1_n: u32, total: u32) -> f64 {
    if total == 0 { return 0.0; }
    let p1 = p1_n as f64 / total as f64;
    let p0 = 1.0 - p1;
    let mut e = 0.0;
    if p0 > 0.0 { e -= p0 * p0.log2(); }
    if p1 > 0.0 { e -= p1 * p1.log2(); }
    e
}

/// Pick the feature index whose split minimises conditional entropy.
fn best_split(samples: &[(u16, bool)], rows: &[u32]) -> u8 {
    let total = rows.len() as u32;
    let mut best_feat: u8 = 0;
    let mut best_ce: f64 = f64::INFINITY;

    for f in 0..FEATURE_BITS {
        let mut left_total = 0u32;
        let mut left_pos   = 0u32;
        let mut right_total = 0u32;
        let mut right_pos   = 0u32;
        for &r in rows {
            let (feat, label) = samples[r as usize];
            let bit = (feat >> f) & 1;
            if bit == 0 {
                left_total += 1;
                if label { left_pos += 1; }
            } else {
                right_total += 1;
                if label { right_pos += 1; }
            }
        }
        let l = entropy(left_pos,  left_total);
        let r = entropy(right_pos, right_total);
        let wl = left_total  as f64 / total as f64;
        let wr = right_total as f64 / total as f64;
        let ce = wl * l + wr * r;
        if ce < best_ce - 1e-12 {
            best_ce = ce;
            best_feat = f as u8;
        }
    }
    best_feat
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_threshold_bits() {
        let f0 = Features::new(0, 0, 0, b'e');
        let v0 = encode_features(f0);
        assert_eq!(v0 & 1, 0,  "dep_count=0 must clear bit 0");
        assert_ne!(v0 & (1 << 14), 0, "dep_count==0 must set bit 14");
        assert_ne!(v0 & (1 << 10), 0, "cmd 'e' must set bit 10");

        let f1 = Features::new(5, 17, 3, b'F');
        let v1 = encode_features(f1);
        assert_eq!(v1 & 0b111, 0b111, "dep_count=5 sets bits 0,1,2");
        assert_ne!(v1 & (1 << 6), 0,  "subtree_size=17 >= 16");
        assert_eq!(v1 & (1 << 15), 0, "subtree_size=17 < 32");
        assert_ne!(v1 & (1 << 12), 0, "cmd 'F' (lowercased to 'f') set bit 12");
        assert_ne!(v1 & (1 << 13), 0, "cmd alphabetic set bit 13");
        assert_eq!(v1 & (1 << 14), 0, "dep_count!=0 clear bit 14");
    }

    #[test]
    fn frame_round_trip() {
        let mut t = Tree32::baseline();
        for i in 0..N_INTERNAL { t.internals[i] = (i as u8) & 0xF; }
        t.leaves    = 0xA5A5;
        t.leaf_pure = 0x0F0F;
        t.gen       = 42;

        let frame = t.to_frame();
        assert!(frame.verify_parity(), "tree frame must satisfy diagonal parity");
        let restored = Tree32::from_frame(&frame).expect("magic ok");
        assert_eq!(restored, t);
    }

    #[test]
    fn from_frame_rejects_non_tree() {
        let mut not_a_tree = Frame32::new();
        not_a_tree.set_header(crate::OP_SUBMIT, 0);
        not_a_tree.update_parity();
        assert!(Tree32::from_frame(&not_a_tree).is_none());
    }

    #[test]
    fn pure_split_zero_misclassification() {
        // Build a dataset where label is exactly bit 5 of features.
        let mut samples = Vec::new();
        for i in 0..256u16 {
            let label = ((i >> 5) & 1) == 1;
            samples.push((i, label));
        }
        let tree = Tree32::train(&samples, 1);
        let mut wrong = 0;
        for &(feat, label) in &samples {
            let pred = tree.infer(feat) == 1;
            if pred != label { wrong += 1; }
        }
        assert_eq!(wrong, 0, "label = bit5(features) is a depth-1 separable concept; tree must be perfect");
    }

    #[test]
    fn pure_split_conjunction_two_features() {
        // label = bit3 AND bit7 — greedy ID3 splits on bit3 (or bit7),
        // recurses, and reaches conditional entropy 0 well within depth 4.
        // (XOR is the classic ID3 failure case — we deliberately pick a
        // monotone concept here so that greedy info-gain converges.)
        let mut samples = Vec::new();
        for i in 0..1024u16 {
            let f = i & 0xFFFF;
            let label = (((f >> 3) & 1) & ((f >> 7) & 1)) == 1;
            samples.push((f, label));
        }
        let tree = Tree32::train(&samples, 1);
        let mut wrong = 0;
        for &(feat, label) in &samples {
            if (tree.infer(feat) == 1) != label { wrong += 1; }
        }
        assert_eq!(wrong, 0,
            "label = bit3 ∧ bit7 is depth-2 ID3-separable; depth-4 tree must classify perfectly");
    }

    #[test]
    fn empty_training_set_is_baseline() {
        let t = Tree32::train(&[], 7);
        assert_eq!(t.gen, 7);
        // baseline returns class 1 for every input
        for f in [0u16, 0xFFFF, 0x1234, 0xAAAA] {
            assert_eq!(t.infer(f), 1);
        }
    }

    #[test]
    fn baseline_frame_round_trip() {
        let t = Tree32::baseline();
        let f = t.to_frame();
        let r = Tree32::from_frame(&f).expect("baseline magic");
        assert_eq!(r, t);
    }

    #[test]
    fn confidence_marks_pure_leaves() {
        // Wholly-determined dataset should yield pure leaves.
        let mut samples = Vec::new();
        for i in 0..256u16 {
            samples.push((i, ((i >> 2) & 1) == 1));
        }
        let t = Tree32::train(&samples, 0);
        // Every leaf reachable should be pure.
        let mut any_pure = false;
        for f in 0..256u16 {
            if t.confidence(f) == 1 { any_pure = true; }
        }
        assert!(any_pure, "at least one leaf must be flagged pure on a separable dataset");
    }

    #[test]
    fn inference_is_deterministic() {
        let samples: Vec<(u16, bool)> = (0..500u16)
            .map(|i| (i, (i & 0x55) > 30))
            .collect();
        let t = Tree32::train(&samples, 0);
        let a = t.infer(0xBEEF);
        for _ in 0..1000 { assert_eq!(t.infer(0xBEEF), a); }
    }
}
