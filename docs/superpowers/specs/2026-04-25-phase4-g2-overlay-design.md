# Phase 4 G2 — Viewer In-Stream Overlay (B1 分離プロセス) Design

**Project**: power-remote-dt
**Phase**: 4 (GUI + 配布)、サブプラン G2
**Date**: 2026-04-25
**Status**: Draft (built on `phase4-g6-complete` master)
**Parent spec**: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`

---

## Summary

`prdt-viewer` 接続中に ESC キーで「オーバーレイ」をトグル表示する。オーバーレイは別プロセス(`prdt-viewer-overlay` バイナリ、eframe 製)として spawn され、レイテンシ snapshot などのライブ統計を 5 Hz でファイル経由 IPC から読んで表示する。Resume / Disconnect ボタンで操作可能。デスクトップ 3 OS(Windows / Linux / macOS)で同一実装で動く設計。**モバイル(iOS/Android)は Phase 5+ で viewer 丸ごと再実装する際に組み込む** — G2 のコード自体は再利用しない前提だが、UX パターン・i18n 文字列・レイアウト判断は Phase 5+ で参照される。

到達目標: G2 終了時点で、Windows でリモートデスクトップを使用中に ESC を押すとオーバーレイが開き、p50/p95/p99 ms とサンプル数、現在のデコーダ、Disconnect ボタンが見える。Disconnect で接続が終了する。

---

## Scope

### In-scope (G2)

- 新クレート `crates/viewer-overlay/`(eframe バイナリ、`prdt-gui-common` 再利用)
- 新モジュール `crates/viewer/src/overlay_ipc.rs`(ファイル IPC、stats writer、control flag polling)
- 既存 `crates/viewer/src/main.rs`:
  - ESC キー検出 → overlay 子プロセス spawn(既に live なら何もしない)
  - 1 Hz で stats JSON を IPC ディレクトリに書き出し
  - 1 Hz で control flag を polling、Disconnect 検出時に Bye 送信 + 終了
  - 終了時に IPC ファイルをクリーンアップ
- IPC ファイル仕様:
  - 場所: `dirs::cache_dir() / "prdt" / "overlay-ipc" / <pid>/`(全 OS で適切な temp 相当パス)
  - `stats.json`(viewer 書き、overlay 読み):latency / samples / decoder / fps_observed / connection_state
  - `control.json`(overlay 書き、viewer 読み):`{ "action": "disconnect" }` 等
- overlay GUI(`prdt-viewer-overlay`):
  - ヘッダ:接続先表示(host_id か addr)、現在の decoder
  - ライブ統計:latency p50/p95/p99 ms、samples 数、接続状態
  - ボタン:`Resume`(ウィンドウ閉じる)、`Disconnect`(control flag 書き出し → 自分も終了)
  - 5 Hz で stats.json を polling
- `--headless` モード viewer は overlay を spawn しない(スクリプト/CI で邪魔にならないよう)
- i18n: 既存 `prdt-gui-common` に新 ID 追加(`overlay-window-title`、`overlay-button-resume`、`overlay-button-disconnect`、`overlay-stats-latency`、`overlay-stats-samples`、`overlay-stats-decoder`、`overlay-stats-connecting`)
- テスト:
  - `overlay_ipc::write_stats / read_stats` round-trip
  - control flag write/poll round-trip
  - PID-based ディレクトリ隔離(2 つの viewer が同時に動いても crosstalk 無し)

### Out (G3+ 以降 / 別 Phase)

- 真 inline overlay(D3D11 swapchain への egui 描画合成)→ Phase 5+ で viewer 丸ごと egui-wgpu 化したときに実装
- 音量スライダ → Phase 3b audio 側に volume API がまだ無い、追加実装含めて G3+ で
- フルスクリーン切替ボタン → 既存 winit 機能を呼ぶだけだが G2 では入れない
- iOS / Android(parent spec で Phase 5+、別プロセス自体不可なため再アーキ必要)
- Linux / macOS(viewer 自体が Windows-only、Phase 1 Linux 対応時に overlay も該 OS で動作確認)
- ESC 以外のホットキー(F1 メニュー等)
- overlay ウィンドウ位置の保存(常にデフォルト位置、G3+)
- overlay の半透明化や常時最前面(OS 依存、G3+)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| プロセスモデル | 別プロセス spawn(B1) | macOS main-thread 制約・winit/eframe イベントループ衝突を完全回避、Win+Linux+macOS で同一実装 |
| IPC 方式 | ファイルベース JSON(stats 1Hz / control polling 1Hz) | 全 OS 動作、依存ライブラリゼロ、デバッグ容易 |
| IPC ディレクトリ | `dirs::cache_dir().join("prdt").join("overlay-ipc").join(<pid>)` | OS ごとの慣例パス、PID 別で並行 viewer 安全 |
| 子プロセス検出 | viewer が `Child::try_wait()` を 1 Hz で呼ぶ + control flag polling | クロスプラットフォーム、追加 API 不要 |
| 多重起動防止 | viewer 側で `Option<Child>` を保持、ESC 押下時 alive check → 既存なら no-op | 単純、フォーカス戻しは OS 依存なので諦める |
| stats 更新頻度 | viewer 書き出し 1 Hz、overlay polling 5 Hz | latency 表示は 1 Hz で十分自然、CPU 負荷ほぼゼロ |
| シリアライズ | `serde_json`(workspace 既存) | toml は overspec、bincode は人間が読めない |
| バイナリ名 | `prdt-viewer-overlay`(suffix なし、cargo が自動付与) | クロスプラットフォーム慣例 |
| spawn 検出経路 | viewer は実行中 binary と同ディレクトリの `prdt-viewer-overlay[.exe]` を探す | `cargo install` でも `cargo run` でも target/debug/ でも統一的に動く |
| overlay 終了モード | ユーザーが Resume / Disconnect ボタン押下、または X クローズ | Disconnect は control file 書く + 自身終了、Resume / X は単に終了 |
| viewer 終了時のクリーンアップ | Drop で IPC ディレクトリを削除、子プロセスは kill | リーク防止 |
| --headless との整合 | viewer が `--headless` なら ESC 検出無効、子プロセス spawn しない | CI / 自動テストで邪魔にならない |
| i18n | `prdt-gui-common` の `en.ftl` / `ja.ftl` に 7-8 ID 追加 | G6 の i18n 仕組みをそのまま再利用 |

---

## Architecture

### プロセス・データフロー

```
[viewer process]                               [overlay process]
  - winit event loop                              - eframe::run_native
  - existing render path                          - polls stats.json @ 5 Hz
  - LatencyProbe.snapshot()                       - shows latency p50/p95/p99
        ↓                                         - displays decoder, samples
  writes stats.json @ 1 Hz                        - "Resume" → close window
        ↓                                         - "Disconnect" → write
   IPC dir                                          control.json + close
   (per-PID isolated)
        ↑
  reads control.json @ 1 Hz
        ↓
  if action == "disconnect":
     send Bye + exit

  detects child exit via try_wait
        ↓
  on next ESC: spawn fresh child
