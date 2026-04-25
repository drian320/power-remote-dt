# Phase 4 G1 — egui Foundation + Host GUI + Viewer Launcher (Design)

**Project**: power-remote-dt
**Phase**: 4 (GUI + 配布)、サブプラン G1
**Date**: 2026-04-25
**Status**: Draft (built on `plan2d-zerocopy-complete` master)
**Parent spec**: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(Phase 4 全体)

---

## Summary

Phase 4 GUI の最初のサブプラン。CLI 専用だった `prdt-host` / `prdt-viewer` に egui ベースの GUI 入口を追加する。host は「鍵生成 → pubkey/QR 表示 → 待ち受け開始 → 稼働状態」を 1 ウィンドウで完結。viewer は「保存済み接続先一覧 → 選んで接続 → 既存の winit/D3D11 ウィンドウへ遷移」のランチャー。既存の CLI フラグはすべて `--headless` で保持。

到達目標: G1 終了時点で **CLI を打たずに** host を起動して viewer から接続できる。tray / overlay / インストーラ / 自動更新 / クラッシュレポータは G2 以降。

---

## Scope

### In-scope (G1)

- 新クレート 3 つ:
  - `crates/gui-common/` — `Config` 型、TOML 入出力、egui スタイル + 日本語フォント、QR 生成
  - `crates/gui-host/` — host eframe アプリ(F1)
  - `crates/gui-viewer/` — viewer launcher eframe アプリ(F2)
- 既存 binary の改造:
  - `prdt-host.exe`: 既定で GUI モード起動、`--headless` で従来 CLI 互換、`--config <PATH>` で config.toml の場所を指定可
  - `prdt-viewer.exe`: 既定で GUI ランチャー、`--headless` で従来 CLI 互換
- 設定ファイル: `%APPDATA%\prdt\config.toml`(WIndows のみ。Linux 対応は Phase 1)
- F1 host GUI:
  - 初回起動: 「鍵を生成」ボタン → `host-key.bin` 作成 → pubkey base64 + QR コード表示
  - 2 回目以降: pubkey + 接続中 viewer 数 + listening port + 直近ログ tail を表示
  - 「待ち受け開始 / 停止」ボタンで既存 host サーバを spawn / cancel
  - 「設定」パネルで bind / monitor / bitrate / outgoing_dir / signaling_url を編集 → config.toml に保存
- F2 viewer launcher:
  - 保存済み接続先一覧(config.toml `[[viewer.hosts]]` 配列)
  - 接続先のラベル / アドレス / host_id / pubkey / 最終接続日時 を表示
  - 「新規追加」フォーム: ラベル + 接続モード(直接 IP / signaling)+ アドレス or host_id + pubkey base64
  - 「接続」ボタン: GUI を閉じ、既存 winit/D3D11 viewer に CLI 引数相当の値を渡して遷移(同一プロセス)
  - 「設定」: decoder(mf / nvdec)、解像度、recv_dir、signaling_url、known-hosts ファイル の編集
- `--headless` 互換: 両 binary とも `--headless` フラグが付くか、既存の必須フラグ(`--bind`/`--host-id`/`--host-pubkey`/`--signaling-url` など)が CLI に与えられた場合は GUI を立ち上げず従来の CLI 経路を走る

### Out (G2 以降)

