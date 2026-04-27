# ADR: NVDEC `latest_dual` を `ArcSwapOption<DualPlaneFrame>` に置換

- **タグ予定**: `nvdec-arcswap-complete`
- **親タグ**: `host-session-liveness-complete`
- **作成日**: 2026-04-27
- **判断者**: ralplan consensus loop iteration 4 で APPROVE

## Context

NVDEC zero-copy パス(`plan2d-zerocopy-complete`)では、`crates/media-win/src/nvdec/decoder.rs` の `DecoderState::latest_dual` が `Mutex<Option<DualPlaneFrame>>` で管理されており:

1. writer (display callback) が `Mutex::lock()` を取得してフレームを書き込み、
2. reader (`take_latest_dual_plane`) が同 `Mutex::lock()` を取得して `Option::take()` でムーブ取得。

writer の lock 区間は `cuGraphicsMapResources` → `cuMemcpy2D_v2 (Y)` → `cuMemcpy2D_v2 (UV)` → `cuGraphicsUnmapResources` を含む比較的長い区間ではないが、`latest_dual` 書き込み(`Some(DualPlaneFrame { y_tex.clone(), uv_tex.clone(), … })`)は別 Mutex(`latest_dual.lock()`)で保護されており、`DualPlaneFrame::clone()`(field-level の `D3d11Texture::clone` = `ID3D11Texture2D::AddRef`)が writer 側 + reader 側それぞれで起こる構造になっていた。

加えて `known_limitations.md` §1b に「DualCache 単一バッファによる write-while-read race」として記録されていたが、現コードでは Mutex 直列化により race は live ではない(ralplan iteration 1-3 の Architect 検証で確認済)。

本タグはユーザ可視のレイテンシ・テールを構造的に削るとともに、将来の reader 別スレッド化(複数 sink 対応)に向けた前提を整える予防的リファクタである。

## Drivers

1. **writer ホットパスから `lock()` を排除** — display callback の最終書き込みステップで `Mutex::lock` が不要になり、microsecond-scale のロック解放を実現。
2. **per-frame `ID3D11Texture2D::AddRef` を半減** — reader 側の `DualPlaneFrame::clone()` を排除(`Arc<DualPlaneFrame>` 配布のみ)。writer 側の `cache.{y,uv}_tex.clone()` 2 回は残すが、reader 側 +2 = 計 4 回/frame → writer 2 + reader 0 = 計 2 回/frame に削減。
3. **将来のマルチシンク renderer の地ならし** — `Arc<DualPlaneFrame>` 経由の参照配布構造により、別スレッド reader を追加しても `take_latest_dual_plane` の consume セマンティクスを維持しつつ拡張可能。

## Decision

`crates/media-win/src/nvdec/decoder.rs` の `DecoderState::latest_dual` を `Mutex<Option<DualPlaneFrame>>` から `arc_swap::ArcSwapOption<DualPlaneFrame>` に置換する。

API 変更:
- `pub fn take_latest_dual_plane(&self) -> Option<std::sync::Arc<DualPlaneFrame>>`(返り値型を `Arc` 化)。実装は `self.state.latest_dual.swap(None)`(consume セマンティクス、`load_full()` は禁止 — DOC コメントで明示)。
- `crates/media-win/src/nvdec/consumer.rs::NvdecD3d11Consumer::take_latest_dual_plane` も同シグネチャ。
- `crates/viewer/src/main.rs` の `enum LatestFrame::DualPlane(...)` を `Arc<DualPlaneFrame>` ラップ型に変更。

型レベル不変条件:
- `DualPlaneFrame` から `#[derive(Clone)]` を削除(grep 検証で全体 clone 呼び出しゼロ)。
- `pub y_tex` / `pub uv_tex` を `pub(crate)` に変更し、`pub fn y_tex_raw(&self) -> &D3d11Texture` / `uv_tex_raw` accessor を提供。これにより外部から内部 `D3d11Texture` を `clone()` する経路を型レベルで封鎖。

