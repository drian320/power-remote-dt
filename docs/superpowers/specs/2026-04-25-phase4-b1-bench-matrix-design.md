# Plan 4 B1 Bench Matrix — Design Spec

**Date:** 2026-04-25
**Tag (on completion):** `plan4-b1-bench-matrix-complete`
**Scope:** B1 (resolution) × B2 (bitrate) × B5 (decoder) × fps の 4 軸 sweep

## Goal

`power-remote-dt` の loopback bench を 4 軸 60 構成のマトリクスで一気に流し、各構成の per-frame raw + 構成間比較 summary を CSV 出力する `prdt-bench-matrix` bin を追加する。Machine A(RTX 3070 Ti)単体で実行し、約 15–20 分で全構成を収録する。

## Non-goals

- B3 (AV1 コーデック) — NVENC AV1 サポート未実装
- B4 (LAN / loopback / TURN 経路比較) — 2 台 LAN 自動化が要、別 spec
- B6 (FEC k=8/32/64 sweep) — transport ベンチ別 spec
- B7 (input round-trip) — 2 台 LAN 必要、別 spec
- B8 (30 分長時間安定性) — host/viewer 本体に bench mode 必要、別 spec
- ヒートマップ画像生成 — pandas/matplotlib で外部処理
- M3 真の glass-to-glass(カメラ実測) — 別 plan
- マトリクス間 regression 比較(前回比) — 後続拡張可

## Architecture

`crates/latency-bench` を **crate-binary から lib + 2 bins** に再編する。既存 `prdt-latency-bench` は単発構成 bin として残し、新 `prdt-bench-matrix` は同 crate 内に追加。

```
crates/latency-bench/
  Cargo.toml          ← [[bin]] を 2 個追加宣言、lib も追加
  src/
    lib.rs            ← 新規 (pub mod full_pipeline, pub fn percentiles, 共有型 export)
    main.rs           ← 既存 (--mode in-process と --mode full-pipeline-win の単発 bin)
    full_pipeline.rs  ← 既存。run() を分割し pub fn run_for_matrix(cfg) -> RunStats を切り出す。
                        既存 run() は薄いラッパ(run_for_matrix を呼んで CSV 書き出し)
    bin/
      bench-matrix.rs ← 新規 prdt-bench-matrix bin
docs/
  bench-matrix.md     ← 新規 usage + サンプル CSV 解釈ガイド
```

### 共有型(`lib.rs`)

```rust
pub use full_pipeline::{ConsumerBackend, FullPipelineConfig, StageTimes};

/// 1 構成の bench 実行結果。frames は per-frame raw、stats は集計後。
pub struct RunStats {
    pub sent: u64,
    pub received: u64,
    pub frames: Vec<StageTimes>,
}

/// 1 構成 1 行の summary 行データ。
pub struct ConfigStats {
    pub config_id: String,           // "1080p60-30mbps-mf" 等
    pub resolution: (u32, u32),
    pub bitrate_mbps: u32,
    pub decoder: ConsumerBackend,
    pub fps: u32,
    pub sent: u64,
    pub received: u64,
    pub loss_ppm: u64,
    pub arrival_p50_us: u64, pub arrival_p95_us: u64, pub arrival_p99_us: u64,
    pub decode_p50_us: u64,  pub decode_p95_us: u64,  pub decode_p99_us: u64,
    pub e2e_p50_us: u64,     pub e2e_p95_us: u64,     pub e2e_p99_us: u64,
}

/// 集計関数。frames が空なら全 0 + loss_ppm = 1_000_000(skip 用)。
pub fn aggregate(cfg: &FullPipelineConfig, run: &RunStats) -> ConfigStats { ... }

/// マトリクス展開。CLI で渡された軸の直積を取って FullPipelineConfig 群を作る。
pub fn expand_matrix(axes: &MatrixAxes) -> Vec<FullPipelineConfig> { ... }

pub struct MatrixAxes {
    pub resolutions: Vec<(u32, u32)>,
    pub bitrates_mbps: Vec<u32>,
    pub decoders: Vec<ConsumerBackend>,
    pub fps: Vec<u32>,
    pub duration: std::time::Duration,
}
```

