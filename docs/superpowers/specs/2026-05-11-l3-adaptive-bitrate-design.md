# L3 — Adaptive Bitrate (observed-loss-driven)

**Date:** 2026-05-11
**Phase:** L3 (post-L2 transport robustness)
**Branch (suggested):** `phase-l3-adaptive-bitrate`
**Cross-platform:** Linux + Windows, regression bar = 0
**Estimated LoC:** ~360 across 6 modify + 1 new file (+ 3 new test files)
**Predecessor:** L2 transport robustness (`docs/superpowers/specs/2026-05-10-l2-transport-robustness-design.md`, master `8a6a623`)

---

## 1. Goal & Non-Goals

### Goal
L2 smoke walkthrough (2026-05-10) で残った **17 秒以降 host→viewer の packet delivery が 5.7% に落ちて host watchdog が session を kill する** 問題を解消する。viewer 側で観測した frame loss rate に応じて host encoder の target bitrate を動的に下げ、IDR fragment 数を loss tolerance 内に収める。

### Definition of Done
1. **Linux 実機 smoke**: WSLg host (`--bitrate-mbps 30 --encoder openh264`) + 実機 Wayland viewer で接続 1 分後に `target_bps` が ≤5 Mbps レンジに収束、session が 5 分継続して watchdog kill しないこと
2. **L2 回帰なし**: `--bitrate-mbps 5` smoke は 2.4 秒以内に black→live (adaptive controller が "loss なし" 状態で max_bps を維持し L2 動作を変えない)
3. **CI green**: GitHub Actions Linux + Windows release workflow pass、`cargo clippy --workspace -- -D warnings` 両 target

### Non-Goals (L4 以降)
- IDR fragment 専用 FEC tuning (k/m を IDR に対してだけ大きく)
- ARQ NACK retransmit (lost seq の再送要求)
- Host watchdog の adaptive timeout (現状 5s 固定)
- Bandwidth probe (upper bound 探索) — `--bitrate-mbps` をハードキャップとする
- AV1 / SVC layered encoding
- Multi-decoder switching on quality

---

## 2. Background