```

### モジュール / ファイル配置

```
crates/viewer-overlay/                       (新)
  Cargo.toml
  src/
    main.rs                                  bin entry — eframe app
    app.rs                                   OverlayApp impl eframe::App
    ipc.rs                                   stats reader + control writer

crates/viewer/src/
  overlay_ipc.rs                             (新) stats writer + control reader
                                             + IPC dir管理
  overlay_supervisor.rs                      (新) Child管理 + ESC handler

crates/viewer/src/main.rs                    (修正) ESC キー検出 → spawn、stats
                                             write loop、control poll、
                                             cleanup on exit

crates/gui-common/locales/{en,ja}/main.ftl   (修正) overlay-* IDs 追加
```

### IPC スキーマ

#### `<ipc_dir>/stats.json`(viewer 書き、overlay 読み)

```json
{
  "version": 1,
  "viewer_pid": 12345,
  "updated_at_unix_ms": 1714024822123,
  "connection_state": "connected",
  "host_label": "192.168.1.5:9000",
  "decoder": "nvdec",
  "latency_us": {
    "p50": 18234,
    "p95": 41023,
    "p99": 67100,
    "samples": 512
  },
  "fps_observed": 59.8
}
```

`connection_state`: `"connecting"` | `"connected"` | `"disconnecting"`

`latency_us` が `null` の場合は「まだサンプル無し」(connecting 中)。overlay は「Connecting…」表示にフォールバック。

#### `<ipc_dir>/control.json`(overlay 書き、viewer 読み)

```json
{
  "action": "disconnect",
  "issued_at_unix_ms": 1714024830000
}
```

`action`: 現状は `"disconnect"` のみ。将来的に `"toggle_audio"`、`"set_volume"` 等を追加可能。

viewer が一度 control を読んだら同ファイルを削除して "consumed" にする(idempotent)。

### IPC ディレクトリのライフサイクル

```
viewer 起動時:
  ipc_root = dirs::cache_dir().join("prdt/overlay-ipc")
  ipc_dir = ipc_root.join(pid.to_string())
  fs::create_dir_all(&ipc_dir)