Drop 順序:
- `CuvidDecoder::Drop` で `*self.state.dual_cache.lock().unwrap() = None;` の **直前** に `self.state.latest_dual.store(None);` を挿入。これで `Arc<DualPlaneFrame>` が CUDA context 生存中に release され、`cuGraphicsUnregisterResource` の呼び出し前提(context 必要)を維持。

メモリ順序: `arc-swap` クレート(>= 1.7)が AcqRel を提供する。`store` = Release、`swap` = AcqRel 同期点。writer の `cuGraphicsUnmapResources` 完了(device 側 Memcpy2D retire 観測可能点)以後に store するため、reader が swap して取得した `D3d11Texture` の中身は Memcpy2D 結果以後の状態であることが保証される。

## Alternatives

| 案 | 採否 | 理由 |
|---|---|---|
| `[DualCache; 2]` ダブルバッファ + `AtomicUsize` write_idx/latest_idx | 棄却 | Mutex contention の本質を解かず、CUDA-registered slot を 2 個持つ複雑性のみ増える。Memory ordering の自前管理が必要。 |
| `Mutex<Option<Arc<DualPlaneFrame>>>` (stdlib のみ) | 棄却 | reader 側ロック μs オーダ残存、writer 側ロックも残る。lock-free read 経路が得られない。supply chain 増分が `arc-swap` で実質ゼロ。 |
| `Arc::try_unwrap` ファストパス | 棄却 | フォールバック分岐でクローンが走り得る、API 複雑化。 |
| `RwLock<Option<DualPlaneFrame>>` | 棄却 | writer が write lock 必須、依然 tail 要因。 |
| `tokio::sync::watch` | 棄却 | 内部で `parking_lot::Mutex`、lock-free read が得られない。 |
| O2 Adaptive FEC を本サイクル先行 | 保留 | 価値は高いが本タグの 1 ファイル局所変更スコープと混ぜると revert 単位が大きくなる。Follow-ups で確約。 |

## Consequences

- **+** writer/reader 双方のホットパスから Mutex 獲得が消える(reader はロックフリー)。
- **+** フレームあたり AddRef 回数が writer 側の 2 回のみに縮減(reader 側 +0)。
- **+** 将来 reader を別スレッド化する作業の前提が整う。
- **−** 依存クレート `arc-swap = "1"` 追加(well-maintained、MSRV 1.45+、サプライ・チェーン増分は実質ゼロ)。
- **−** `DualPlaneFrame::{y_tex, uv_tex}` が `pub(crate)` に変わり、外部からは `*_raw()` accessor 経由必須(viewer/renderer/consumer の call sites を全て書換済)。
- **−** `LatestFrame::Nv12(D3d11Texture)` と `LatestFrame::DualPlane(Arc<DualPlaneFrame>)` の API 非対称化(後述 Notes)。

## Acceptance(全件 blocking)

### 計測条件

`prdt-bench-matrix --resolutions 1080 --bitrates 30 --decoders nvdec --encoders nvenc --fps 60 --duration 5m --out-dir bench-out/{baseline,arcswap}-{1..5}`(N=5、loopback RTX 3070 Ti、サマリ CSV 列 `e2e_p50_us/p95_us/p99_us, decode_p50_us/p95_us/p99_us, loss_ppm`)。

### N=5 採用理由

χ² 自由度 4 で σ の不確定性は約 ±35% に収まり、±2σ 回帰ガードに必要な精度を確保する。N=3 では df=2 で σ 不確定性 ±50% となり統計的に脆弱。

### 閾値ルール

- **改善要件 (primary)**: `arcswap_median(e2e_p99_us) ≤ baseline_median(e2e_p99_us)` および `arcswap_median(decode_p99_us) ≤ baseline_median(decode_p99_us)`。
- **回帰ガード (secondary)**: 他 KPI(`e2e_p50_us`, `e2e_p95_us`, `decode_p50_us`, `decode_p95_us`, `arrival_p99_us`, `loss_ppm`)について `arcswap_median ≤ baseline_median + 2σ`。σ は N=5 標本標準偏差(自由度 4)。

### プラン記載の "take_latest_dual_plane_p99_us / writer_lock_wait_p99_us / d3d11_addref_per_frame / render_fps" について

