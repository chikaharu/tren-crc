//! `bench_priority` — measure scheduler wall-step cost on a synthetic
//! random DAG under three priority policies:
//!
//! 1. **fifo**     — submission order (no priority signal).
//! 2. **subtree**  — subtree-size descending (the new v0.4.0 default).
//! 3. **tree**     — Frame32-resident decision tree trained on
//!    completed-job labels (`feature = "model"` parity).
//!
//! Output is a single-line summary suitable for piping to a benchmark
//! harness:
//!
//! ```text
//! n=128 fifo=347 subtree=271 tree=259
//! ```
//!
//! No external services and no I/O — runs entirely in-process so it can
//! be wired into CI without a wrapper binary.

use std::collections::{BTreeSet, BinaryHeap, VecDeque};
use std::cmp::Ordering as CmpOrdering;

use tren::priority::{encode_features, Features, Tree32};

const N: usize = 128;
const SEED: u64 = 0xCAFEF00D_DEADBEEF;
const WORKERS: usize = 4;

/// Tiny xorshift64 PRNG. Good enough for benchmarks; deterministic.
struct Rng(u64);
impl Rng {
    fn new(s: u64) -> Self { Rng(s.max(1)) }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, n: u64) -> u64 { self.next_u64() % n }
}

#[derive(Clone)]
struct Node {
    deps:         Vec<usize>,
    work:         u32, // wall steps to complete
    subtree_size: u32, // computed
    depth:        u32,
}

fn make_dag(n: usize, seed: u64) -> Vec<Node> {
    let mut rng = Rng::new(seed);
    let mut nodes: Vec<Node> = Vec::with_capacity(n);
    for i in 0..n {
        // Each node may depend on 0..=2 earlier nodes.
        let max_deps = (rng.range(3)) as usize;
        let mut deps: Vec<usize> = Vec::new();
        for _ in 0..max_deps {
            if i == 0 { break; }
            let d = rng.range(i as u64) as usize;
            if !deps.contains(&d) { deps.push(d); }
        }
        let work = (rng.range(7) + 1) as u32; // 1..=7 steps
        nodes.push(Node { deps, work, subtree_size: 1, depth: 0 });
    }
    // Compute subtree_size and depth in reverse topological order
    // (indices are already topological because deps[i] < i).
    for i in 0..n {
        let dmax = nodes[i].deps.iter().map(|&d| nodes[d].depth).max().unwrap_or(0);
        nodes[i].depth = if nodes[i].deps.is_empty() { 0 } else { dmax + 1 };
    }
    for i in (0..n).rev() {
        let mut visited: BTreeSet<usize> = BTreeSet::new();
        let mut q: VecDeque<usize> = nodes[i].deps.iter().copied().collect();
        while let Some(a) = q.pop_front() {
            if !visited.insert(a) { continue; }
            for &dd in &nodes[a].deps { q.push_back(dd); }
        }
        for a in &visited { nodes[*a].subtree_size += 1; }
        // self counts as 1 (already initialised)
    }
    nodes
}

#[derive(Clone, Copy)]
struct Item { prio: u64, sub: u32, idx: usize }
impl Eq for Item {}
impl PartialEq for Item { fn eq(&self, o: &Self) -> bool { self.cmp(o) == CmpOrdering::Equal } }
impl Ord for Item {
    fn cmp(&self, o: &Self) -> CmpOrdering {
        match self.prio.cmp(&o.prio) {
            CmpOrdering::Equal => {}
            x => return x,
        }
        match self.sub.cmp(&o.sub) {
            CmpOrdering::Equal => {}
            x => return x,
        }
        // Lower idx first → reverse for max-heap.
        o.idx.cmp(&self.idx)
    }
}
impl PartialOrd for Item { fn partial_cmp(&self, o: &Self) -> Option<CmpOrdering> { Some(self.cmp(o)) } }

/// Simulate the scheduler. `priority(idx) -> (primary, secondary)` —
/// items with larger `primary` are scheduled first; `secondary` breaks
/// ties.
fn simulate<F: Fn(usize) -> (u64, u32)>(nodes: &[Node], priority: F) -> u64 {
    let n = nodes.len();
    let mut remaining: Vec<u32> = nodes.iter().map(|n| n.work).collect();
    let mut done: Vec<bool>     = vec![false; n];
    let mut ready: BinaryHeap<Item> = BinaryHeap::new();
    let mut in_flight: Vec<usize>   = Vec::new();

    let push_ready = |i: usize, ready: &mut BinaryHeap<Item>, prio: &F| {
        let (p, s) = prio(i);
        ready.push(Item { prio: p, sub: s, idx: i });
    };

    for i in 0..n {
        if nodes[i].deps.is_empty() { push_ready(i, &mut ready, &priority); }
    }

    let mut step: u64 = 0;
    let mut completed = 0usize;
    while completed < n {
        // Fill workers from ready queue.
        while in_flight.len() < WORKERS {
            match ready.pop() {
                Some(it) => in_flight.push(it.idx),
                None     => break,
            }
        }
        if in_flight.is_empty() { break; } // deadlock guard
        // Advance one wall step on every in-flight node.
        let mut finished_now: Vec<usize> = Vec::new();
        for &i in &in_flight {
            remaining[i] -= 1;
            if remaining[i] == 0 { finished_now.push(i); }
        }
        in_flight.retain(|i| !finished_now.contains(i));
        for fi in finished_now {
            done[fi] = true;
            completed += 1;
            for j in 0..n {
                if done[j] || remaining[j] != nodes[j].work { continue; }
                if nodes[j].deps.iter().all(|&d| done[d]) {
                    push_ready(j, &mut ready, &priority);
                }
            }
        }
        step += 1;
    }
    step
}

fn main() {
    let nodes = make_dag(N, SEED);

    // 1. FIFO: order by submission index → smaller idx first.
    //    To make BinaryHeap pop smaller-first, store negated idx.
    let fifo = simulate(&nodes, |i| (u64::MAX - i as u64, 0));

    // 2. Subtree-size descending (the new default).
    let subtree = simulate(&nodes, |i| (nodes[i].subtree_size as u64, 0));

    // 3. Tree-trained model. Train on (features, label) where label is
    //    "subtree_size >= 2" (proxy for critical-path membership).
    let samples: Vec<(u16, bool)> = nodes.iter().map(|n| {
        let f = Features {
            dep_count: n.deps.len().min(255) as u8,
            subtree_size: n.subtree_size,
            depth: n.depth.min(255) as u8,
            cmd_first_byte: 0,
        };
        (encode_features(f), n.subtree_size >= 2)
    }).collect();
    let model = Tree32::train(&samples, 1);
    let tree = simulate(&nodes, |i| {
        let f = Features {
            dep_count: nodes[i].deps.len().min(255) as u8,
            subtree_size: nodes[i].subtree_size,
            depth: nodes[i].depth.min(255) as u8,
            cmd_first_byte: 0,
        };
        let p = model.infer(encode_features(f)) as u64;
        (p, nodes[i].subtree_size)
    });

    println!("n={} fifo={} subtree={} tree={}", N, fifo, subtree, tree);
}