### L2 smoke 実測 (2026-05-10, commit `a9bd81d`)
- WSLg host + 実機 Wayland viewer、`--bitrate-mbps 5 --encoder openh264`
- 接続後 ~2.4 秒で black→live ✅ (DoD #1 達成)
- 17 秒以降: host 262 frames send → viewer 15 frames recv = **5.7% delivery**
- 原因: WiFi/LAN の物理層 packet loss + IDR fragment loss が連鎖して回復不能 stretch に入る
- 結果: 5 秒 silence で host watchdog が session を kill

### 既存 transport 構成
- `crates/transport/src/udp.rs:99` — デフォルト `fec_k=64, fec_m=6` (容量 ~75 KB/frame、burst tolerance 6 packets)
- `crates/transport/src/assembler.rs` — `purge(now)` が timeout した seq を `Vec<u64>` で返す (L2 で expose)
- `crates/transport/src/udp.rs` — `purge_assembler() -> Vec<u64>` (L2 で追加、viewer から呼べる)

### 既存 wire (再利用)
- `ControlMessage::SetBitrate { target_bps: u32 }` (kind_u8=6) — 定義済み、handler 無し dead path
- `ControlMessage::Stats { loss_rate_ppm, fps_millis, bitrate_bps }` (kind_u8=7) — 定義済み、未使用 (L3 でも使わない)
- `LatencyReport` (kind_u8=16) — viewer→host 5s 周期で plumbed (loss は未含)

### 既存 encoder set_target_bitrate
- `crates/media-sw/src/encoder.rs:127` — `Openh264Encoder::set_target_bitrate(&mut self, bps: u32)` 実装済み
- `crates/media-win/src/nvenc/encoder.rs` — `NvencEncoder::set_target_bitrate` 実装済み (L1.5b で `Hevc265Encoder` trait 経由で確認)
- `crates/media-win/src/mf/encoder.rs` — `MfH265Encoder::set_target_bitrate` 実装済み

### 抜けている piece
- Host control loop に `SetBitrate` arm 無し (`crates/host/src/lib.rs:650-690`)
- Host video loop に "external bitrate change" を伝える channel 無し
- Viewer に loss measurement + AIMD controller 無し

---

## 3. Architecture

### 3.1 Data flow

```
Viewer side                              Host side
─────────────────                        ─────────────────
[recv loop]                              [control loop]
  ↓ chunks                                 SetBitrate arm
[FrameAssembler]                            ↓ bitrate_tx.send(bps)
  ↓ purge(now) → Vec<seq>                 ────────────────
                                          [video loop]
[latency_task @ 1Hz]                        per-frame:
  ↓ controller.observe(lost, total)         bitrate_rx.try_recv() → drain → latest
  ↓ controller.aimd_step(now)                if Some(bps): producer.set_target_bitrate(bps)
  ↓ controller.should_send() → SetBitrate    next_frame().await
  ↓ transport.send_control(SetBitrate)
   ──────────────────────────────────→   [control loop recv]
```

### 3.2 Module boundaries

| 新規 / 変更 | パス | 責務 |
|---|---|---|
| 新 | `crates/transport/src/bitrate_control.rs` | 純ロジック `BitrateController` (`observe`, `aimd_step`, `should_send`)。tokio 不要、unit-testable |
| 変 | `crates/transport/src/lib.rs` | `pub mod bitrate_control;` 公開 |
| 変 | `crates/viewer/src/lib.rs` (latency_task 内) | 1Hz tick で `purge_assembler()` + controller 駆動 + `SetBitrate` 送信 |
| 変 | `crates/viewer/src/lib.rs` (CLI) | `--no-adaptive-bitrate` flag 追加 |
| 変 | `crates/host/src/lib.rs` (control loop) | `SetBitrate` arm 追加、`bitrate_tx` 経由で video loop へ |
| 変 | `crates/host/src/lib.rs` (video loop) | `bitrate_rx.try_recv()` を `next_frame()` 直前に drain、最新値を `producer.set_target_bitrate` に渡す |
| 変 | `crates/host/src/platform/{win,linux}.rs` | `VideoProducer` trait に `set_target_bitrate(&mut self, bps: u32)` 追加 (各実装が encoder へ pass-through) |

### 3.3 BitrateController API

```rust
pub struct BitrateControllerConfig {
    pub initial_bps: u32,
    pub min_bps: u32,
    pub max_bps: u32,
    pub loss_high: f32,           // 0.02
    pub loss_low: f32,            // 0.005
    pub md_factor: f32,           // 0.7
    pub ai_step_bps: u32,         // 200_000
    pub send_threshold_pct: f32,  // 0.05 (5%)
    pub cooldown_after_md: Duration, // 2s
    pub enabled: bool,            // false => return max_bps always
}

pub struct BitrateController {
    cfg: BitrateControllerConfig,
    target_bps: u32,
    last_md_at: Option<Instant>,
    last_sent_bps: u32,
    rolling_lost: u64,
    rolling_total: u64,
}

impl BitrateController {
    pub fn new(cfg: BitrateControllerConfig) -> Self;

    /// Observe a 1s window's loss/total counts. Caller owns rolling reset.
    pub fn observe(&mut self, lost: u64, total: u64);

    /// Compute next target_bps based on observed loss. Call once per second.
    pub fn aimd_step(&mut self, now: Instant);

    /// Returns target_bps. Caller decides whether to send via should_send().
    pub fn target_bps(&self) -> u32;

    /// Returns true if change exceeds send_threshold_pct since last send.
    pub fn should_send(&self) -> bool;

    /// Mark the current target as sent (updates last_sent_bps).
    pub fn mark_sent(&mut self);

    /// Reset rolling counters at the end of each 1s window.
    pub fn reset_window(&mut self);
}
```

### 3.4 AIMD numerical parameters

| Param | Value | Rationale |
|---|---|---|
| `loss_high` | 0.02 (2%) | FEC k=64 m=6 の tolerance 9.4% の 1/4。早期反応で IDR loss 連鎖前にバックオフ |
| `loss_low` | 0.005 (0.5%) | 完全 quiet 状態だけで AI、ジッター下では hold |
| `md_factor` | 0.7 | TCP NewReno の 0.5 より mild、画質連続性優先。L2 smoke の 5.7% delivery → 0.7^4 = 0.24 で 30→7 Mbps、4 秒で底打ち |
| `ai_step_bps` | 200_000 (200 kbps/s) | 1→30 Mbps recovery に ~2.5 分。loss 再発で即減速されるので safety > speed |
| `cooldown_after_md` | 2s | WiFi block-ack の grace + probe 周期 |
| `min_bps` | 1_000_000 (1 Mbps) | L2 smoke 5 Mbps が動いた事実 + OpenH264 が極端に低 bitrate でも frame を吐く実証あり |
| `max_bps` | `--bitrate-mbps × 1e6` | ユーザー指定 cap |
| `send_threshold_pct` | 0.05 (5%) | hysteresis、control msg spam 抑制 |

### 3.5 Tick cadence
- 1 Hz controller step (既存 `latency_task` の 1s ticker に相乗り — 新 task 無し)
- AIMD step → `should_send()` → 5% 越え時のみ `transport.send_control(SetBitrate)`
- 想定 wire load: ~10 msg/min average、4-byte payload → 無視可

### 3.6 Loss measurement
- 1 Hz tick の冒頭で `transport.purge_assembler().await` を呼び、戻り `Vec<u64>` の `len()` を `lost` に加算
- `total` は `LatencyProbe::snapshot().present.samples` の前回 tick からの差分 + `lost` (= 期待 frame 数)
- ローリング 1s window: `controller.observe(lost, total)` → `controller.aimd_step(now)` → `controller.reset_window()`

### 3.7 Host actuator path

```rust
// session setup
let (bitrate_tx, mut bitrate_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();

// control loop arm (lib.rs:~676)
Ok(ReceivedMessage::Control(ControlMessage::SetBitrate { target_bps })) => {
    info!(target_bps, "viewer requested bitrate change");
    let _ = bitrate_tx.send(target_bps); // unbounded; never blocks
}

// video loop, per-frame, before next_frame().await
let mut latest_bps: Option<u32> = None;
while let Ok(bps) = bitrate_rx.try_recv() {
    latest_bps = Some(bps); // drain to newest
}
if let Some(bps) = latest_bps {
    producer.set_target_bitrate(bps);
}
```

`unbounded_channel` を選んだ理由: capacity 1 だとレースで drop される、bounded sized N だと drain ロジックが複雑。1 Hz × 32-bit のチャネルは memory pressure 無し。

### 3.8 VideoProducer trait extension

`crates/media-core/src/lib.rs` の `VideoProducer` trait に追加:

```rust
pub trait VideoProducer: Send {
    // 既存
    async fn next_frame(&mut self) -> Result<EncodedFrame, MediaError>;
    fn request_idr(&mut self);

    // 新規
    fn set_target_bitrate(&mut self, bps: u32);
}
```

各実装での pass-through:
- `DxgiNvencProducer.encoder.set_target_bitrate(bps)` (Hevc265Encoder trait)
- `DxgiSwProducer.encoder.set_target_bitrate(bps)` (Openh264Encoder)
- `V4l2Producer.encoder.set_target_bitrate(bps)` (Linux media-sw OpenH264 path)

trait method がデフォルト実装 `{}` (no-op) を持つようにすれば既存実装の break なし。ただし production path 全てが overrideする。

---

## 4. Wire & Backward Compatibility

### Wire format (変更なし)
- `ControlMessage::SetBitrate { target_bps: u32 }` をそのまま使用
- protocol_version bump 不要

### 互換性
| 旧/新 | 旧 host | 新 host |
|---|---|---|
| 旧 viewer | 動く (現状) | 動く (host adaptive 受けないだけ) |
| 新 viewer | 動く (viewer 送るが host 黙って捨てる、固定 bitrate に退化) | 動く (フル adaptive) |

破壊的変更なし。

---

## 5. Testing Strategy

### A. 純ロジック (`crates/transport/src/bitrate_control.rs` の `#[cfg(test)] mod tests`) — 8 tests
1. `aimd_md_on_high_loss` — `observe(50, 1000)` (5%) → step → target = max_bps × 0.7
2. `aimd_ai_on_low_loss` — `observe(1, 1000)` (0.1%) → step → target = max_bps + 200_000 (clamped to max)
3. `aimd_hold_in_band` — `observe(15, 1000)` (1.5%) → step → target unchanged
4. `aimd_md_clamps_to_min` — 連続 MD 50 回で target == min_bps
5. `aimd_ai_clamps_to_max` — min_bps から AI で max_bps に clamped
6. `aimd_cooldown_after_md` — MD 直後 1s 内で `observe(0, 1000)` → step → AI されない、3s 後の step で AI される
7. `hysteresis_filters_small_changes` — 4% 変化で `should_send() == false`、6% で true
8. `disabled_controller_returns_max_always` — `enabled=false` で `observe(50, 100)` (50% loss) でも target == max_bps

### B. Viewer→Host 統合 (`crates/transport/tests/adaptive_bitrate_test.rs`) — 2 tests
1. `setbitrate_round_trip` — InProcTransport で SetBitrate を送り、host arm 等価のロジックが target_bps を取り出すこと
2. `loss_burst_drives_md` — `LoopbackOptions::drop_ppm = 50_000` (5%) で 5 秒シミュレーション、controller.target_bps が単調減少を assert

### C. Host arm smoke (`crates/host/tests/setbitrate_handler_smoke.rs`) — 1 test
- mock control channel で SetBitrate を流し、`bitrate_tx.send(bps)` が呼ばれることを assert (mpsc::unbounded_channel の receiver 側で観測)

### D. Regression
- `cargo build --workspace --all-targets` Linux + Windows green
- `cargo clippy --workspace -- -D warnings` 両 target
- 既存 348 + L2 7 + 新 ~11 = **366+ pass**
- `transport::probe_test::two_transports_find_each_other` は L2 同様 pre-existing flaky として除外 (gh issue で記録)

### E. Manual smoke (DoD #1)
- WSLg host: `prdt host --bind 0.0.0.0:9000 --bitrate-mbps 30 --encoder openh264 --silent-allow`
- 実機 Wayland viewer: `prdt connect <host>:9000`
- 観測項目:
  1. 接続 1 分以内に viewer log で `target_bps=N` (N ≤ 5_000_000) が出ること
  2. session が 5 分継続して watchdog kill しないこと
  3. host log で `viewer requested bitrate change target_bps=N` が出ること
  4. 帯域が回復したら target_bps が緩やかに上がること (200kbps/s)

---

## 6. Open Questions for Plan Writer (T0 で解決)

### Q1: VideoProducer trait の現在の shape
`crates/media-core/src/lib.rs` の `VideoProducer` trait に既に `set_target_bitrate` があるか? なければ追加 + 全 3 producer 実装に pass-through を書く。L2 で `Hevc265Encoder::set_target_bitrate` は trait method として確認済みだが、`VideoProducer` (DxgiNvencProducer / DxgiSwProducer / V4l2Producer) のレベルでの抽象化は未確認。

### Q2: Linux V4l2Producer / OpenH264 path の bitrate change cost
`Openh264Encoder::set_target_bitrate(&mut self, bps: u32)` は `media-sw/encoder.rs:127` 実装済みで cheap (re-init 不要、`encoder.set_bitrate(BitRate::from_bps(bps))` を呼ぶだけ) のはず。要確認: change が適用される最初の frame は IDR か P か (NVENC は次 IDR まで反映遅延、OpenH264 は per-frame で即適用と思われる)。

### Q3: bitrate_tx channel placement
HostState struct に置くか、session-local closure capture で済ますか — 既存 `force_idr_flag: Arc<AtomicBool>` は session-local cloning パターン。`bitrate_tx` も同パターンで OK だが、control_loop と video_loop が異なる closure context にあるので clone() が必要 → 既存 pattern と整合する。

### Q4: viewer 側 controller の rolling window 実装
`LatencyProbe::snapshot().present.samples` は cumulative count なので、tick 間の差分を取って rolling 1s total を出す。前回 tick の `last_total_samples` を controller の caller (latency_task) が保持。controller 自体は stateless step (lost/total を毎 tick 渡される) で OK。

### Q5: `--no-adaptive-bitrate` flag の名前
clap derive で boolean flag とする。代替名候補: `--fixed-bitrate` / `--adaptive-bitrate=false` / `--no-abr`。L2 の `--silent-allow` パターンに合わせて `--no-adaptive-bitrate` を使う。

---

## 7. Implementation Task Skeleton

| Task | Files | LoC | TDD |
|---|---|---|---|
| T0 | baseline + Q1〜Q5 解決 | 0 | research only |
| T1 | `bitrate_control.rs` + 8 unit tests | ~150 | yes |
| T2 | viewer latency_task 配線 + `--no-adaptive-bitrate` flag | ~50 | indirect (manual smoke) |
| T3 | host control loop SetBitrate arm + `bitrate_tx` channel + video loop `try_recv` drain → `producer.set_target_bitrate` | ~50 | yes (smoke test) |
| T4 | `VideoProducer::set_target_bitrate` trait method + default no-op + 3 producer pass-through | ~30 | yes (compile + Q2 ack) |
| T5 | `adaptive_bitrate_test.rs` 統合 2 tests + `setbitrate_handler_smoke.rs` | ~80 | yes |
| T6 | Linux smoke walkthrough + STATUS update + tag | ~20 (STATUS only) | manual |

合計: ~360 LoC (新+変更)、TDD 構造、~7 tasks

---

## 8. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Encoder bitrate change が即時反映されず IDR まで待つ場合、loss spike 中は古い bitrate で焼き続ける | OpenH264 は `encoder.set_bitrate()` で per-frame 反映の見込み (Q2 で確認)。NVENC/MF は実装上 `set_target_bitrate` を持つが、反映タイミングは encoder/driver 依存 (Q2 で実機確認)。万一遅延がある場合は `set_target_bitrate` 直後に `request_idr()` も同時に呼んで強制 IDR で適用 (L4 検討) |
| 1 Hz tick 周期が WiFi burst loss (~100ms) より粗い | 1 Hz step + cooldown 2s で十分。frame loss が観測されてから controller が反応するまで最大 1s。session timeout 5s より十分速い |
| `--bitrate-mbps 5` で起動した時、min_bps 1Mbps まで下げ可能 → 過剰 | spec 通り。1Mbps でも OpenH264 は frame 出力可、最悪ケースで session 維持を優先 |
| min_bps すら維持できない catastrophic loss 環境 | adaptive bitrate ではなく ARQ や FEC tuning が必要 → L4 territory、本 spec の対象外 |
| hysteresis が大きすぎて反応遅い | send_threshold_pct=5% は protocol load 抑制目的、controller 内部 target_bps は毎 tick 動く。actual encoder への伝播だけが遅延、AIMD 自体の正確性は保たれる |
| Disabled controller (`--no-adaptive-bitrate`) と enabled の挙動差を実環境で観測する必要 | 本 spec の DoD #2 で 5 Mbps 回帰チェック (controller が "loss なし" 状態で max 維持 = 既存挙動と同じ) |