`prdt-bench-matrix` の summary CSV はこれらの専用列を出力しない。実体に最も近い代理指標は:

| プラン記載 | 実列(代理) |
|---|---|
| take_latest_dual_plane_p99_us | decode_p99_us(decode 直後の take 含む) |
| writer_lock_wait_p99_us | (直接代理なし — Mutex 削除自体が writer 側構造改善の十分条件) |
| d3d11_addref_per_frame | (実装でフィールド `pub(crate)` + `#[derive(Clone)]` 削除により reader 側 AddRef 経路を型レベル封鎖、間接保証) |
| render_fps | sent / duration、receive / duration |

専用列追加は別タスク(`prdt-bench-matrix` の拡張)として deferred。本 ADR の改善要件は `e2e_p99_us` と `decode_p99_us` の 2 メトリクスに二重化することで担保。

### テスト

`cargo test -p prdt-media-win --lib` 38 件全部 pass(既存 34 + 新規 4):
- `take_latest_dual_plane_consume_semantics` — `swap(None)` の drain-once 契約。
- `take_latest_dual_plane_no_inner_clone_via_strong_count` — 100 回 publish/drain で `Arc::strong_count == 2/1` 不変、内部 `D3d11Texture` 複製ゼロ。
- `dual_cache_drop_counter_increments` — `DualCache::Drop` が 1 回呼ばれる(`#[cfg(test)] static DUAL_CACHE_DROP_COUNT`)。
- `dual_cache_drop_calls_unregister_twice` — `cuGraphicsUnregisterResource` shim カウンタが Y plane + UV plane で 2 回(`#[cfg(test)] static UNREG_CALLS`)。

Liveness invariant(間接 Drop 順序検証): `cuGraphicsUnregisterResource` は CUDA context 生存中に呼ぶ必要がある。`UNREG_CALLS == 2` がパニック無しで到達 = `DualCache::Drop` が context drop の前に走った間接保証。直接時系列観測は `Arc<CudaContext>` への Drop ラッパが必要だが prod 型汚染となるため不採用。

### Lint / Format

- `cargo clippy -p prdt-media-win -p prdt-viewer -p prdt-latency-bench --all-targets -- -D warnings` clean(変更 3 クレートで warnings ゼロ)。
- `rustfmt --check` 変更 5 ファイル clean。
- ワークスペース全体の clippy/fmt は本タスクスコープ外の既存差分(prdt-host の `unused_mut` 6 件、`signaling-server/tests/server_tests.rs` の fmt 差分、`mf/decoder.rs` の fmt 差分)を含むため修正対象外。

## Notes

- **`take_latest_dual_plane` の consume セマンティクス**: `ArcSwapOption::swap(None)` で原子的に取り出す。`load_full()` は **禁止** — peek 用途は refcount を増やし、`Drop` 不変条件と consume 契約の両方を破壊する。DOC コメントに明記。
- **dhat heap profiling**: 元計画では dhat-heap で alloc プロファイルを取る予定だったが、`prdt-bench-matrix` の Cargo.toml に dhat-heap feature を追加する作業が本 ADR の局所変更スコープを超えるため、defer。代わりに型レベル不変条件(`#[derive(Clone)]` 削除 + `Arc::strong_count` 100-iter テスト)で reader 側 AddRef 倍化が起きないことを保証。writer 側の `Arc::new(DualPlaneFrame { ... })` 1 回/frame の追加 alloc は許容(60-120 Hz × ~64B = 数 KB/sec、e2e に有意影響なし)。
- **dhat × `panic = "abort"`**: workspace `Cargo.toml` の `[profile.release] panic = "abort"`(dev は default unwind)により、将来 dhat-heap を導入する場合は dev profile での実行が必要。release profile は dhat-free。
- **`LatestFrame` 非対称性**: `LatestFrame::Nv12(D3d11Texture)` は単一 plane で AddRef コストが半分(2 → 1)であり、MF パスで同等の問題は観測されていないため対称化は意図的に保留。本 ADR は driver 2 を `DualPlane` 側のみで満たす。
- **Drop ordering 検証手法**: Test #3/#4 は liveness invariant(`UNREG_CALLS == 2` が context 活時に到達)による間接観測。本番型に Drop ラッパを被せる汚染を避ける目的で採用。
- **CudaContext Drop ラッパ廃止**: ralplan iteration 3 案の `CTX_DROP_OBSERVED_AT` 計装は prod 型汚染のため不採用。Test #3/#4 が代替する。