ESC 押下時:
  if child is None or child.try_wait() returned Some(_):
    spawn prdt-viewer-overlay --ipc-dir <ipc_dir>
    self.child = Some(child)

毎フレーム(または 1 Hz tick):
  write stats.json with current snapshot
  if control.json exists:
    read action
    fs::remove_file(control.json)
    handle action (e.g. set self.disconnect_requested = true)

viewer 終了時:
  if let Some(child) = self.child.take():
    let _ = child.kill();
    let _ = child.wait();
  fs::remove_dir_all(&ipc_dir)
```

`Drop for OverlaySupervisor` でクリーンアップを保証。

### Cross-platform notes

- **バイナリ探索**: `std::env::current_exe()?.parent()?.join(format!("prdt-viewer-overlay{}", std::env::consts::EXE_SUFFIX))`。`EXE_SUFFIX` は Windows で `.exe`、他で空。
- **`dirs::cache_dir()`**: Win = `%LOCALAPPDATA%\Temp` 相当 / Linux = `$XDG_CACHE_HOME` or `~/.cache` / macOS = `~/Library/Caches`。全 OS で書き込み可能。
- **PID 取得**: `std::process::id()` — 全 OS 対応。
- **`Child::kill()`**: Win = `TerminateProcess`、Unix = `SIGKILL`。両方とも子プロセスを即座に殺す。overlay 側でクリーンアップは期待しない(viewer 側で IPC dir 削除責任)。
- **`Child::try_wait()`**: 全 OS 非ブロッキング、安全。
- **ファイル書き込みアトミック性**: stats.json は `tempfile + rename` で atomic に書く(`fs::rename` は同一ファイルシステムなら全 OS atomic)。overlay の polling 中に半端な JSON を読まない。
- **macOS 固有**: `current_exe()` は app bundle 内の `Contents/MacOS/<binary>` を返す。同ディレクトリに `prdt-viewer-overlay` を入れる前提(`cargo-bundle` か Phase 4 G4 MSI 相当の app bundle 戦略は別タスク)。

### Overlay GUI レイアウト

```
┌─ Power Remote Desktop — Overlay ────────────────────────┐
│  Connected to: 192.168.1.5:9000                          │
│                                                           │
│  Latency                                                  │
│  ┌─────────────────────────────────────────────────────┐ │
│  │  p50:   18.2 ms                                      │ │
│  │  p95:   41.0 ms                                      │ │
│  │  p99:   67.1 ms                                      │ │
│  │  samples: 512                                        │ │
│  └─────────────────────────────────────────────────────┘ │
│                                                           │
│  Decoder: NVDEC (zero-copy)                               │
│  FPS: 59.8                                                │
│                                                           │
│        [ Resume ]   [ Disconnect ]                        │
└──────────────────────────────────────────────────────────┘
```

`connection_state == "connecting"` のとき:

```
┌─ Power Remote Desktop — Overlay ────────────────────────┐
│  Connecting…                                              │
│                                                           │
│  Latency: (not yet sampled)                               │
│  Decoder: NVDEC (zero-copy)                               │
│                                                           │
│        [ Resume ]   [ Disconnect ]                        │
└──────────────────────────────────────────────────────────┘
```

Window サイズ: 360 × 280、最小サイズ同。背景 egui デフォルト(不透明、リサイズ不可)。

### 既存 viewer への変更点(最小)

```rust
// crates/viewer/src/main.rs

// Args に追加(--headless 既にあり)
// (新規 CLI フラグ無し — overlay は ESC 自動 spawn)

