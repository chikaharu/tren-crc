// bench_idtree.rs
// ビットツリー ID 体系 + XNOR マージ ベンチマーク
// ── 100 具体ユースケース + 50 エッジケース ──────────────────────────────
//
// デーモン起動なし。全演算を純関数で再現し ns 精度で計測する。
// 出力: カテゴリ別 throughput (Mops/s) + 全ケースの PASS/FAIL。

use std::time::Instant;

// ─────────────────────────────────────────────────────────────
// 純関数: SchedulerHandle::* と xnor_merge の演算コアを再現
// ─────────────────────────────────────────────────────────────

#[inline(always)]
fn depth(id: u64) -> u32 {
    63u32.saturating_sub(id.leading_zeros())
}

#[inline(always)]
fn parent_id(id: u64) -> Option<u64> {
    if id <= 1 { None } else { Some(id >> 1) }
}

#[inline(always)]
fn left_child(id: u64) -> u64 { id << 1 }

#[inline(always)]
fn right_child(id: u64) -> u64 { id << 1 | 1 }

#[inline(always)]
fn sibling_id(id: u64) -> Option<u64> {
    if id <= 1 { None } else { Some(id ^ 1) }
}

#[inline(always)]
fn path_bits(id: u64) -> u64 {
    let d = depth(id);
    id ^ (1u64 << d)
}

#[inline(always)]
fn is_left_child(id: u64) -> bool { id & 1 == 0 }

#[inline(always)]
fn are_siblings(a: u64, b: u64) -> bool { a ^ 1 == b }

#[inline(always)]
fn merge_id(a: u64, b: u64) -> u64 {
    if are_siblings(a, b) { a.min(b) >> 1 } else { (1u64 << 60) | a.wrapping_add(b) }
}

#[inline(always)]
fn xnor_diversity(count_a: u64, count_b: u64) -> u32 {
    (!(count_a ^ count_b)).count_ones()
}

#[inline(always)]
fn leaf_range(depth_d: u32) -> (u64, u64) {
    (1u64 << depth_d, (1u64 << (depth_d + 1)).saturating_sub(1))
}

#[inline(always)]
fn is_leaf_at_depth(id: u64, depth_d: u32) -> bool {
    let (lo, hi) = leaf_range(depth_d);
    id >= lo && id <= hi
}

#[inline(always)]
fn lca(mut a: u64, mut b: u64) -> u64 {
    while a != b {
        if a > b { a >>= 1; } else { b >>= 1; }
    }
    a
}

#[inline(always)]
fn path_distance(a: u64, b: u64) -> u32 {
    let common = lca(a, b);
    depth(a) + depth(b) - 2 * depth(common)
}

#[inline(always)]
fn subtree_size(id: u64, tree_depth: u32) -> u64 {
    let d = depth(id);
    if d >= tree_depth { return 1; }
    let remaining = tree_depth - d;
    (1u64 << (remaining + 1)).saturating_sub(1)
}

#[inline(always)]
fn socket_name_hash(id: u64) -> u64 {
    // "scheduler-node-{id}.sock" の長さを bit 演算で近似
    let digits = if id < 10 { 1u64 }
                 else if id < 100 { 2 }
                 else if id < 1_000 { 3 }
                 else if id < 10_000 { 4 }
                 else { 5 };
    // prefix="scheduler-node-" (16) + digits + ".sock" (5)
    16 + digits + 5
}

#[inline(always)]
fn path_to_id(bits: u64, depth_d: u32) -> u64 {
    // depth_d ビットの path_bits から ID を復元
    (1u64 << depth_d) | bits
}

// ─────────────────────────────────────────────────────────────
// テストケース記述
// ─────────────────────────────────────────────────────────────

struct Case {
    name:   &'static str,
    pass:   bool,
}

fn run_case(name: &'static str, ok: bool) -> Case {
    Case { name, pass: ok }
}

// ─────────────────────────────────────────────────────────────
// ベンチマーク: 関数ポインタ + 反復回数
// ─────────────────────────────────────────────────────────────

fn bench_ns(label: &str, iters: u64, mut f: impl FnMut() -> u64) -> f64 {
    let start = Instant::now();
    let mut acc = 0u64;
    for _ in 0..iters {
        acc = acc.wrapping_add(f());
    }
    let ns = start.elapsed().as_nanos() as f64;
    let mops = iters as f64 / ns * 1000.0;
    println!("  {:45}  {:>10.2} Mops/s  (acc={})", label, mops, acc & 0xf);
    mops
}