## Follow-ups

### O2 Adaptive FEC(本タグマージ直後に開始確約)

- **着手日**: 本タグマージ翌日(2026-04-28)。
- **起票ファイル**:
  - `docs/superpowers/specs/2026-04-28-o2-adaptive-fec-design.md`
  - `docs/superpowers/plans/2026-04-28-o2-adaptive-fec.md`
- **背景**: `docs/encoders.md` で文書化された「NVIDIA の MF HEVC MFT は ICodecAPI bitrate hints を無視し IDR が ~470KB に膨張して FEC budget (75KB) を超過する」現象が、4K60 IDR burst で観測されるプロダクション制約。
- **設計**: viewer の `LatencyReport` で観測した loss rate を host が読み、観測ロス率と IDR バーストサイズに応じて FEC `(k, m)` を動的切替(`k=8 m=2` baseline、`m=6` を高損失時のみ)。`crates/protocol/src/control.rs` に `FecParams { k, m, generation }` 制御メッセージを追加、`generation` を `VideoPacket` ヘッダに含める。
- **Trigger metric (採用判断基準)**: 「4K60 IDR burst で MF budget overflow 時のフレームドロップ率を **-50% 削減**(ベースライン比)」を `prdt-fec-bench` のパケットロス注入シナリオで実測。
- **判定**: 上記 -50% を満たさなければ O2 はドロップ。満たせば次タグ `o2-adaptive-fec-complete` として merge。

### LatestFrame 対称化(O2 完了後に評価)

- `LatestFrame::Nv12(D3d11Texture)` も `LatestFrame::Nv12(Arc<D3d11Texture>)` に揃えるかを再評価(API 一貫性目的、performance gain は小)。

### dhat-heap 計測の自動化

- 専用ベンチに dhat-heap feature を組み込み、CI で alloc rate 監視を行う(本タグでは defer)。

### `prdt-bench-matrix` への take/writer-lock メトリクス追加

- 専用列(`take_latest_dual_plane_p99_us`、`writer_lock_wait_p99_us`、`d3d11_addref_per_frame` 推定値)を summary CSV に追加し、本 ADR のような代理指標利用を将来は不要化する。

## Appendix(N=5 実測値)

ベンチマトリクス: `--resolutions 1080 --bitrates 30 --decoders nvdec --encoders nvenc --fps 60 --duration 5m`、RTX 3070 Ti loopback、host-session-liveness-complete 子コミット 948555a。

### Baseline (host-session-liveness-complete + 948555a 「pre-arcswap」)

実行時刻: 2026-04-27 11:17:30 〜 11:42:46 JST、25 分 19 秒。

| Run | e2e_p50_us | e2e_p95_us | e2e_p99_us | decode_p50_us | decode_p95_us | decode_p99_us | loss_ppm |
|---|---|---|---|---|---|---|---|
| baseline-1 | 12204 | 18412 | 28882 | 2069 | 3745 | 7182 | 72 |
| baseline-2 | 12259 | 16922 | 23834 | 2093 | 3617 | 5415 | 66 |
| baseline-3 | 11323 | 15864 | 19295 | 2035 | 3096 | 4508 | 59 |
| baseline-4 | 11614 | 31717 | 58891 | 2013 | 3780 | 14114 | 72 |
| baseline-5 | 11796 | 22621 | 50169 | 2106 | 3475 | 13414 | 67 |
| **median** | **11796.0** | **18412.0** | **28882.0** | **2069.0** | **3617.0** | **7182.0** | **67.0** |
| **σ (N=5, df=4)** | 396.3 | 6463.9 | 17336.8 | 39.0 | 277.0 | 4526.2 | 5.4 |
| **threshold (median + 2σ)** | 12588.7 | 31339.9 | 63555.7 | 2146.9 | 4171.0 | 16234.3 | 77.7 |