### `full_pipeline::run_for_matrix`

既存 `run(cfg)` から CSV 書き出しを除いた core を切り出す:

```rust
pub async fn run_for_matrix(cfg: &FullPipelineConfig) -> anyhow::Result<RunStats>;
```

呼出側(matrix bin)が CSV 書き出しを管理する。既存 `run(cfg)` は `run_for_matrix(cfg).await? + write_csv` の薄いラッパに変える(後方互換性維持)。

## CLI(`prdt-bench-matrix`)

```
--out-dir <path>             # required: summary.csv + per-frame/ をそこに書く
--resolutions 1080,1440,2160 # heights、16:9 で WxH 自動展開 (例: 1080 → 1920x1080)
--bitrates 5,10,20,30,50     # Mbps
--decoders mf,nvdec
--fps 60,120
--duration 10s               # 各構成の収録時間
--dry-run                    # マトリクス展開だけ stdout に書く、bench 実行せず
```

すべてカンマ区切り。default は上記の値そのまま(60 構成)。

## データフロー

1. CLI parse → `MatrixAxes` 構築 → `expand_matrix()` → `Vec<FullPipelineConfig>` (60 構成)
2. `--dry-run` なら構成 ID 一覧を stdout に書いて exit
3. `--out-dir` を作成、`per-frame/` サブディレクトリも作成
4. for each config (sequential):
   - `tracing::info!("[{i}/{n}] running {config_id}")`
   - `full_pipeline::run_for_matrix(cfg).await` を呼ぶ
   - 成功なら `write_per_frame_csv(out_dir/per-frame/<id>.csv, &frames)` を即書き(途中 fail で部分結果残す)
   - 失敗なら警告ログ + `RunStats { sent: 0, received: 0, frames: vec![] }` で続行
   - `aggregate(cfg, &run)` で `ConfigStats` を作って `Vec<ConfigStats>` に push
5. 全構成終了後 `write_summary_csv(out_dir/summary.csv, &all_stats)` を書く

### 構成 ID

`{height}p{fps}-{bitrate}mbps-{decoder}` で安定 stringify(ASCII のみ、ファイルシステム safe):

- `1080p60-5mbps-mf`
- `2160p120-50mbps-nvdec`

per-frame ファイル名にも summary.csv の `config_id` 列にも同じ string を使う。

### per-frame raw CSV(`per-frame/<config_id>.csv`)

ヘッダ:
```
seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us
```

各 lag は `*_us - capture_us` から計算済の値を書く(後の分析で再計算する手間を省く)。`StageTimes` に `present_us` フィールドはあるが loopback bench は画面提示しないので **decode_done_us を e2e の終端**として扱い、`e2e_lag_us = decode_done_us - capture_us` とする。M3 でカメラ実装後に真の present_us へ移行可。

### summary.csv

ヘッダ:
```
config_id,resolution,bitrate_mbps,decoder,fps,sent,received,loss_ppm,arrival_p50_us,arrival_p95_us,arrival_p99_us,decode_p50_us,decode_p95_us,decode_p99_us,e2e_p50_us,e2e_p95_us,e2e_p99_us
```

`resolution` は `1920x1080` 形式の文字列。`decoder` は `mf` か `nvdec`。

## エラーハンドリング

- **NVENC init 失敗**(例: 4K@120fps が unsupported): その構成だけ skip、`summary.csv` に `loss_ppm=1000000`、全 percentile = 0、log で `ERROR config X failed: ...`
- **decoder context 失敗**(NVDEC init 含む): 同上
- **bench 中 panic**: 既存 panic_hook(G5)で crash dump、bin 自体は落ちる。per-frame/ に partial 結果残る。再実行時は CLI で残り構成だけ取れない(全部やり直し)— 不便だが YAGNI、必要なら resume 機能を後付け
- **--out-dir が既存**: 上書き許可(既存 summary.csv / per-frame/ ファイルを上書きする)。確認プロンプトなし(自動化前提)