- viewer in-stream overlay(latency p50/p95、ESC メニュー)→ G2
- system tray + auto-start(タスクトレイ常駐)→ G3
- MSI installer + 自動更新 → G4
- crash reporter + Authenticode 署名 → G5
- 多言語化(日本語 / 英語以外)→ G6、ただし G1 では英語ストリングのみ
- Linux / macOS の GUI(別 Phase)
- web カメラ経由での QR 読み取り(モバイル QR 出力で開始するが、視覚的には base64 ペーストで足りるので G1 後でよい)
- log tail パネルの実装は **直近 200 行のシンプル文字列バッファ**(tracing から拾う仕組みは G1 で入れるが、フィルタ/検索/エクスポートは G3+)
- アンインストール時の鍵 / 接続履歴の扱い → G4

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| GUI フレームワーク | `eframe` 0.28+(egui ベース) | parent spec 確定 |
| 同居方式 | host は GUI が supervisor(常駐)、viewer は GUI が launcher(接続後に GUI 終了 → 既存 winit へ遷移) | host は稼働状態を継続表示、viewer は描画ウィンドウに集中 |
| QR 生成 | `qrcode` crate(pure Rust、MIT/Apache2)→ `egui::ColorImage` に変換 | 軽量、依存追加最小 |
| ファイルダイアログ | `rfd` crate(Native File Dialog ラッパー、MIT) | dir picker / file picker 両用 |
| 設定ファイル | TOML、`toml` crate(MIT/Apache2、既に signaling-server 依存) | 人間可読、既存依存 |
| 設定ディレクトリ | `dirs::config_dir()` 経由(`%APPDATA%\prdt\`)→ G6 で localization の locale dir も同所 | クロスプラットフォーム抽象、Linux 拡張時に再利用可 |
| 日本語フォント | Noto Sans CJK JP の小サブセットを `gui-common` に静的埋め込み | GUI 起動の自己完結、依存無し |
| host server の spawn | `tokio::task::spawn` で既存 `prdt-host` の main loop 関数を呼び、`CancellationToken` で停止 | 既存 main を最小改造で関数化 |
| viewer 起動 | eframe ループ終了 → `winit` を main thread で起動(既存 viewer コード) | プロセス分割しない、eframe と winit は同居せず順次 |
| ログ tail | `tracing_subscriber` に追加の `Layer` 実装で `Mutex<VecDeque<String>>` に push、GUI が読む | 既存 stderr 出力は維持 |
| binary 配置 | 既存 `prdt-host.exe` / `prdt-viewer.exe` のまま、GUI コードは crate 化 | 配布は G4 で考える、G1 では distinct binary 不要 |
| `--headless` セマンティクス | フラグ名そのもの。既存の必須 CLI フラグが付いた場合の互換性は **`--headless` 必須**(自動 fallback はしない) | `--bind 0.0.0.0:9000` だけ付けてもなお GUI を出してしまうリスクを排除、明示性優先 |

---

## Architecture

### モジュール / ファイル配置

```
crates/
  gui-common/                      (新)
    Cargo.toml
    src/
      lib.rs                       — pub re-exports
      config.rs                    — Config struct + TOML I/O + paths
      style.rs                     — egui style + JP font setup
      qr.rs                        — QR code → ColorImage helper
      log_tail.rs                  — tracing layer that captures last N lines

  gui-host/                        (新)
    Cargo.toml
    src/
      lib.rs                       — `pub fn run_host_gui(args) -> Result<()>` (eframe entry)
      app.rs                       — `HostApp { ... }` impl eframe::App
      keygen.rs                    — first-run key generation flow
      status.rs                    — running-state UI (pubkey/QR/peers/bitrate/log)
      settings.rs                  — settings panel (bind/monitor/bitrate/outgoing/...)

  gui-viewer/                      (新)
    Cargo.toml
    src/
      lib.rs                       — `pub fn run_viewer_launcher(args) -> Result<LaunchOutcome>` (eframe)
      app.rs                       — `LauncherApp` impl eframe::App
      hosts_list.rs                — saved-hosts list UI
      connect_form.rs              — add-new-host form + connect dialog
      settings.rs                  — viewer prefs panel

  host/src/main.rs                  — extend Args with `--headless`, `--config`;
                                     main() routes to run_host_gui() unless --headless

  viewer/src/main.rs                — extend Args with `--headless`, `--config`;
                                     main() routes to run_viewer_launcher() unless --headless;
                                     on `LaunchOutcome::Connect(args)`, fall through to existing
                                     winit/D3D11 viewer with the form-derived args
```

### 既存 crate との依存関係

```
prdt-host (bin)
  ├── existing crates (transport, crypto, media-win, ...)
  └── gui-host (NEW)
       └── gui-common (NEW)

prdt-viewer (bin)
  ├── existing crates
  └── gui-viewer (NEW)
       └── gui-common (NEW)
```

`gui-common` は `prdt-protocol`(`PubKey` だけ)以外 既存ロジックに依存しない(pure UI / config 層)。
`gui-host` / `gui-viewer` は `prdt-protocol` の wire 型に依存して config フィールドを持つ。

### ホスト GUI のスレッディング

```
main thread:
  - eframe::run_native(HostApp)
  - HostApp::update() で UI を描画
  - 「待ち受け開始」ボタン押下 → spawn_blocking 内で tokio::Runtime::new() + spawn(host_main_loop_with_cancel(token, args))
  - スレッド間共有: Arc<Mutex<HostStatus>>(peers, bitrate, last_log_lines, ...)
  - 「停止」ボタン押下 → cancel_token.cancel() → tokio task が drop → status を Idle に戻す
```

`host_main_loop_with_cancel` は既存の `host::main` の本体を `pub async fn run_host(args, status: Arc<Mutex<HostStatus>>, cancel: CancellationToken) -> Result<()>` として切り出す。CLI mode の `main` は同じ関数を呼んで blocking で待つだけになる。

### ビューアー GUI 〜 既存 viewer 遷移

```
main thread:
  - parse_args() — Args { headless: bool, config: Option<PathBuf>, ... existing flags ... }
  - if !headless && existing required flags absent:
        let outcome = gui_viewer::run_viewer_launcher(args.config)?;
        match outcome {
            LaunchOutcome::Connect(connect_args) => {
                // mutate `args` to reflect the user's launcher choice,
                // OR construct a new Args struct
                // fall through to existing winit/D3D11 main
            }
            LaunchOutcome::Quit => return Ok(()),
        }
  - existing winit_main(args) ...
```

eframe::run_native は blocking call(MainEventsCleared 駆動)。ユーザーが「接続」ボタンを押した時点で `eframe::App::update` が `frame.close()` を呼び、`run_native` から戻る。launcher が「接続フォームの内容」を `Arc<Mutex<Option<LaunchOutcome>>>` に書き込んでから close するので、main は close 直後にそれを取り出して既存 winit に渡す。

### 設定ファイル

`%APPDATA%\prdt\config.toml`:

```toml
[host]
bind = "0.0.0.0:9000"
monitor = 0
bitrate_mbps = 30
outgoing_dir = "C:/Users/alice/prdt-outgoing"
signaling_url = "ws://signaling.example.com:8080/signal"  # optional
host_id_file = "host-id.txt"
key_file = "host-key.bin"
auto_start = false  # G1 では未使用、G3 で使う

[viewer]
recv_dir = "C:/Users/alice/Downloads/prdt-received"
decoder = "mf"               # "mf" | "nvdec"
default_resolution = "1920x1080"  # informational, not yet enforced
default_fps = 60
signaling_url = "ws://signaling.example.com:8080/signal"
known_hosts = "known-hosts"
known_host_ids = "known-host-ids"

[[viewer.hosts]]
label = "Office PC"
mode = "direct"               # "direct" | "signaling"
addr = "192.168.1.5:9000"     # required if mode == "direct"
host_id = ""                  # required if mode == "signaling"
pubkey = "AAAAB3NzaC1yc2E..."
last_connected = "2026-04-23T14:30:00Z"

[[viewer.hosts]]
label = "Home"
mode = "signaling"
addr = ""
host_id = "123-456-789"
pubkey = ""                   # empty => TOFU on first connect (existing behavior)
last_connected = "2026-04-25T08:11:00Z"
```

`Config::load(path)` で読み、なければ default を生成して save。
`Config::save(path)` で書く。
`Config::default_path()` → `dirs::config_dir().join("prdt/config.toml")`。

### Wire types

`gui-common::config::Config` は serde derive + TOML。host / viewer GUI は config を Arc<Mutex<Config>> で共有して、編集 → save を atomic に。

`gui-viewer::LaunchOutcome`:
```rust
pub enum LaunchOutcome {
    Connect(ConnectArgs),
    Quit,
}

pub struct ConnectArgs {
    pub mode: ConnectMode,        // Direct | Signaling
    pub direct_addr: Option<SocketAddr>,
    pub signaling_url: Option<url::Url>,
    pub host_id: Option<String>,
    pub pubkey: Option<String>,   // base64; empty for TOFU
    pub decoder: String,          // "mf" | "nvdec"
    pub recv_dir: PathBuf,
    pub known_hosts_path: PathBuf,
    pub known_host_ids_path: PathBuf,
}
```

`gui-host::HostStatus`:
```rust
pub struct HostStatus {
    pub state: HostState,         // Idle | Listening | Stopping
    pub pubkey_b64: String,
    pub allocated_host_id: Option<String>,  // from signaling, after register
    pub listening_addr: Option<SocketAddr>,
    pub peers_connected: u32,
    pub bitrate_mbps_actual: f32,
    pub last_log_lines: VecDeque<String>,    // capacity 200
}
```

---

## F1 — Host GUI flow

### 起動シーケンス

1. `prdt-host.exe` 起動 → Args::parse
2. `--headless` 付き OR 既存必須 CLI フラグ全部揃いなら従来 CLI へ
3. それ以外: `gui_host::run_host_gui(args.config)` を呼ぶ
4. eframe::run_native で `HostApp` ウィンドウが開く

### 初回(`host-key.bin` が無い場合)

```
┌─ Power Remote Desktop — Host ────────────────────────┐
│  Welcome.                                            │
│                                                       │
│  Generate a host key to start. The key uniquely      │
│  identifies this machine to viewers.                 │
│                                                       │
│  Key file: %APPDATA%\prdt\host-key.bin               │
│                                                       │
│         [ Generate host key ]                        │
└──────────────────────────────────────────────────────┘
```

「Generate host key」押下 → `KeyPair::generate()` → `host-key.bin` 書き込み → 次の状態へ遷移。

### 鍵あり / 待機中(Idle)

```
┌─ Power Remote Desktop — Host ────────────────────────┐
│  Status: Idle                                        │
│                                                       │
│  Public key:                                         │
│  ┌────────────────────────────────────────────────┐  │
│  │ AAAAB3NzaC1yc2E... [Copy]                      │  │
│  └────────────────────────────────────────────────┘  │
│                                                       │
│       ┌─────────────┐                                │
│       │  ▓▓▓▓▓▓▓▓   │  ← QR (pubkey + optional      │
│       │  ▓ ▓▓▓ ▓▓   │     host_id if assigned)       │
│       │  ▓▓ ▓ ▓▓▓   │                                │
│       └─────────────┘                                │
│                                                       │
│  Bind:    0.0.0.0:9000      [Settings...]            │
│  Monitor: 0                                          │
│  Bitrate: 30 Mbps                                    │
│                                                       │
│   [ Start listening ]                                │
└──────────────────────────────────────────────────────┘
```

「Start listening」押下:
1. config の現在値で host main loop を spawn
2. status を Listening に
3. UI が peers / bitrate / log tail を継続表示

### 待ち受け中(Listening)

```
┌─ Power Remote Desktop — Host ────────────────────────┐
│  Status: ● Listening on 0.0.0.0:9000                 │
│                                                       │
│  Connected viewers: 1                                │
│  Bitrate (current): 28.4 Mbps                        │
│  Public key: AAAAB3NzaC1yc2E... [Copy]              │
│                                                       │
│  Recent activity:                                    │
│  ┌────────────────────────────────────────────────┐  │
│  │ 12:34:01 INFO viewer connected from ...        │  │
│  │ 12:34:02 INFO Hello/HelloAck OK                │  │
│  │ 12:34:02 INFO encoder ready (NVENC HEVC)       │  │
│  │ ...                                             │  │
│  └────────────────────────────────────────────────┘  │
│                                                       │
│   [ Stop ]   [ Settings... ]                         │
└──────────────────────────────────────────────────────┘
```

「Stop」押下: cancel_token.cancel() → host main loop が落ちる → Idle に戻る。

### 設定パネル(モーダル)

```
┌─ Settings ───────────────────────────────────────────┐
│  Bind:        [ 0.0.0.0:9000 ]                       │
│  Monitor:     [▼ 0 (Display 1) ]                     │
│  Bitrate:     [ 30 ] Mbps                            │
│  Outgoing:    [ C:\...\prdt-outgoing ] [Browse]      │
│  Signaling:   [ ws://signaling.example.com:8080/  ]  │
│  Host ID file:[ %APPDATA%\prdt\host-id.txt ] [Browse]│
│                                                       │
│      [ Cancel ]      [ Save ]                        │
└──────────────────────────────────────────────────────┘
```

「Save」 → config.toml に書き戻し、UI 状態を更新。「Listening」中の変更は次回 Start 時に反映(spec G1 では再起動必要なものだけサポート、live restart は G3+)。

---

## F2 — Viewer launcher flow

### 起動シーケンス

1. `prdt-viewer.exe` 起動 → Args::parse
2. `--headless` OR 既存必須 CLI が指定 → 従来 CLI で winit_main へ
3. それ以外: `gui_viewer::run_viewer_launcher(config_path)` を呼ぶ
4. eframe::run_native で `LauncherApp` が開く
5. ユーザー操作 → `LaunchOutcome` を返して終了
6. main が outcome を受け取り、Connect なら既存 winit_main に args を流して入る

### 接続先一覧

```
┌─ Power Remote Desktop — Viewer ──────────────────────┐
│  Saved connections:                                  │
│                                                       │
│  ┌────────────────────────────────────────────────┐  │
│  │ ● Office PC                                    │  │
│  │   192.168.1.5:9000 · pubkey AAA…    last 4/23 │  │
│  ├────────────────────────────────────────────────┤  │
│  │ ● Home                                         │  │
│  │   ID 123-456-789 (signaling)        last 4/25 │  │
│  ├────────────────────────────────────────────────┤  │
│  │ + Add new connection                           │  │
│  └────────────────────────────────────────────────┘  │
│                                                       │
│  Decoder: [▼ mf ]   [Settings...]                    │
│                                                       │
│   [ Connect ]   [ Quit ]                             │
└──────────────────────────────────────────────────────┘
```

接続先を選択 → 「Connect」押下 → eframe close → main が `LaunchOutcome::Connect(args)` を受け取って既存 winit へ。

### 新規追加フォーム(モーダル)

```
┌─ Add Connection ─────────────────────────────────────┐
│  Label:        [ My laptop                       ]   │
│  Mode:         (●) Direct  ( ) Signaling             │
│                                                       │
│  Address:      [ 192.168.1.10:9000              ]    │
│   — or —                                              │
│  Host ID:      [ 123-456-789                    ]    │
│                                                       │
│  Public key:   [ base64 from host display       ]    │
│                  Leave empty to TOFU on first connect │
│                                                       │
│      [ Cancel ]      [ Save ]                        │
└──────────────────────────────────────────────────────┘
```

ラベル必須。Mode に応じて Address か Host ID のどちらか必須。Pubkey は任意(空なら known-hosts に追記して TOFU)。

### 設定パネル

```
┌─ Settings ───────────────────────────────────────────┐
│  Decoder:           (●) MF (default)                 │
│                     ( ) NVDEC (zero-copy)            │
│                                                       │
│  Default resolution: [▼ 1920x1080 ]                  │
│  Default fps:        [ 60 ]                          │
│  Receive directory:  [ C:\…\prdt-received ] [Browse] │
│  Signaling URL:      [ ws://...           ]          │
│  Known hosts file:   [ %APPDATA%\…\known-hosts  ]    │
│  Known host IDs:     [ %APPDATA%\…\known-host-ids ]  │
│                                                       │
│      [ Cancel ]      [ Save ]                        │
└──────────────────────────────────────────────────────┘
```

---

## `--headless` 互換性

両 binary の Args に `headless: bool` フラグを追加。動作:

| 起動方法 | 挙動 |
|---|---|
| `prdt-host.exe --headless --bind 0.0.0.0:9000` | 既存 CLI モード(GUI 非表示)|
| `prdt-host.exe`(フラグ無し)| GUI モード起動 |
| `prdt-host.exe --bind 0.0.0.0:9000`(headless 無し)| **GUI 起動**(設定の bind に CLI 値を上書きしない、既存設定を使う)— ユーザー意図が曖昧なため |
| `prdt-viewer.exe --headless --signaling-url ... --host-id ...` | 既存 CLI モード |
| `prdt-viewer.exe`(フラグ無し)| ランチャー起動 |
| `prdt-viewer.exe --signaling-url ...`(headless 無し)| **ランチャー起動**(CLI 値は無視)|

これにより既存の自動テスト / CI(`--headless` 付きの CLI 引数を渡してくる)は無変更で動く。GUI モードと CLI モードの切り替えは明示的。

---

## Testing Strategy

### 1. `gui-common` unit tests

- `Config::default()` の round-trip(serialize → deserialize で同じ値)
- `Config::load(missing path)` → default 値 + 自動 save
- `qr::generate(b64_pubkey)` → 非空 ColorImage、大きさ妥当
- `log_tail::TailLayer` が直近 N 行を保持(N+1 行目で先頭が落ちる)

### 2. `gui-host` integration

- eframe::App は GUI コードのまま実行が難しいので、ロジック層(状態遷移)を `HostAppLogic` という pure な struct に切り出し、`HostAppLogic::on_generate_key()` / `on_start_listening()` / `on_stop()` を unit test で叩く
- 「`host-key.bin` が無い状態で on_generate_key → ファイル作成 + 状態遷移」を temp dir で確認

### 3. `gui-viewer` integration

- `LauncherAppLogic::on_connect(idx)` → 選択された host の値で `LaunchOutcome::Connect(args)` を組み立てる
- `add_new_host` フォームのバリデーション(label 必須、mode に応じた addr/host_id 必須、pubkey 空でも OK)

### 4. host bin / viewer bin 統合

- `prdt-host.exe --headless --help` → 既存と同じ usage が出る(回帰チェック)
- `prdt-viewer.exe --headless --signaling-url ws://... --host-id 123-456-789` → 既存挙動(CLI 経由で接続まで進む)
- 既存の `phase2-w*` smoke / `decode_*` test に回帰なし

### 5. 手動 smoke(ドキュメント化のみ、自動化なし)

- Machine A: `prdt-host.exe`(GUI)を起動 → 鍵生成 → 待ち受け開始
- Machine B: `prdt-viewer.exe`(GUI)を起動 → host 追加 → 接続 → 画面が出る
- 操作時間 < 1 分(parent spec の Exit Criteria § Phase 4 全体)

### 6. clippy / fmt

- `cargo clippy --workspace --all-targets --all-features -- -D warnings` 0 警告
- 既存パッシュタイル `cargo fmt --all -- --check` の drift は ignore(W6/Plan2d で確認済みの既存問題、G1 では新規ファイルだけ clean に保つ)

---

## Exit Criteria

- [ ] `crates/gui-common/`、`crates/gui-host/`、`crates/gui-viewer/` 作成、ビルド通過
- [ ] `Config` 型の load / save が unit test で round-trip OK
- [ ] `gui-host`: 鍵生成 → pubkey 表示 → 待ち受け開始 / 停止 が GUI 上で動作
- [ ] `gui-viewer`: 保存済み接続先一覧 → 新規追加フォーム → 接続 が GUI 上で動作
- [ ] `prdt-host.exe --headless --bind 0.0.0.0:9000 ...` が既存挙動を保つ(W1-W6 smoke 回帰なし)
- [ ] `prdt-viewer.exe --headless ...` が既存挙動を保つ
- [ ] 218 既存 + 新 unit tests すべて pass
- [ ] clippy 0 警告(G1 で追加したコードに限る、既存 drift は別)
- [ ] ドキュメント: `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`(本書)+ implementation plan
- [ ] git tag `phase4-g1-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| eframe と既存 winit/D3D11 の同居が動かない(viewer launcher → 描画への遷移)| viewer GUI 不可 | 順次起動方式(eframe close → winit start)で同時実行を回避。今回は「同時並走」を試みない |
| host サーバを tokio task で動的 spawn / cancel する際に既存 main loop の構造が前提崩壊 | host GUI 不可 | 既存 main loop を `pub async fn run_host(...) -> Result<()>` に切り出し、CLI mode はそれを直接 await する |
| 日本語フォントを静的埋め込みでバイナリサイズ膨張 | binary size +5-10MB | Noto Sans CJK の JP-Reduced(常用漢字 + ひらがな + カタカナのみ)で ~3MB 増程度に抑える。binary 1 個 ~30MB 程度なら許容 |
| 既存の `--headless` ない CLI(`--bind` だけ付ける等)が GUI 起動して混乱 | UX 混乱 | 表で明示。`--headless` 必須にすることでユーザーへの説明は簡潔 |
| `tokio::Runtime` を eframe 内で作ると `block_on` 問題、Send/Sync 違反 | host GUI 不安定 | runtime はメインの`#[tokio::main]` で 1 個だけ作り、eframe ループは `runtime.handle()` を持って spawn する |
| QR 生成失敗(空 pubkey / 巨大データ) | 表示崩れ | qrcode crate の `Result<>` を expect ではなく match で扱い、失敗時は base64 文字列のみ表示 |
| 設定ファイル不整合(壊れた TOML)| GUI 起動失敗 | `Config::load` が失敗したら default を生成して破壊した方を `.bak` にリネーム、警告を log_tail に流す |

---

## Open Questions(実装中に決めてよい)

- 「設定」パネルが modal か side-panel か(modal 推奨、簡単)
- 接続先リストの並び順(最終接続日時 desc がデフォルト?ユーザー編集可?)
- host GUI で「Stop」→ Idle 状態で pubkey を再表示する見た目(F1 と Idle で同じ画面 component を使う?)
- ファイル選択ダイアログの初期ディレクトリ(`%APPDATA%\prdt\` or 最後に開いた場所?)
- viewer launcher の Connect 後、既存 winit が即起動しない場合のエラー表示(launcher を再表示?エラーダイアログ?)— 推奨: エラーダイアログ表示後にプロセス終了

---

## References

- 親 spec: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`
- W6 polish: `docs/superpowers/specs/2026-04-24-phase2-w6-polish-design.md`(`discover_outbound_ip` の参考)
- plan2d-zerocopy: `docs/superpowers/specs/2026-04-25-plan2d-zerocopy-nvdec-design.md`(decoder = nvdec を viewer config で選べる)
- egui ガイド: <https://docs.rs/egui/0.28/>(Context7 で参照可)
- eframe チュートリアル: <https://github.com/emilk/egui/tree/master/examples>(Context7 で参照可)