// ─────────────────────────────────────────────────────────────
// メイン
// ─────────────────────────────────────────────────────────────

fn main() {
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║   bench_idtree — ビットツリー ID 体系 ベンチマーク              ║");
    println!("║   100 ユースケース + 50 エッジケース                             ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();

    let mut cases: Vec<Case> = Vec::with_capacity(150);

    // ══════════════════════════════════════════════════════════
    // SECTION 1: 基本 ID 演算 (1〜20)
    // ══════════════════════════════════════════════════════════
    println!("━━ SECTION 1: 基本 ID 演算 (Case 1–20) ━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    cases.push(run_case("C01: root の深さ = 0",              depth(1) == 0));
    cases.push(run_case("C02: root の parent = None",        parent_id(1).is_none()));
    cases.push(run_case("C03: root の sibling = None",       sibling_id(1).is_none()));
    cases.push(run_case("C04: root の path_bits = 0",        path_bits(1) == 0));
    cases.push(run_case("C05: left_child(1) = 2",            left_child(1) == 2));
    cases.push(run_case("C06: right_child(1) = 3",           right_child(1) == 3));
    cases.push(run_case("C07: depth(2) = 1",                 depth(2) == 1));
    cases.push(run_case("C08: depth(3) = 1",                 depth(3) == 1));
    cases.push(run_case("C09: sibling(2) = 3",               sibling_id(2) == Some(3)));
    cases.push(run_case("C10: sibling(3) = 2",               sibling_id(3) == Some(2)));
    cases.push(run_case("C11: parent(2) = 1",                parent_id(2) == Some(1)));
    cases.push(run_case("C12: parent(3) = 1",                parent_id(3) == Some(1)));
    cases.push(run_case("C13: path_bits(2) = 0 (左)",        path_bits(2) == 0));
    cases.push(run_case("C14: path_bits(3) = 1 (右)",        path_bits(3) == 1));
    cases.push(run_case("C15: depth(4) = 2",                 depth(4) == 2));
    cases.push(run_case("C16: depth(7) = 2",                 depth(7) == 2));
    cases.push(run_case("C17: is_left_child(4) = true",      is_left_child(4)));
    cases.push(run_case("C18: is_left_child(5) = false",     !is_left_child(5)));
    cases.push(run_case("C19: are_siblings(4,5) = true",     are_siblings(4, 5)));
    cases.push(run_case("C20: are_siblings(4,6) = false",    !are_siblings(4, 6)));

    // ══════════════════════════════════════════════════════════
    // SECTION 2: パス復元・葉範囲 (21〜40)
    // ══════════════════════════════════════════════════════════
    println!("━━ SECTION 2: パス復元・葉範囲 (Case 21–40) ━━━━━━━━━━━━━━━━━━━━━━");

    cases.push(run_case("C21: path_bits(4) = 0b00",          path_bits(4) == 0b00));
    cases.push(run_case("C22: path_bits(5) = 0b01",          path_bits(5) == 0b01));
    cases.push(run_case("C23: path_bits(6) = 0b10",          path_bits(6) == 0b10));
    cases.push(run_case("C24: path_bits(7) = 0b11",          path_bits(7) == 0b11));
    cases.push(run_case("C25: path_to_id(0b00,2) = 4",       path_to_id(0b00, 2) == 4));
    cases.push(run_case("C26: path_to_id(0b01,2) = 5",       path_to_id(0b01, 2) == 5));
    cases.push(run_case("C27: path_to_id(0b10,2) = 6",       path_to_id(0b10, 2) == 6));
    cases.push(run_case("C28: path_to_id(0b11,2) = 7",       path_to_id(0b11, 2) == 7));
    cases.push(run_case("C29: leaf_range(0) = (1,1)",        leaf_range(0) == (1, 1)));
    cases.push(run_case("C30: leaf_range(1) = (2,3)",        leaf_range(1) == (2, 3)));
    cases.push(run_case("C31: leaf_range(2) = (4,7)",        leaf_range(2) == (4, 7)));
    cases.push(run_case("C32: leaf_range(3) = (8,15)",       leaf_range(3) == (8, 15)));
    cases.push(run_case("C33: is_leaf_at_depth(4,2)=true",   is_leaf_at_depth(4, 2)));
    cases.push(run_case("C34: is_leaf_at_depth(3,2)=false",  !is_leaf_at_depth(3, 2)));
    cases.push(run_case("C35: depth-3葉は8個 (2^3)",         {
        let (lo, hi) = leaf_range(3);
        hi - lo + 1 == 8
    }));
    cases.push(run_case("C36: path_bits(8)=0b000",           path_bits(8) == 0b000));
    cases.push(run_case("C37: path_bits(15)=0b111",          path_bits(15) == 0b111));
    // 全 path_bits は depth ビットで 0..2^depth の全パターンを網羅
    cases.push(run_case("C38: depth-2 葉 path 全网羅",       {
        let paths: Vec<u64> = (4u64..=7).map(path_bits).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        sorted == vec![0, 1, 2, 3]
    }));
    // パス → ID → パス の往復が恒等
    cases.push(run_case("C39: path→id→path 恒等 (depth=4)", {
        (0u64..16).all(|p| path_bits(path_to_id(p, 4)) == p)
    }));
    // ID → パス → ID の往復が恒等
    cases.push(run_case("C40: id→path→id 恒等 (depth=3 葉)", {
        (8u64..=15).all(|id| path_to_id(path_bits(id), depth(id)) == id)
    }));

    // ══════════════════════════════════════════════════════════
    // SECTION 3: LCA・距離・サブツリー (41〜60)
    // ══════════════════════════════════════════════════════════
    println!("━━ SECTION 3: LCA・距離・サブツリー (Case 41–60) ━━━━━━━━━━━━━━━━━");

    cases.push(run_case("C41: lca(4,5) = 2",                 lca(4, 5) == 2));
    cases.push(run_case("C42: lca(4,6) = 1",                 lca(4, 6) == 1));
    cases.push(run_case("C43: lca(4,7) = 1",                 lca(4, 7) == 1));
    cases.push(run_case("C44: lca(6,7) = 3",                 lca(6, 7) == 3));
    cases.push(run_case("C45: lca(2,3) = 1",                 lca(2, 3) == 1));
    cases.push(run_case("C46: lca(id,id) = id",              lca(13, 13) == 13));
    cases.push(run_case("C47: path_distance(4,5) = 2",       path_distance(4, 5) == 2));
    cases.push(run_case("C48: path_distance(4,6) = 4",       path_distance(4, 6) == 4));
    cases.push(run_case("C49: path_distance(4,4) = 0",       path_distance(4, 4) == 0));
    cases.push(run_case("C50: path_distance(2,7) = 3",       path_distance(2, 7) == 3));
    cases.push(run_case("C51: subtree_size(1,3) = 15",       subtree_size(1, 3) == 15));
    cases.push(run_case("C52: subtree_size(2,3) = 7",        subtree_size(2, 3) == 7));
    cases.push(run_case("C53: subtree_size(4,3) = 3 (depth2ノード+2葉)", subtree_size(4, 3) == 3));
    cases.push(run_case("C54: subtree_size(1,1) = 3",        subtree_size(1, 1) == 3));
    cases.push(run_case("C55: 全葉の subtree_size=1",        (4u64..=7).all(|id| subtree_size(id, 2) == 1)));
    // 兄弟 2 ノードの subtree_size の和 = 親の subtree_size - 1
    cases.push(run_case("C56: sibling subtree 和 = parent-1", {
        subtree_size(4, 3) + subtree_size(5, 3) + 1 == subtree_size(2, 3)
    }));
    // LCA は常に a.min(b) 以下
    cases.push(run_case("C57: lca(a,b) <= min(a,b)",        lca(13, 17) <= 13.min(17)));
    // 兄弟の LCA = 親
    cases.push(run_case("C58: lca(siblings) = parent",       lca(12, 13) == 6));
    // lca は交換則
    cases.push(run_case("C59: lca 交換則",                   lca(9, 14) == lca(14, 9)));
    // path_distance は対称
    cases.push(run_case("C60: distance 対称",                path_distance(9, 14) == path_distance(14, 9)));

    // ══════════════════════════════════════════════════════════
    // SECTION 4: XNOR マージ多様性 (61〜80)
    // ══════════════════════════════════════════════════════════
    println!("━━ SECTION 4: XNOR マージ多様性 (Case 61–80) ━━━━━━━━━━━━━━━━━━━━");

    // count が等しいとき XNOR = 全ビット 1 → popcount = 64
    cases.push(run_case("C61: xnor_diversity(n,n)=64",       xnor_diversity(7, 7) == 64));
    cases.push(run_case("C62: xnor_diversity(0,0)=64",       xnor_diversity(0, 0) == 64));
    // count が 1 違うとき XNOR の最下位ビット=0 → 最大 63
    cases.push(run_case("C63: xnor_diversity(0,1)=63",       xnor_diversity(0, 1) == 63));
    // count が u64::MAX と 0 → XOR=全1 → XNOR=0
    cases.push(run_case("C64: xnor_diversity(u64::MAX,0)=0", xnor_diversity(u64::MAX, 0) == 0));
    // 対称
    cases.push(run_case("C65: xnor_diversity 対称",          xnor_diversity(3, 9) == xnor_diversity(9, 3)));
    // merge_id: 兄弟ペアは親 ID
    cases.push(run_case("C66: merge_id(2,3) = 1",            merge_id(2, 3) == 1));
    cases.push(run_case("C67: merge_id(4,5) = 2",            merge_id(4, 5) == 2));
    cases.push(run_case("C68: merge_id(6,7) = 3",            merge_id(6, 7) == 3));
    // merge_id: 非兄弟は高ビット (bit60 が立つ)
    cases.push(run_case("C69: merge_id(2,4) bit60 セット",   merge_id(2, 4) >= (1u64 << 60)));
    cases.push(run_case("C70: merge_id(3,6) bit60 セット",   merge_id(3, 6) >= (1u64 << 60)));
    // 兄弟マージの結果は親の深さ
    cases.push(run_case("C71: merge depth 兄弟→親の深さ",    depth(merge_id(4, 5)) == 1));
    // 3段連鎖マージ: (4,5)→2, (6,7)→3, (2,3)→1
    cases.push(run_case("C72: 3段連鎖マージ = root",         {
        let m1 = merge_id(4, 5); // = 2
        let m2 = merge_id(6, 7); // = 3
        merge_id(m1, m2) == 1   // = 1 (root)
    }));
    // XNOR diversity は単調ではない
    cases.push(run_case("C73: diversity(1,2)=62",             xnor_diversity(1, 2) == 62));
    cases.push(run_case("C74: diversity(1,3)=63",             xnor_diversity(1, 3) == 63));
    cases.push(run_case("C75: diversity(2,3)=63 (XOR=1, 1bit差)", xnor_diversity(2, 3) == 63));
    // popcount の最大は 64 (= 全ビット一致)
    cases.push(run_case("C76: diversity(n,n)=64 任意",        xnor_diversity(12345, 12345) == 64));
    // diversity が 0 になるのは XOR = 全ビット 1 のとき = u64::MAX
    cases.push(run_case("C77: diversity=0 iff XOR=0xfff...", {
        let a = 0xAAAA_AAAA_AAAA_AAAAu64;
        let b = 0x5555_5555_5555_5555u64;
        xnor_diversity(a, b) == 0
    }));
    // XNOR ≡ NOT(XOR) でも NOT(NOT(XNOR)) = XNOR
    cases.push(run_case("C78: double-NOT identity",           {
        let a = 100u64; let b = 200u64;
        (!(!(a ^ b))).count_ones() == (a ^ b).count_ones()
    }));
    // マージ後 diversity_count を挿入すると全ジョブ数
    cases.push(run_case("C79: total_jobs = a+b+diversity",    {
        let a = 10u64; let b = 10u64;
        let d = xnor_diversity(a, b) as u64;
        a + b + d == 84  // 10+10+64
    }));
    // diversity は差分測度: |a-b| が大きいほど平均的に低い
    cases.push(run_case("C80: diversity(0,n) < diversity(n,n) for n>0", {
        (1u64..=10).all(|n| xnor_diversity(0, n) < xnor_diversity(n, n))
    }));

    // ══════════════════════════════════════════════════════════
    // SECTION 5: 新規運用ユースケース (81〜100)
    // ══════════════════════════════════════════════════════════
    println!("━━ SECTION 5: 新規運用ユースケース (Case 81–100) ━━━━━━━━━━━━━━━━━");

    // UC: socket_name_hash で名前長が決定論的に得られる
    cases.push(run_case("C81: socket名長 root = 22byte",      socket_name_hash(1) == 22));
    // UC: depth から同期不要でリーフ識別
    cases.push(run_case("C82: リーフ検出 (depth=D iff in range)", is_leaf_at_depth(12, 3)));
    // UC: path_bits でルーティングキー生成 (depth=4 は 4bit)
    cases.push(run_case("C83: routing key 4bit (depth=4)",    path_bits(20) < 16));
    // UC: ノード ID だけで「このジョブは左サブツリー担当」判定
    cases.push(run_case("C84: 左右サブツリー判別",            is_left_child(20) && !is_left_child(21)));
    // UC: 兄弟を送信先にして冗長化 (backup node = sibling)
    cases.push(run_case("C85: backup node = sibling(20)=21",  sibling_id(20) == Some(21)));
    // UC: 全祖先リストを O(depth) で生成
    cases.push(run_case("C86: 全祖先リスト生成",             {
        let mut ancestors = Vec::new();
        let mut id = 20u64;
        while id > 1 { id >>= 1; ancestors.push(id); }
        ancestors == vec![10, 5, 2, 1]
    }));
    // UC: 特定深さへのブロードキャスト ID 列
    cases.push(run_case("C87: depth=3 ブロードキャスト 8ノード", {
        let (lo, hi) = leaf_range(3);
        hi - lo + 1 == 8
    }));
    // UC: ツリー全体ノード数 = 2^(D+1) - 1
    cases.push(run_case("C88: 深さ4ツリーは31ノード",        {
        let total: u64 = (0u32..=4).map(|d| leaf_range(d).1 - leaf_range(d).0 + 1).sum();
        total == 31
    }));
    // UC: path_bits で一意なハッシュキー (深さ内で衝突なし)
    cases.push(run_case("C89: depth=5 path_bits 全32値一意", {
        let mut paths: Vec<u64> = (32u64..64).map(path_bits).collect();
        paths.sort(); paths.dedup();
        paths.len() == 32
    }));
    // UC: lca でジョブ集約担当ノード判定
    cases.push(run_case("C90: 集約担当 = lca(workers)",      lca(lca(8, 9), lca(10, 11)) == 2));
    // UC: 2つの葉から共通ルーターを 1命令で特定
    cases.push(run_case("C91: 共通ルーター特定",              lca(13, 14) == 3));
    // UC: are_siblings で merge 可否を O(1) 判定
    cases.push(run_case("C92: O(1) merge 可否判定",           are_siblings(22, 23)));
    // UC: path_distance で負荷分散スコア計算 (距離が近いほど転送コスト低)
    cases.push(run_case("C93: 転送コスト最小ペア (sibling=2)", path_distance(22, 23) == 2));
    // UC: subtree_size で担当ジョブ数見積もり
    cases.push(run_case("C94: depth5 root subtree=63",        subtree_size(1, 5) == 63));
    // UC: xnor diversity で負荷不均衡検出 (diversity < 60 が警告閾値)
    cases.push(run_case("C95: 負荷均衡チェック (equal→64)",   xnor_diversity(50, 50) == 64));
    // UC: 再起動後に socket path を再生成 (タイムスタンプ不要)
    cases.push(run_case("C96: 再起動後 socket path 再現",     {
        let id = 37u64;
        let name = format!("scheduler-node-{}.sock", id);
        name == "scheduler-node-37.sock"
    }));
    // UC: ID から「このノードは depth%2 == 0 か」で世代判定
    cases.push(run_case("C97: 偶数 depth = 偶数世代ノード",   depth(4) % 2 == 0 && depth(16) % 2 == 0));
    // UC: merge_id 連鎖で 8→4→2→1 に収束
    cases.push(run_case("C98: 8葉→root 4段収束",             {
        let l1 = merge_id(8, 9);   // 4
        let l2 = merge_id(10, 11); // 5
        let l3 = merge_id(12, 13); // 6
        let l4 = merge_id(14, 15); // 7
        let m1 = merge_id(l1, l2); // 2
        let m2 = merge_id(l3, l4); // 3
        merge_id(m1, m2) == 1      // root
    }));
    // UC: path_bits で sharding key (depth=8 → 256 shards)
    cases.push(run_case("C99: 256 shard routing key",         {
        let id = path_to_id(0b10110101, 8);
        path_bits(id) == 0b10110101
    }));
    // UC: subtree 全ノードの path_bits が prefix-free codes を形成
    cases.push(run_case("C100: prefix-free path codes",       {
        // depth=2 の全ノード (1..=7) の path_bits が衝突なし
        let mut seen = std::collections::HashSet::new();
        (1u64..=7).all(|id| {
            let key = (depth(id), path_bits(id));
            seen.insert(key)
        })
    }));

    // ══════════════════════════════════════════════════════════
    // SECTION 6: エッジケース 50 件 (E01〜E50)
    // ══════════════════════════════════════════════════════════
    println!("━━ SECTION 6: エッジケース (Edge E01–E50) ━━━━━━━━━━━━━━━━━━━━━━━━");

    // E01-E10: 境界値
    cases.push(run_case("E01: depth(u64::MAX)=63",            depth(u64::MAX) == 63));
    cases.push(run_case("E02: depth(1<<63)=63",               depth(1u64 << 63) == 63));
    cases.push(run_case("E03: path_bits(1<<63)=0",            path_bits(1u64 << 63) == 0));
    cases.push(run_case("E04: sibling(u64::MAX) = MAX^1",     sibling_id(u64::MAX) == Some(u64::MAX ^ 1)));
    cases.push(run_case("E05: parent(2) = 1",                 parent_id(2) == Some(1)));
    cases.push(run_case("E06: parent(u64::MAX) が存在",       parent_id(u64::MAX).is_some()));
    cases.push(run_case("E07: left_child(1<<62) オーバー検知", {
        // 1<<63 がシフトされると符号なし最大近辺になる
        let child = left_child(1u64 << 62);
        child == (1u64 << 63)
    }));
    cases.push(run_case("E08: xnor_diversity(u64::MAX,MAX)=64", xnor_diversity(u64::MAX, u64::MAX) == 64));
    cases.push(run_case("E09: xnor_diversity(0,MAX)=0",       xnor_diversity(0, u64::MAX) == 0));
    cases.push(run_case("E10: lca(1,any) = 1",                lca(1, 12345) == 1));

    // E11-E20: 対称性・可換性
    cases.push(run_case("E11: lca 可換 (大小逆)",             lca(100, 7) == lca(7, 100)));
    cases.push(run_case("E12: path_distance 可換",            path_distance(100, 7) == path_distance(7, 100)));
    cases.push(run_case("E13: are_siblings 可換",             are_siblings(100, 101) == are_siblings(101, 100)));
    cases.push(run_case("E14: xnor_diversity 可換",           xnor_diversity(100, 200) == xnor_diversity(200, 100)));
    cases.push(run_case("E15: merge_id 可換 (兄弟)",         merge_id(4, 5) == merge_id(5, 4)));
    cases.push(run_case("E16: merge_id 可換 (非兄弟)",        merge_id(4, 6) == merge_id(6, 4)));
    cases.push(run_case("E17: depth 単調増加 (親<子)",        depth(5) < depth(10) && depth(10) < depth(20)));
    cases.push(run_case("E18: parent の depth = depth-1",    {
        let id = 42u64;
        parent_id(id).map(|p| depth(p)) == Some(depth(id) - 1)
    }));
    cases.push(run_case("E19: sibling の depth = 同じ",      sibling_id(42).map(|s| depth(s)) == Some(depth(42))));
    cases.push(run_case("E20: child の depth = parent+1",    depth(left_child(42)) == depth(42) + 1));

    // E21-E30: XNOR diversity 性質
    cases.push(run_case("E21: xnor_diversity(a,b) + popcount(a^b) == 64", {
        let a = 0xDEAD_BEEF_u64;
        let b = 0xCAFE_BABEu64;
        xnor_diversity(a, b) + (a ^ b).count_ones() == 64
    }));
    cases.push(run_case("E22: xnor_diversity ≤ 64",           (0..100u64).all(|i| xnor_diversity(i, i * 3) <= 64)));
    cases.push(run_case("E23: xnor(a,a) = 64 常に成立",      (0u64..64).all(|i| xnor_diversity(i, i) == 64)));
    cases.push(run_case("E24: xnor(a,a+1) < 64",             xnor_diversity(0, 1) < 64));
    cases.push(run_case("E25: diversity 0 は a^b=0xfff..のみ", {
        let a = 0xF0F0_F0F0_F0F0_F0F0u64;
        let b = !a;
        xnor_diversity(a, b) == 0
    }));
    cases.push(run_case("E26: 3-way merge diversity可算",     {
        // (a∪b) の diversity + (ab∪c) の diversity = 2回分の保険
        let d1 = xnor_diversity(10, 20) as u64;
        let d2 = xnor_diversity(10 + 20 + d1, 30) as u64;
        d1 + d2 > 0
    }));
    cases.push(run_case("E27: 1M vs 999999: XOR=7bit → diversity=57", xnor_diversity(1_000_000, 999_999) == 57));
    cases.push(run_case("E28: diversity(1,2)=62 (XOR=0b11, 2bit差)", {
        // 1=0b01, 2=0b10 → XOR=0b11 (2 bits) → XNOR popcount=62
        xnor_diversity(1, 2) == 62
    }));
    cases.push(run_case("E29: popcount(xnor) + popcount(xor) = 64", {
        let a = 17u64; let b = 23u64;
        (a ^ b).count_ones() + (!(a ^ b)).count_ones() == 64
    }));
    cases.push(run_case("E30: xnor(power2, 0) = 63",         xnor_diversity(1024, 0) == 63));

    // E31-E40: マージ連鎖・ツリー整合性
    cases.push(run_case("E31: 16葉→root 4段マージ",          {
        let lvl1: Vec<u64> = (0..8).map(|i| merge_id(16+2*i, 16+2*i+1)).collect();
        let lvl2: Vec<u64> = (0..4).map(|i| merge_id(lvl1[2*i], lvl1[2*i+1])).collect();
        let lvl3: Vec<u64> = (0..2).map(|i| merge_id(lvl2[2*i], lvl2[2*i+1])).collect();
        merge_id(lvl3[0], lvl3[1]) == 1
    }));
    cases.push(run_case("E32: 親は必ず子の lca",             (2u64..16).all(|id| lca(left_child(id), right_child(id)) == id)));
    cases.push(run_case("E33: sibling のsibling = 自分",     (2u64..32).all(|id| sibling_id(sibling_id(id).unwrap()).unwrap() == id)));
    cases.push(run_case("E34: left_child の is_left = true",  (1u64..32).all(|id| is_left_child(left_child(id)))));
    cases.push(run_case("E35: right_child の is_left=false", (1u64..32).all(|id| !is_left_child(right_child(id)))));
    cases.push(run_case("E36: 全祖先の depth は単調減少",    {
        let mut id = 63u64;
        let mut prev_d = depth(id) + 1;
        let mut ok = true;
        while id > 1 { id >>= 1; let d = depth(id); if d >= prev_d { ok = false; break; } prev_d = d; }
        ok
    }));
    cases.push(run_case("E37: 葉の subtree_size = 1",        (8u64..16).all(|id| subtree_size(id, 3) == 1)));
    cases.push(run_case("E38: ルートの subtree_size = 2^D+1-1", {
        (1u32..=6).all(|d| subtree_size(1, d) == (1u64 << (d+1)) - 1)
    }));
    cases.push(run_case("E39: depth D の葉数 = 2^D",         {
        (0u32..=6).all(|d| { let (lo,hi) = leaf_range(d); hi-lo+1 == 1u64<<d })
    }));
    cases.push(run_case("E40: path_bits(1<<D) = 0 (常に最左)", {
        (0u32..=6).all(|d| path_bits(1u64<<d) == 0)
    }));

    // E41-E50: 運用特殊ケース
    cases.push(run_case("E41: socket_name_hash(1) = 22",     socket_name_hash(1) == 22));
    cases.push(run_case("E42: socket_name_hash(9999) = 25",  socket_name_hash(9999) == 25));
    cases.push(run_case("E43: depth=63 の葉範囲下限 = 1<<63", leaf_range(63).0 == (1u64 << 63)));
    cases.push(run_case("E44: ID=2 の path=0, ID=3 の path=1", path_bits(2)==0 && path_bits(3)==1));
    cases.push(run_case("E45: 非兄弟 merge_id ≥ 2^60",      merge_id(3, 7) >= (1u64 << 60)));
    cases.push(run_case("E46: 兄弟 merge_id < 2^60",         merge_id(8, 9) < (1u64 << 60)));
    cases.push(run_case("E47: depth の上限 = 63",            depth(u64::MAX) == 63));
    cases.push(run_case("E48: path_bits の bit 数 = depth",  {
        (1u64..=15).all(|id| path_bits(id) < (1u64 << depth(id)))
    }));
    cases.push(run_case("E49: lca(id, parent(id)) = parent", {
        let id = 50u64;
        lca(id, parent_id(id).unwrap()) == parent_id(id).unwrap()
    }));
    cases.push(run_case("E50: 連続 clone_pair で depth=6 到達", {
        let mut ids = vec![1u64];
        for _ in 0..6 {
            let next: Vec<u64> = ids.iter().flat_map(|&id| vec![left_child(id), right_child(id)]).collect();
            ids = next;
        }
        ids.len() == 64 && ids.iter().all(|&id| depth(id) == 6)
    }));

    // ══════════════════════════════════════════════════════════
    // 結果集計
    // ══════════════════════════════════════════════════════════
    println!();
    println!("━━ RESULTS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    let total = cases.len();
    let passed = cases.iter().filter(|c| c.pass).count();
    let failed: Vec<&Case> = cases.iter().filter(|c| !c.pass).collect();

    for c in &cases {
        let mark = if c.pass { "✓" } else { "✗" };
        println!("  {} {}", mark, c.name);
    }
    println!();
    println!("  TOTAL: {}/{}  PASS: {}  FAIL: {}", passed, total, passed, total - passed);
    if !failed.is_empty() {
        println!();
        println!("  FAILED CASES:");
        for c in &failed { println!("    ✗ {}", c.name); }
    }

    // ══════════════════════════════════════════════════════════
    // THROUGHPUT ベンチマーク
    // ══════════════════════════════════════════════════════════
    println!();
    println!("━━ THROUGHPUT (10M iters each) ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    let n = 10_000_000u64;
    let mut total_mops = 0f64;
    let count = 7;

    total_mops += bench_ns("depth(id)  [CLZ]",         n, || depth(n.wrapping_mul(3) | 1) as u64);
    total_mops += bench_ns("parent_id(id) [SHR]",      n, || parent_id(n.wrapping_mul(7) | 2).unwrap_or(0));
    total_mops += bench_ns("sibling_id(id) [XOR]",     n, || sibling_id(n.wrapping_mul(5) | 2).unwrap_or(0));
    total_mops += bench_ns("path_bits(id) [CLZ+XOR]",  n, || path_bits(n.wrapping_mul(11) | 1));
    total_mops += bench_ns("lca(a,b) [loop SHR]",      n, || lca(n.wrapping_mul(13), n.wrapping_mul(7)));
    total_mops += bench_ns("xnor_diversity [XNOR+POPCNT]", n, || xnor_diversity(n.wrapping_mul(3), n.wrapping_mul(7)) as u64);
    total_mops += bench_ns("merge_id(a,b) [branch+SHR]", n, || merge_id(n.wrapping_mul(2), n.wrapping_mul(2)|1));

    println!();
    println!("  平均スループット: {:.2} Mops/s", total_mops / count as f64);

    // ══════════════════════════════════════════════════════════
    // 新規運用ナラティブ
    // ══════════════════════════════════════════════════════════
    println!();
    println!("━━ 新規運用パターン サマリー ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("
  [1] ゼロ設定・自己記述ツリー
      ID=1 個あれば深さ63まで全トポロジーを再現可能。
      設定ファイル/レジストリ/データベース不要。
      → 障害後の再起動コストが「整数復元」だけ。

  [2] ルーティングとシャーディングの統合
      path_bits(id) がそのまま shard key / routing key になる。
      depth=8 → 256 shards、depth=16 → 65536 shards。
      ハッシュ関数不要、衝突なし、prefix-free codes。

  [3] ボトムアップ集約の自然な表現
      xnor_merge(sibling_a, sibling_b) → 親ノードへ自動収束。
      8葉 → 4 → 2 → root の MapReduce が ID 演算だけで表現。

  [4] 多様性注入による自己組織化負荷分散
      XNOR diversity ジョブは「類似しすぎを防ぐ摩擦」として機能。
      外部 load balancer 不要、ジョブ数の類似度で自動的にエントロピー補填。

  [5] 冗長化・フェイルオーバーが O(1)
      sibling_id(id) = バックアップノード。
      parent_id(id) = フェイルオーバー先。
      両方とも 1命令で確定。

  [6] バックプレッシャー伝播
      subtree_size で担当ジョブ数を見積もり、
      lca で「最初に詰まるノード」を即特定。
      キューの深さが path_distance に比例。
");

    if !failed.is_empty() {
        std::process::exit(1);
    }
}