## 進捗 logging

```
[ 1/60] running 1080p60-5mbps-mf      duration=10s
[ 1/60] done    1080p60-5mbps-mf      received=600/600 e2e_p95=18ms
[ 2/60] running 1080p60-10mbps-mf     duration=10s
[ 2/60] done    1080p60-10mbps-mf     received=600/600 e2e_p95=19ms
...
[60/60] done    2160p120-50mbps-nvdec received=1198/1200 e2e_p95=24ms

summary written to bench-results/2026-04-25/summary.csv (60 rows, 0 skipped)
```

## テスト戦略

NVENC / decoder の動作は既存 `full_pipeline::run` のテストでカバー済み。新規分は **CLI + 集計ロジックのみ** unit test:

1. **`expand_matrix_produces_cartesian_product`** — `axes={r=[1080,1440], b=[10,30], d=[mf], fps=[60]}` → 4 構成、構成内容と順序を assert
2. **`config_id_format`** — `(1920, 1080), 30, ConsumerBackend::Mf, 60` → `"1080p60-30mbps-mf"` を assert
3. **`aggregate_empty_run_emits_skip_row`** — `RunStats { sent: 0, received: 0, frames: vec![] }` → `loss_ppm = 1_000_000`、全 percentile = 0
4. **`aggregate_full_run_computes_percentiles`** — 既知の 100 frame サンプルで p50/p95/p99 を assert(既存 `percentiles()` の sanity check 兼ねる)
5. **`summary_csv_writer_emits_header_and_rows`** — `tempfile::tempdir` に書いて行数 + ヘッダ文字列 + 1 行内容を確認

実機 NVENC/NVDEC 実行はテストではなく **manual smoke**(後述 Exit criteria #4):

```bash
prdt-bench-matrix --out-dir bench-results/2026-04-25/ --duration 5s
```

## Exit criteria

1. `cargo build --release -p prdt-latency-bench --bin prdt-bench-matrix` 通る
2. `cargo test -p prdt-latency-bench` — 5 新 unit test + 既存 test 全 pass(全体 workspace ≥ 282 を期待: 277 + 5)
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
4. **実機 1 周実行**: Machine A で `prdt-bench-matrix --out-dir bench-results/<date>/` を流して、`summary.csv`(60 行 expect、skip があれば 60 行のうち何行 skip かをログ集計)+ `per-frame/<id>.csv`(成功した構成数分)が生成される
5. `docs/bench-matrix.md` に使い方サンプル + サンプル CSV 行の解釈例 1 つ
6. tag `plan4-b1-bench-matrix-complete` 作成

## Risks & Notes

- **NVENC HEVC 4K@120fps の上限**: RTX 3070 Ti(Ampere)は仕様上通る想定だが、実測で encode_done_us が大きく跳ねる可能性。問題視せず数値として記録(後で別構成と比較可能)
- **構成切替コスト**: NVENC encoder + decoder の context は構成ごとに作り直す。1 構成あたり ~100ms の init 時間が duration に乗らない(計測対象外)
- **MF decoder の単一 GPU loopback での 3fps 問題**(known_limitations): MF 構成で `received < sent` が大きく出る可能性。skip ではなく数値として記録、loss_ppm が大きい行として残す
- **構成順序**: 解像度 outer、ビットレート → デコーダ → fps の順で sweep。CSV 行順は予測可能、ヒートマップ作成時に行番号 → セルマップしやすい
- **CSV ヘッダ stability**: `config_id` を最左列にして必ず安定。ヘッダ列順は将来の拡張で末尾追加のみ許容(中間挿入禁止)

## Estimate

- spec: 0.5d (this session)
- plan: 0.5d
- 実装 + 検証: 1d
- 合計 ~2d