struct ViewerApp {
    // ... existing fields ...
    overlay: Option<overlay_supervisor::OverlaySupervisor>,
    last_stats_write: std::time::Instant,
}

// resumed() 内で OverlaySupervisor を初期化(--headless でなければ)
fn resumed(&mut self, event_loop: &ActiveEventLoop) {
    // ... existing ...
    if !self.headless {
        match overlay_supervisor::OverlaySupervisor::new() {
            Ok(s) => self.overlay = Some(s),
            Err(e) => warn!(?e, "overlay disabled"),
        }
    }
}

// window_event() の KeyboardInput 処理に追加
WindowEvent::KeyboardInput { event, .. } => {
    if event.physical_key == PhysicalKey::Code(KeyCode::Escape)
        && event.state == ElementState::Pressed
    {
        if let Some(ref mut s) = self.overlay {
            if let Err(e) = s.toggle_spawn() {
                warn!(?e, "overlay spawn failed");
            }
        }
        // ESC は host にも転送するか?— No(オーバーレイ専用)
        return;
    }
    // ... existing input forwarding ...
}

// about_to_wait() 内、または専用 1Hz tick:
if self.last_stats_write.elapsed() >= Duration::from_secs(1) {
    if let Some(ref s) = self.overlay {
        let snap = self.shared.latency.snapshot();
        let _ = s.write_stats(/* build StatsPayload from snap */);
        if let Some(action) = s.read_control() {
            if action == "disconnect" {
                self.disconnect_requested = true;
            }
        }
    }
    self.last_stats_write = std::time::Instant::now();
}

// exiting() 内: overlay supervisor の Drop に任せる(明示処理不要)
```

---

## Testing Strategy

### 1. `viewer::overlay_ipc` unit tests

```rust
#[test]
fn stats_round_trip_through_json() {
    let dir = tempfile::tempdir().unwrap();
    let s = OverlaySupervisor::with_ipc_dir(dir.path().to_path_buf());
    let payload = StatsPayload { /* ... */ };
    s.write_stats(&payload).unwrap();
    let parsed: StatsPayload = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("stats.json")).unwrap()
    ).unwrap();
    assert_eq!(payload, parsed);
}

#[test]
fn control_flag_consumed_after_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("control.json"), r#"{"action":"disconnect"}"#).unwrap();
    let s = OverlaySupervisor::with_ipc_dir(dir.path().to_path_buf());
    let action = s.read_control().unwrap();
    assert_eq!(action, "disconnect");
    assert!(!dir.path().join("control.json").exists()); // consumed
}

#[test]
fn ipc_dir_isolated_per_pid() {
    // OverlaySupervisor::new() should pick a dir under
    // dirs::cache_dir()/prdt/overlay-ipc/<pid>/.
    // Two simulated PIDs → two distinct dirs.
}
```

### 2. `viewer-overlay::ipc` unit tests

```rust
#[test]
fn missing_stats_yields_connecting_state() {
    let dir = tempfile::tempdir().unwrap();
    let r = read_stats(dir.path());
    assert!(matches!(r, Err(_)) || matches!(r.unwrap().connection_state.as_str(), "connecting"));
}

