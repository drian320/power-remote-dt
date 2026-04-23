# Phase 4: GUI + Distribution — Design

**Project**: power-remote-dt
**Document**: Phase 4 GUI & 配布の設計書
**Date**: 2026-04-23
**Status**: Draft (brainstorming 合意待ち、実装計画未作成)
**Prereq**: Phase 0 〜 Phase 3 完了(`phase3c-multimonitor-complete` / `phase3c-bidirectional-filetransfer-complete` / `plan2d-complete` まで)

---

## Summary

**Phase 4 の目的**: CLI 専用ツールだった power-remote-dt を、非技術者が 5 分以内に接続できるプロダクト品質の GUI アプリに仕上げる。Windows MSI インストーラ、自動更新、コード署名を用意して、OSS として公開可能な配布物を作る。

Phase 0〜3 の CLI で確立した接続フロー(host 起動 → pubkey 表示 → viewer に pubkey を伝える → viewer 起動)を、GUI で **1 画面以内** に圧縮する。暗号鍵管理・接続履歴・QR コード経由のモバイル視聴など、CLI では煩雑だった操作も GUI に畳み込む。

---

## Context & Scope

### Why now

- Phase 0〜3 の機能面は実装・テスト済み(双方向 FT、多モニタ、レイテンシ計測、暗号化、NVDEC)。残るのは **配布と体験の壁** のみ。
- 現状のセットアップ経路:
  1. `host-key.bin` 生成 → stdout に pubkey base64 を印字
  2. ユーザーが pubkey 文字列をコピー、別マシンに運ぶ
  3. viewer に `--host-pubkey <base64>` または `--known-hosts <file>` を渡す
  4. viewer 起動、winit ウィンドウが出る
- これは開発者向け。普通のユーザーには届かない。

### In-scope (Phase 4)

- **host GUI**: Windows アプリ(`.exe`、アイコン付き)
  - 初回起動時の鍵生成ダイアログ + pubkey コピー/QR 表示
  - 稼働状態表示(connected peers、監視中の`--outgoing-dir`、現在のビットレート)
  - システムトレイ常駐 + 自動起動オプション
  - log パネル(現在は stderr に出している tracing 出力を画面内に)
- **viewer GUI**: Windows アプリ
  - 接続履歴を保存するランチャー(host IP + pubkey のペア)
  - 接続時に解像度/FPS/デコーダ選択を GUI で
  - 既存の winit 描画ウィンドウにオーバーレイ(接続状態、レイテンシ p50/p95、ビットレート、ESC メニュー)
- **配布**:
  - MSI インストーラ (`cargo-wix`)
  - バージョン表記、アイコン、右クリック「このマシンでホストを起動」
  - Windows Authenticode コード署名(EV cert か OV cert)
- **自動更新**:
  - `self_update` crate または独自実装
  - GitHub Releases をバックエンドにしたチャネル(`stable` / `beta`)