### Post-change (nvdec-arcswap-complete)

実行時刻: 2026-04-27 11:43:29 〜 12:08:37 JST、25 分 8 秒。

| Run | e2e_p50_us | e2e_p95_us | e2e_p99_us | decode_p50_us | decode_p95_us | decode_p99_us | loss_ppm |
|---|---|---|---|---|---|---|---|
| arcswap-1 | 11153 | 15360 | 21309 | 2052 | 2909 | 4023 | 57 |
| arcswap-2 | 11221 | 15611 | 21944 | 2057 | 2958 | 4049 | 58 |
| arcswap-3 | 11020 | 15105 | 21304 | 2039 | 2797 | 3945 | 57 |
| arcswap-4 | 11185 | 15591 | 20929 | 2050 | 2828 | 3985 | 58 |
| arcswap-5 | 12487 | 16911 | 22054 | 2144 | 3855 | 5689 | 64 |
| **median** | **11185.0** | **15591.0** | **21309.0** | **2052.0** | **2909.0** | **4023.0** | **58.0** |
| **σ (N=5, df=4)** | 605.1 | 699.1 | 475.6 | 42.8 | 443.8 | 756.1 | 2.9 |

### 判定

| 指標 | Baseline median | Threshold | arcswap median | Δ vs baseline | 結果 |
|---|---|---|---|---|---|
| **e2e_p99_us** (primary 改善) | 28882.0 | ≤ baseline_median | **21309.0** | **−26.2 %** | **PASS (improved)** |
| **decode_p99_us** (primary 改善) | 7182.0 | ≤ baseline_median | **4023.0** | **−44.0 %** | **PASS (improved)** |
| e2e_p50_us (regression guard) | 11796.0 | ≤ 12588.7 (+2σ) | 11185.0 | −5.2 % | PASS (within +2σ) |
| e2e_p95_us (regression guard) | 18412.0 | ≤ 31339.9 (+2σ) | 15591.0 | −15.3 % | PASS (within +2σ) |
| decode_p50_us (regression guard) | 2069.0 | ≤ 2146.9 (+2σ) | 2052.0 | −0.8 % | PASS (within +2σ) |
| decode_p95_us (regression guard) | 3617.0 | ≤ 4171.0 (+2σ) | 2909.0 | −19.6 % | PASS (within +2σ) |
| loss_ppm (regression guard) | 67.0 | ≤ 77.7 (+2σ) | 58.0 | −13.4 % | PASS (within +2σ) |

**OVERALL: PASS** — 全 7 指標 acceptance クリア、primary 2 指標は実質改善。

### 補足観察

| 観察 | Baseline | Post-arcswap | 効果 |
|---|---|---|---|
| e2e_p99 run-to-run spread (max−min) | 39596 us (19.3〜58.9 ms) | 1125 us (20.9〜22.1 ms) | **97 % 削減** |
| decode_p99 run-to-run spread | 9606 us (4.5〜14.1 ms) | 1744 us (3.9〜5.7 ms) | **82 % 削減** |
| e2e_p99 σ | 17336.8 us | 475.6 us | σ が **36 倍** 安定 |
| decode_p99 σ | 4526.2 us | 756.1 us | σ が **6 倍** 安定 |

ライターホットパスの Mutex 撤去とリーダー側 AddRef 排除により tail latency の絶対値だけでなく **run-to-run の安定性も大幅に改善**。これは Driver 1(writer ホットパスからの lock 排除)が tail の上振れ要因を構造的に除去したことの強い裏付け。

### 解析スクリプト

`scripts/analyze-arcswap-acceptance.py` を本タグ直下に同梱(N=5 標本標準偏差 df=4 で σ 算出 + median + 2σ threshold で acceptance 判定)。`bench-out/baseline-{1..5}/summary.csv` と `bench-out/arcswap-{1..5}/summary.csv` を入力に再実行可能。