#[test]
fn write_disconnect_flag_creates_control_json() {
    let dir = tempfile::tempdir().unwrap();
    write_disconnect(dir.path()).unwrap();
    let raw = std::fs::read_to_string(dir.path().join("control.json")).unwrap();
    assert!(raw.contains("\"disconnect\""));
}
```

### 3. Manual smoke

- viewer GUI 起動 → 接続 → 動画表示 → ESC 押下 → overlay ウィンドウが現れる
- overlay の latency 数値が 1 秒おきに更新される
- overlay の Resume → ウィンドウ閉じる、ESC 再押下で再オープン
- overlay の Disconnect → viewer が終了、Bye が host に送られたことを host.log で確認
- viewer を Ctrl+C で殺す → overlay も同時に終了する(viewer の Drop が child.kill を呼ぶ)
- viewer を `--headless` で起動 → ESC で overlay 起動しない(現行 ESC 動作維持)

### 4. `prdt-viewer-overlay` 単独実行スモーク(IPC 手動シミュレーション)

```bash
mkdir -p /tmp/prdt-overlay-test
cat > /tmp/prdt-overlay-test/stats.json <<EOF
{"version":1,"viewer_pid":42,"updated_at_unix_ms":0,"connection_state":"connected","host_label":"test","decoder":"mf","latency_us":{"p50":15000,"p95":30000,"p99":50000,"samples":100},"fps_observed":60.0}
EOF
./target/debug/prdt-viewer-overlay --ipc-dir /tmp/prdt-overlay-test
# Expect: overlay opens, shows the values from stats.json
```

### 5. clippy / fmt

`cargo clippy --workspace --all-targets --all-features -- -D warnings` clean、新規ファイル fmt clean

---

## Exit Criteria

- [ ] 新クレート `crates/viewer-overlay/` 作成、ビルド通過
- [ ] 新モジュール `crates/viewer/src/{overlay_ipc.rs,overlay_supervisor.rs}` 実装
- [ ] viewer の ESC キー検出 → overlay spawn が動作
- [ ] viewer が 1 Hz で stats.json を書き出し
- [ ] overlay が 5 Hz で stats.json を polling して latency 表示更新
- [ ] overlay の Disconnect ボタンで viewer 終了
- [ ] viewer の `--headless` で overlay が一切起動しない
- [ ] 7-8 個の i18n ID(`overlay-*`)を en.ftl / ja.ftl に追加
- [ ] unit tests(IPC round-trip + 隔離)pass
- [ ] workspace 全テスト pass
- [ ] clippy clean
- [ ] git tag `phase4-g2-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| バイナリパス解決失敗(`current_exe()` が予期せぬ場所返す) | overlay 起動不可、サイレント失敗 | spawn 失敗時 warn ログ、ESC を host に転送する fallback (現状 ESC は host に送られている)|
| IPC ファイル書き込み権限不足(read-only filesystem 等) | overlay が動かない | `OverlaySupervisor::new()` で書き込み確認、失敗時 None で運用継続 |
| 子プロセス zombie(viewer crash で overlay 残る) | UX 不良 | overlay 側で「`updated_at_unix_ms` が 5 秒以上古い → self-exit」watchdog |
| 同時複数 viewer で IPC 衝突 | overlay が他の viewer の stats を読む | PID 別ディレクトリで隔離(spec で確定) |
| ESC が host に転送されてしまう(競合) | host 側でアプリ終了等が発生 | ESC 押下を捕捉した時点で `return` し input forwarding をスキップ |
| `EXE_SUFFIX` ロジック間違いで Linux 起動失敗 | spawn エラー | `std::env::consts::EXE_SUFFIX` を直接使う(自前 cfg(windows) 不使用) |
| macOS app bundle 内パス問題 | overlay 起動不可 | Phase 4 G4 で app bundle 化するときに統合検証、G2 単体では Windows + Linux 確認のみ |
| --headless 検出漏れで CI に overlay popup が出る | テスト破綻 | Args.headless 必須チェック、resumed() で初期化スキップ + テスト追加 |

---

## Open Questions(実装中に決めてよい)

- overlay ウィンドウの初期位置(画面中央 or 右上 or 直近位置)— 中央デフォルトで簡素化
- stats.json のフォーマットバージョン管理 — `version: 1` フィールドを入れて将来 break 用に予約済
- overlay が viewer 接続待ち中(connecting state)に開かれた場合の挙動 — 「Connecting…」表示で待たせる(Disconnect は接続前でも有効)
- IPC dir のクリーンアップタイミング — Drop で削除、ただし viewer が SIGKILL されたら残る。次回起動時に「自分の PID と異なる古いディレクトリは削除」する gc を検討(本 spec では実装しない)
- overlay が複数開かれた場合 — viewer 側で 1 つだけ管理、新たな ESC で「既存 child が alive か?」をチェック、alive なら no-op

---

## References

- 親 spec: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(F3「viewer in-stream overlay」)
- G1 spec: `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`
- G6 spec: `docs/superpowers/specs/2026-04-25-phase4-g6-i18n-design.md`(i18n 仕組み再利用)
- LatencyProbe: `crates/viewer/src/latency.rs`(`snapshot() -> LatencyStats { p50_us, p95_us, p99_us, samples }`)
- `dirs` crate: <https://docs.rs/dirs/5.0/>
- `std::env::consts::EXE_SUFFIX`: <https://doc.rust-lang.org/std/env/consts/constant.EXE_SUFFIX.html>