- **crash reporter**:
  - `panic_hook` で `%LOCALAPPDATA%\prdt\crashes\` にダンプ
  - ユーザー同意で GitHub Issues / Sentry に送信(Phase 5 以降でも可)

### Out (Phase 4 の外)

- Linux / macOS 版 GUI(Phase 1 の LinuxGUI と合流、別 Phase)
- モバイル(iOS / Android)視聴(公式 OSS としては Phase 5+)
- 多言語化 UI(日本語と英語の 2 言語は Phase 4 で入れる、他言語は contrib)
- マーケットプレイスへの登録(Microsoft Store / winget は Phase 5)
- 公式リレーサーバ、ID サーバ(Phase 5)

---

## UI Framework Selection

### Requirements

| # | 要件 | Hard / Soft |
|---|---|---|
| 1 | Rust native、配布が `.exe` 単体(+ DLL 数個)で完結 | Hard |
| 2 | 既存の winit 0.30 + D3D11 レンダパスを壊さない | Hard |
| 3 | Windows 10/11 で tray icon 対応 | Hard |
| 4 | HiDPI、キーボードナビ、アクセシビリティ | Soft |
| 5 | 軽量(cold start < 500ms、binary < 20MB) | Soft |
| 6 | 日本語フォント表示(UI + フォールバック) | Hard |

### Candidates

| Framework | Status | 所感 |
|-----------|--------|------|
| **egui** (egui/eframe) | 成熟 | immediate-mode、軽量、winit と混在可能。スタイルは地味だが実務十分 |
| iced | 成熟 | Elm 式、declarative。デザインは綺麗だが winit とコンポーザビリティが弱い |
| Slint | 商用+FOSS | declarative DSL、LSP 対応、見た目良好。Rust 以外のツール依存 |
| GTK4 (gtk-rs) | 成熟 | Windows は動くが配布が重い(ランタイム DLL) |
| native Win32 (windows-rs) | 〇 | 最軽量だが生産性が最悪、Phase 4 の範囲で書くには重い |
| Tauri | 別方向性 | HTML/CSS/JS UI、WebView2 ランタイム依存。リモートデスクトップと相性が悪い |

### Decision (tentative): **egui (eframe backend) + winit co-existence**

**理由**:
- egui は winit とイベントループを共有できる。viewer の既存 D3D11 レンダとオーバーレイを混在させやすい
- immediate-mode なので状態管理が単純、latencyProbe.snapshot() のような都度取得パターンにフィット
- binary ~5MB、cold start ~100ms、日本語フォントも `egui::FontDefinitions` で載せ替え可能
- host 側は eframe 単独で OK、viewer 側は winit メインの上に egui パスを追加

代替案として **Slint** を残す(視覚的クオリティが効く「公式版」に格上げするとき差し替え可能な設計にしておく)。

### Risks

- egui の tray icon は `tray-icon` crate を別途使う必要あり(winit 非含有)。ポーリングまたは別ウインドウ必要
- D3D11 と egui の合成: egui-wgpu 経由は winit なら素直、D3D11 直描きは自前で texture をコピーする必要がある
- Authenticode 署名のために EV 証明書(年間 300 USD〜)か OV 証明書(100 USD〜)が必要 → Phase 5 予算に組み込むか、Phase 4 完了時点では self-signed + ユーザーの「許可」操作で通す

---

## Feature Breakdown

### F1. Host setup window (first run + ongoing)

**初回起動** (`host-key.bin` が未作成):
- 「ようこそ」画面 + host pubkey 生成ボタン
- 生成後: 公開鍵(base64)と QR コードを大きく表示
- 「このマシンで接続を待ち受ける」ボタン → 常時稼働モード

**2 回目以降**:
- 現在の pubkey + 接続中の viewer 数 + `--outgoing-dir` watcher の状態
- 「停止」「再起動」「設定」ボタン
- ログ tail パネル(直近 200 行、右クリックで全文コピー)

### F2. Viewer connection launcher

- 接続先リスト(`%APPDATA%\prdt\known-hosts.json` に保存)
  - 項目: ラベル(ユーザー命名)、host `IP:port`、pubkey、最終接続日時
- 「新規追加」: host 側で表示された QR を web カメラで読むか、base64 ペースト
- 「接続」で既存の viewer プロセスを起動(設定: 解像度、fps、decoder)

### F3. Viewer in-stream overlay

ESC キー、または画面端ホバーで表示(Parsec / Moonlight と同様):
- 接続状態: Noise handshake OK、現在のレイテンシ p50/p95
- ビットレート、fps、loss rate
- 切断ボタン、フルスクリーン切替、キーボードキャプチャ on/off
- 音声ボリューム

実装方針: 既存の D3D11 swapchain の上に **egui-wgpu で別パスのオーバーレイ** を合成。wgpu は winit 0.30 と同居可能。

### F4. System tray integration

- host: タスクトレイに常駐、アイコンで状態表示(動作中 / アイドル / エラー)
- 右クリックメニュー: 「設定を開く」「停止」「ログを開く」「終了」
- Windows 通知: 新規接続、接続切断、エラー
- `tray-icon` crate を使用

### F5. Auto-update

- `self_update` crate + GitHub Releases
- host / viewer とも週 1 回、または手動チェック
- delta update は Phase 5 以降(初期は full binary replace)
- UAC なしで実行可能な `%LOCALAPPDATA%\prdt\` にインストール

### F6. MSI installer

- `cargo-wix` でビルド
- インストール場所: `%LOCALAPPDATA%\prdt\` (per-user、UAC 不要)
- ショートカット: スタートメニュー、デスクトップ(オプション)
- アンインストール: 鍵と接続履歴は残すか削除か選択
- Windows Defender SmartScreen のホワイトリスト化は Authenticode 署名必須

### F7. Crash reporter

- `std::panic::set_hook` で `PanicInfo` → ファイル出力
- フォーマット: JSON(timestamp、バージョン、スタックトレース、直近ログ 50 行)
- 保存先: `%LOCALAPPDATA%\prdt\crashes\YYYYMMDD-HHMMSS.json`
- UI: 次回起動時に「前回クラッシュしました。レポートを送信しますか?」ダイアログ

---

## Architecture

### Binary layout

現状: `prdt-host.exe`、`prdt-viewer.exe` の 2 つ。

Phase 4 後:
- `prdt-host.exe`: host 側 GUI + 既存のキャプチャ/エンコードコア
- `prdt-viewer.exe`: viewer 側 GUI + 既存の描画/入力コア
- (オプション)`prdt-tray.exe`: tray 常駐用の軽量バイナリ(host が stop 中でも起動待機できるように)
- `prdt.dll`: 共通コア(filetransfer、transport、crypto)— 静的リンク継続で OK、動的分離は Phase 5 課題

### 既存 crate との関係

新 crate 追加:
- `crates/gui-common/`: egui の共通スタイル、QR 生成(`qrcode` + `egui::Image`)、設定ファイル入出力
- `crates/gui-host/`: host 専用 GUI 組み立て(eframe 単独アプリ)
- `crates/gui-viewer/`: viewer 専用 GUI(既存の winit 描画パスに egui オーバーレイを追加)

既存 crate は **無変更** を原則とする。host/viewer の `main.rs` は CLI モードと GUI モードの両方を持つ(`--headless` フラグで CLI 互換を保つ)。

### 設定ファイル

`%APPDATA%\prdt\config.toml`:

```toml
[host]
bind = "0.0.0.0:9000"
monitor = 0
bitrate_mbps = 30
outgoing_dir = "C:/Users/alice/prdt-outgoing"
auto_start = true

