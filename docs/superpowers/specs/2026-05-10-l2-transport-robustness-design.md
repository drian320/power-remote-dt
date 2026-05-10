# L2 — Transport Robustness Design

> **Phase:** L2 (post-L1.5b)
> **Predecessor:** L1.5b viewer Linux wiring (PR #1, master `82069e7`, 2026-05-09)
> **Branch:** `phase-l2-transport-robustness`
> **Spec date:** 2026-05-10

## §1. Goal & Definition of Done

### Goal
L1.5b smoke で発生した「ウィンドウは表示されるが中身が真っ暗」問題を、reference frame 喪失時の決定的な回復 path で解消する。Cross-platform (Linux + Windows 両方) の transport layer 改善。

### Background
L1.5b smoke walkthrough (2026-05-09) で WSLg host → 実機 Wayland viewer の cross-machine 接続を検証したところ、handshake / frame 送信 / window 表示は成功するが decode 失敗で画面が黒のままになった。原因:
- WSL2 → LAN UDP (>5 Mbps) で IDR fragment が大量喪失
- Viewer に `RequestIdr` 送信 path が未実装で IDR loss 後の自己回復が無い
- 各 encoder が SPS/PPS を最初の IDR にしか付けない (default OpenH264 strategy + MF/NVENC default)
- 結果、 viewer は最初の IDR が部分喪失すると永久に decode できない

### Definition of Done
1. ✅ 実機 Wayland smoke (host=WSLg / viewer=実機 over LAN) で接続後 ~1 秒以内に black → live に遷移する。`prdt connect` の手動再起動が不要。
2. ✅ Loopback unit test で IDR fragment loss を simulate → assembler の purge から 1 frame 以内に viewer が `RequestIdr` を送る → host encoder が次の encode 呼び出しで fresh IDR を出すことを assert。
3. ✅ Viewer は `RequestIdr` を ≤4/sec (250 ms cooldown) に rate-limit。
4. ✅ 全 IDR access unit (初回 + RequestIdr 起動分) に SPS+PPS NAL units が含まれる。OpenH264 / MF / NVENC 各 encoder で unit test 検証。
5. ✅ Windows regression bar = 0: 既存 Windows CI (rustfmt + clippy + test + build) green。
6. ✅ Linux regression bar = 0: cargo build + clippy が Linux target で green。

### Non-goals (L3+ 送り)
- Forward Error Correction (Reed-Solomon FEC across IDR fragments)
- Adaptive bitrate on observed loss
- Per-fragment FEC for P-frames
- Wayland portal capture / VA-API HW codec
- Multi-monitor capture

---

## §2. Architecture & Components

3 hop の wiring + 各 encoder の SPS/PPS 設定変更。新規 crate / トレイトは作らない。

### 2.1 Viewer 側 — IDR-loss detector
**位置:** `crates/viewer/src/lib.rs` の receive loop (Linux + Windows 両 path)

新規 state (各 PlatformConsumer の隣):
```rust
struct IdrRequester {
    needs_idr_pending: bool,
    last_request_at: Option<Instant>,
}

impl IdrRequester {
    fn new() -> Self {
        Self { needs_idr_pending: false, last_request_at: None }
    }
    fn mark(&mut self) { self.needs_idr_pending = true; }
    fn try_take(&mut self, now: Instant, cooldown: Duration) -> bool {
        if !self.needs_idr_pending { return false; }
        if let Some(t) = self.last_request_at {
            if now.duration_since(t) < cooldown { return false; }
        }
        self.needs_idr_pending = false;
        self.last_request_at = Some(now);
        true
    }
}
```

トリガ 2 系統:
- **Decoder error**: `decoder.decode(...)` が `Err` を返した時 → `requester.mark()`
- **Assembler purge**: 既存 `transport::Assembler::purge_stale()` が purged_count > 0 を返した時 → `requester.mark()`

送信 logic (毎 receive iteration、frame 着 / purge poll の両 path):
```rust
if requester.try_take(Instant::now(), Duration::from_millis(250)) {
    transport.send_control(ControlMessage::RequestIdr).await?;
    tracing::debug!("viewer sent RequestIdr (loss detected)");
}
```

### 2.2 Transport 側 — Assembler purge signal
**位置:** `crates/transport/src/assembler.rs:208`

既存コメント "purged frames triggering IDR requests" を実装する。`purge_stale()` の返り値を `()` から `PurgeReport` に変更:
```rust
pub struct PurgeReport {
    pub purged_count: u32,
}

impl Assembler {
    pub fn purge_stale(&mut self, now: Instant) -> PurgeReport { ... }
}
```

`purged_count` は今回 purge した不完全 frame 数。0 なら viewer 側は何もしない。1+ なら IDR/P-frame の fragment loss が起きた可能性大。

既存 caller の戻り値無視は Rust の linting で warn になるので、 viewer の receive loop 側で必ず assigment + check するようにする。

### 2.3 Host 側 — RequestIdr handler
**位置:** `crates/host/src/lib.rs:626+` (control message match)

既存 KeepAlive / ClipboardText / Bye / LatencyReport の隣に arm 追加:
```rust
Ok(ReceivedMessage::Control(ControlMessage::RequestIdr)) => {
    info!("viewer requested IDR; setting force_idr for next encode");
    force_idr_flag.store(true, Ordering::Release);
}
```

`force_idr_flag: Arc<AtomicBool>` を新規導入。 host の encode loop と control loop で共有 (Arc clone)。 encode loop で frame 投入する直前に:
```rust
let force_idr = force_idr_flag.swap(false, Ordering::AcqRel);
let ef = encoder.encode(&frame, force_idr, ts)?;
```

### 2.4 Encoder 側 — SPS/PPS-with-every-IDR

3 encoder それぞれ設定変更 + unit test:
- **OpenH264** (`crates/media-sw/src/encoder.rs`): `EncSpsPpsIdStrategy = SPS_LISTING` (or equivalent in `openh264-rs` binding) を init 時に set。 binding が直接 expose していない場合は raw FFI 呼び出し → ない場合の fallback として viewer-side で SPS/PPS をキャッシュして decoder に手動 feed する path に切替 (T0 で binding 確認後決定)。
- **MF H.265** (`crates/media-win/src/mf/encoder.rs`): `MFSampleExtension_VideoEncoder_ForceKeyFrame` 経由で SPS/PPS 強制 (T7 で API 詳細決定)。
- **NVENC** (`crates/media-win/src/nvenc/encoder.rs`): `enableRepeatSPSPPS = 1` を `NV_ENC_INITIALIZE_PARAMS` で set。

各 encoder に unit test 追加:
```rust
#[test]
fn second_idr_carries_sps_pps() {
    let mut enc = build_encoder();
    let _ = enc.encode(&frame, true, 0);   // 1st IDR
    let _ = enc.encode(&frame, false, 33); // P
    let ef = enc.encode(&frame, true, 66).unwrap(); // 2nd IDR
    let types = nal_unit_types(&ef.nal_units);
    assert!(types.contains(&7), "missing SPS in 2nd IDR: {types:?}"); // SPS
    assert!(types.contains(&8), "missing PPS in 2nd IDR: {types:?}"); // PPS
    assert!(types.contains(&5), "missing IDR slice in 2nd IDR: {types:?}");
}
```

---

## §3. Data Flow & Failure Cases

### 3.1 正常時 (loss なし)
```
host encode loop:
  encoder.encode(frame, force_idr=false) → P-frame
  → packetize → udp.send_video_packets(...)
viewer recv loop:
  udp.recv_loop → assembler.add_packet → frame complete
  → decoder.decode(nal_units) → I420
  → present_frame
```
`needs_idr_pending` は常に false。`RequestIdr` は流れない。

### 3.2 IDR fragment loss (ケース 1 — 接続後初期 IDR が壊れる)
```
host: 接続直後 IDR 80KB を 60 packet に分解、送信
       packet #15-17 が UDP loss
viewer assembler:
  packet #0..#14, #18..#59 受け取る → 部分集合として保持
  100ms timeout で purge_stale() → 不完全 frame discard、PurgeReport.purged_count=1 返す
viewer recv loop:
  PurgeReport を見て requester.mark()
  rate-limit OK → send_control(RequestIdr)
host control loop:
  RequestIdr 受信 → force_idr_flag.store(true)
host encode loop:
  次の encode 呼び出し直前に force_idr_flag.swap(false)
  → encoder.encode(frame, force_idr=true) → 新しい IDR (SPS+PPS 含む)
viewer:
  完全 IDR 着 → decoder.decode → 成功 → 画面更新開始
```
復旧時間: 250ms (rate-limit floor) + RTT + encoder latency ≈ 280-350ms 想定。

### 3.3 P-frame loss → reference 欠落 (ケース 2)
```
host: IDR (seq=0) 配信成功
host: P-frame (seq=1) UDP loss、丸ごと喪失 (assembler は気付かない)
host: P-frame (seq=2) 着、参照 seq=1 を期待
viewer decoder:
  decode(seq=2) → Err (reference frame missing)
viewer recv loop:
  Err 検知 → requester.mark()
  send_control(RequestIdr)
host: 上と同じ流れで IDR 再送信
```
注: assembler は P-frame の wholesale loss を直接検知しない (個別 packet が来ないだけで purge は trigger しない)。decoder error がメインのトリガになる。 §2.2 の purge は IDR fragment loss 専用。

### 3.4 連続喪失 (rate-limit の効果)
host が IDR を出してから viewer に届くまで RTT 30ms。その間に追加の decode error が連発しても rate-limit (250ms cooldown) のおかげで RequestIdr は 1 つだけ。host encode loop は 1 回だけ余分に IDR を出す。 ネットワークが完全にダウンしている場合、4 req/sec で延々と control を送り続けるが、UDP control は ~16 bytes/req なので無視できる帯域。

### 3.5 想定 failure
- **force_idr_flag が encode loop に届かない**: encode loop と control loop は別 task。`Arc<AtomicBool>` を clone して両方に渡す。 `Acquire`/`Release` ordering で十分 (frame 1 つ遅れても許容)。
- **purge timeout が短すぎて legit frame を捨てる**: 既存 assembler の timeout は固定。 §6 で値を確認 + 必要なら spec で touch。
- **OpenH264 の SPS_LISTING strategy が openh264-rs binding に exposed されてない**: pre-mortem。ない場合は viewer-side で SPS/PPS をキャッシュして decoder に手動 feed する方向に切替。 T0 で binding 確認。

---

## §4. File Structure & Touch List

### 4.1 新規 file

| Path | 目的 | サイズ目安 |
|---|---|---|
| `crates/transport/src/idr_loss_test.rs` | Loopback test: IDR fragment loss → RequestIdr 受信 確認 | ~120 行 |
| `crates/host/tests/request_idr_handler_smoke.rs` | host control handler の RequestIdr arm が force_idr_flag を set することを確認 | ~80 行 |

### 4.2 修正 file

| Path | 変更概要 | 新規行数 |
|---|---|---|
| `crates/transport/src/assembler.rs` | `PurgeReport` struct 追加、`purge_stale()` の戻り値変更 | +30 |
| `crates/viewer/src/lib.rs` | `IdrRequester` struct + receive loop に detector + sender 追加 (Linux + Windows path 両方) | +60 |
| `crates/host/src/lib.rs` | control message match arm に `RequestIdr` 追加、`force_idr_flag: Arc<AtomicBool>` を encode loop と共有 | +25 |
| `crates/media-sw/src/encoder.rs` | OpenH264 init で SPS_LISTING set + 2nd-IDR test 追加 | +20 |
| `crates/media-win/src/mf/encoder.rs` | MF encoder で SPS/PPS-with-every-IDR 設定 + 2nd-IDR test (Windows-only) | +15 |
| `crates/media-win/src/nvenc/encoder.rs` | `enableRepeatSPSPPS = 1` を `NV_ENC_INITIALIZE_PARAMS` に set | +5 |
| `docs/superpowers/STATUS.md` | L2 transport-robustness 完了記録 | +15 |

### 4.3 触らない area
- L1.5b で確定した `crates/viewer/src/platform/` 全体 (mod/win/linux/input_map) — 触る理由なし
- `crates/media-linux/`, `crates/input-linux/`, `crates/input-win/` — トランスポート層の話なので無関係
- `crates/audio/`, `crates/filetransfer/`, `crates/gui-*` — 同上
- `crates/protocol/src/control.rs` の `RequestIdr` 定義 — 既存スキーマそのまま使う

合計 **~250 行追加**。 file は 7 modify + 2 新規。 比較的小型のサブプロジェクト。

---

## §5. Testing Strategy

3 層構成。 各層が独立に何を保証するか明示。

### 5.1 Encoder unit tests (層 1 — encoder 単体)
**目的:** SPS/PPS-with-every-IDR が encoder 設定の変更で実現されていることを保証。

- `crates/media-sw/src/encoder.rs::tests::second_idr_carries_sps_pps` (Linux + Windows 両方走る)
- `crates/media-win/src/mf/encoder.rs::tests::second_idr_carries_sps_pps` (`#[cfg(windows)]`、 D3D11 device 必要なので CI でスキップ可能性あり、`#[ignore]` が必要なら付ける)
- `crates/media-win/src/nvenc/encoder.rs::tests::second_idr_carries_sps_pps` (NVENC GPU 必要、`#[ignore]`)

NAL parse helper は既存の `nal_unit_types(&ef.nal_units)` を再利用。

### 5.2 Transport loopback test (層 2 — wire)
**目的:** Assembler の purge → PurgeReport → viewer-style consumer が RequestIdr を送る完全 round-trip を deterministic に検証。

`crates/transport/src/idr_loss_test.rs` (新規) で `tokio::time::pause()` + `advance` を使って clock を mock し、 IDR fragment 1 つ drop → purge → RequestIdr 送信 → host が control 受信、を全部 in-process で確認。

### 5.3 Host RequestIdr handler smoke (層 2.5 — host 結線)
**目的:** Host の control handler が `force_idr_flag` を確実に set することを確認。

`crates/host/tests/request_idr_handler_smoke.rs` (新規) — ミニマムの Host instance を boot (real transport 不要、mock control channel)、ControlMessage::RequestIdr を送り込み、`force_idr_flag.load(Ordering::Acquire) == true` を assert。

### 5.4 End-to-end smoke (層 3 — 実機)
**目的:** L1.5b smoke の black-screen が解消したことを目視確認。

手順:
1. Master (transport-robustness 完了後) を build → release tag → workflow が Linux binary 配布
2. host: WSLg で `prdt host --bind 0.0.0.0:9000 --encoder openh264 --silent-allow`
3. viewer: 実機 Wayland で `prdt connect --host <WSL_HOST_IP>:9000 --host-pubkey <KEY> --decoder openh264 --codec h264`
4. 確認:
   - ✅ ウィンドウ表示 → 数秒以内に desktop 内容が見える (黒のままにならない)
   - ✅ host のマウス/キーボード操作が viewer 側に反映
   - ✅ host 側の `tracing` log で「viewer requested IDR」のログが出る (loss 状況に応じて)

### 5.5 Regression bar
- Linux: `cargo build -p prdt-client --target x86_64-unknown-linux-gnu` + `cargo clippy --target x86_64-unknown-linux-gnu --all-targets -- -D warnings` 両方 green
- Windows: 既存 CI 全 step green (PR で確認)

---

## §6. Open Questions & Pre-mortem

### 6.1 T0 で要確認 (即解決)
1. `openh264-rs` binding が `EncSpsPpsIdStrategy` を expose しているか? expose 無ければ raw FFI 経由 or fork 回避案として **viewer-side で SPS/PPS をキャッシュして decoder に手動 feed** する方向に切替。
2. 既存 `Assembler::purge_stale()` の timeout 値は何 ms か? それが妥当か (<1 frame interval で誤 purge しないか)。
3. Host の control channel と encode loop は既に別 task か? `Arc<AtomicBool>` 共有のための既存 channel/state があれば再利用。
4. Decoder error 検知が openh264-rs で `Err` ではなく `Ok(None)` になっている可能性。 既存 viewer code で `Ok(None)` も処理されている → そっちもトリガにすべきか? T0 で実際の戻り値挙動確認 (silently skip の場合は seq-gap-based detection 追加が必要)。

### 6.2 Pre-mortem
1. **Reentrant force_idr loop**: viewer が大量 RequestIdr を送る (e.g. permanent network drop) → host encode loop が毎 frame IDR 出して bitrate 爆発。 250ms cooldown で抑制済みだが念のため host 側にも cooldown を入れるか? → **YAGNI、まず 入れない**。 観測してから追加。
2. **MF/NVENC で SPS/PPS-repeat が一発で動かない**: encoder API の知識差。dev host が Windows でない問題は CI に頼る (Windows CI が `#[cfg(windows)]` test を実行するか確認)。
3. **Loopback test の clock 操作**: `tokio::time::pause()` + `advance` が無難。 既存 transport tests でどう扱ってるか T0 で確認。

---

## §7. Implementation Hints (Plan のヒント)

タスク粒度予想 (writing-plans で具体化):
- **T0** — Baseline + 上の open questions 4 つ調査 (verify fail 経路)
- **T1** — `Assembler::PurgeReport` 追加 + 既存 test 更新
- **T2** — Loopback test (`idr_loss_test.rs`) を **failing で write** (TDD)
- **T3** — Viewer の `IdrRequester` 結線 + decoder error / purge トリガ
- **T4** — Host control handler に `RequestIdr` arm + `force_idr_flag` 共有 + handler smoke test
- **T5** — Loopback test pass 確認
- **T6** — OpenH264 SPS/PPS-with-every-IDR config + `second_idr_carries_sps_pps` test
- **T7** — MF encoder の SPS/PPS config + test (`#[cfg(windows)]`)
- **T8** — NVENC encoder の `enableRepeatSPSPPS` + test (`#[cfg(windows)]` + `#[ignore]`)
- **T9** — Linux + Windows 両 CI green 確認 + STATUS 更新 + tag

---

## 改訂履歴
- 2026-05-10 v1: 初版 (7 セクション + open questions + pre-mortem + impl hints)