[viewer]
recv_dir = "C:/Users/alice/Downloads/prdt-received"
decoder = "mf"                 # or "nvdec"
default_resolution = "1920x1080"
default_fps = 60

[[viewer.hosts]]
label = "Office PC"
addr = "192.168.1.5:9000"
pubkey = "base64=="
last_connected = "2026-04-23T14:30:00Z"
```

`%APPDATA%\prdt\host-key.bin`: 既存の生 32 バイト形式を維持(内部形式を変えると既存セッションの known-hosts が壊れる)。

---

## Implementation Plan (段階分割)

### Plan 4-G1: egui 共通基盤 (~2 週)

- `gui-common` crate 作成、スタイル + 日本語フォント設定 + ini/toml 入出力
- `prdt-host.exe` を eframe ベースの単一ウィンドウアプリに(F1)
- `prdt-viewer.exe` にランチャー画面を追加、接続ボタン → 既存 winit 描画に遷移(F2)
- 既存 CLI は `--headless` で完全互換

### Plan 4-G2: viewer overlay (~1 週)

- egui-wgpu を既存 D3D11 swapchain と合成(F3)
- ESC キー / hover でのトグル
- LatencyProbe snapshot を直接表示、ボリュームコントロール(Phase 3b audio)

### Plan 4-G3: tray + auto-start (~1 週)

- `tray-icon` 統合(F4)
- Windows スタートアップ登録(HKCU\Software\Microsoft\Windows\CurrentVersion\Run)

### Plan 4-G4: MSI + 自動更新 (~2 週)

- `cargo-wix` で MSI 生成(F6)
- `self_update` で GitHub Releases から取得(F5)
- アイコン、バージョンリソース

### Plan 4-G5: crash reporter + 署名 (~1 週)

- panic_hook + レポート UI(F7)
- Authenticode 署名環境整備(証明書選定は別タスク、Phase 5 側で購入判断でも可)

### Plan 4-G6: 多言語化 (~1 週)

- `fluent` crate で日本語 + 英語の 2 言語
- 言語ファイルは `%APPDATA%\prdt\locale\*.ftl`(上書き可能)

合計見積もり: 8 週(~2 ヶ月)。実装者 1 名・1 日 4〜6 時間ペース前提。

---

## Exit Criteria

- [ ] `prdt-setup-vX.Y.Z.msi` が Windows 10/11 クリーンインストール環境で起動しインストール完了
- [ ] host GUI で「待ち受け開始」ボタン → viewer GUI でランチャーから接続 → 画面が出る、までが **1 分以内、CLI 操作なし**
- [ ] 接続中の viewer に overlay が表示され、latency p50/p95 が毎秒更新される
- [ ] tray からホストを stop/start できる
- [ ] 自動更新で新バージョンが降ってきて適用される(Release を 1 つ切って手動検証)
- [ ] `--headless` フラグ経由で既存 CLI とバイナリ互換(既存の smoke test 全 pass)

---

## Open Questions (Phase 4 着手前に決めたいこと)

1. **配布チャネル**: GitHub Releases のみ? WinGet も? Store は Phase 5?
2. **証明書**: self-signed で公開 → ユーザーに SmartScreen 突破させる? OV/EV 買う?
3. **i18n の扱い**: 日本語 + 英語のみで Phase 4 を切るか、他言語のコミュニティ翻訳も受け付けるか
4. **クラッシュレポート送信先**: 独自サーバ? GitHub Issues 自動起票? Sentry? (PII 検討必要)
5. **viewer overlay**: egui-wgpu 合成か、自前で D3D11 shader 書くか。前者推奨だが wgpu 依存が増える
6. **host をサービスとして登録**できるようにするか(Windows Service、ログアウト後も稼働)— Phase 4 か Phase 5 か

---

## Notes

- Phase 4 はユーザーの「プロダクトとして触れる」体験を作る最初のフェーズ。ベンチマークは既に Phase 0 で合格済みなので、ここでの遅延は致命的ではないが、オーバーレイ描画が既存 swapchain を妨害しないことは測定する(`prdt-latency-bench --mode full-pipeline-win` を Phase 4 後にも回して比較)。
- egui を選んだのは速度と winit 共存のため。将来 Slint に差し替える場合、F1〜F3 の状態モデルは framework 非依存に保つ(MVVM 的)。
