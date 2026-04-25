# Phase 4 G3 — System Tray + Auto-Start (Windows) Design

**Project**: power-remote-dt
**Phase**: 4 (GUI + 配布)、サブプラン G3
**Date**: 2026-04-25
**Status**: Draft (built on `phase4-g2-complete` master)
**Parent spec**: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`

---

## Summary

`prdt-host` GUI に Windows タスクトレイ常駐機能を追加する。アイコンは listening / idle / error の 3 状態。右クリックメニューから設定 / 停止 / ログ表示 / 終了が呼べる。新規接続・切断・エラーは OS 通知(トースト)で知らせる。G1 で構造体にだけ用意した `Config.host.auto_start` を実装し、ON にすると Windows ログイン時にホストが自動起動する(`HKCU\...\Run` レジストリ)。

到達目標: G3 終了時点で、ホスト GUI を最小化したらタスクトレイに収まり、ウィンドウを再表示できる。再起動後、auto-start ON ならログインで自動的に立ち上がる。

---

## Scope

### In-scope (G3 — Windows のみ)

- 新クレート: なし(`gui-host` 内に `tray.rs` / `autostart.rs` モジュール追加)
- 新 dep: `tray-icon` 0.14、`notify-rust` 4.x、`winreg` 0.52
- アイコンアセット: `crates/gui-host/assets/tray-{idle,listening,error}.png` 16×16 + 32×32(ImageMagick / Photoshop / Inkscape で作成、後述「アイコンソース」)
- gui-host の `HostApp`:
  - tray icon を起動時に作成、host 状態(Idle/Listening/Error)に応じてアイコンを切替
  - 「Hide to tray」挙動:ウィンドウクローズ要求(`x` ボタン押下)→ クローズ抑止 + ウィンドウ非表示
  - tray 右クリック → メニュー(`Open settings` / `Stop listening` / `Show logs` / `Quit`)
  - Quit のみが本当に終了させる(viewer 接続中なら確認ダイアログ → 既定は継続)
- 通知:
  - 接続成立(Listening 中に新規 viewer 接続)→ 「Viewer connected from <addr>」
  - 切断(viewer disconnect)→ 「Viewer disconnected」
  - エラー(host main loop 異常終了)→ 「Host stopped: <error>」
  - 通知は OS デフォルトレベル、ユーザー操作不要(自動消える)
- Auto-start (Windows):
  - `Config.host.auto_start = true` で `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\PrdtHost` に現在の `current_exe()` パスを書き込み
  - Settings に「Auto-start on login」チェックボックス、トグルで registry 書き換え + Config 保存
  - false に戻したら同 registry 値を削除
- i18n: `prdt-gui-common` の `en.ftl` / `ja.ftl` に 13-15 個の `tray-*` / `notif-*` ID 追加
- テスト:
  - `autostart::set_enabled(true)` → `is_enabled()` が true、registry に値が書かれている
  - `autostart::set_enabled(false)` → `is_enabled()` が false、registry 値が消える
  - tray icon 状態切替の状態遷移ロジック(`HostState` → アイコン path)を関数化して unit test
  - notification 抑制(spam 防止)— 同じイベントが 1 秒以内に複数回来たら 1 回にまとめる

### Out (G4+ / 別 Phase)

- Linux / macOS の auto-start(Phase 1 / Phase 5+ で実装)
- gui-viewer 側の tray(viewer は短命プロセスなので不要、parent spec 範囲外)
- 通知の click 動作(クリックでメインウィンドウを前面に出す等、G3 では未対応)
- 通知履歴 / 通知センター
- バルーンチップ vs トースト切替(現代 Windows なら ToastXml 一択、`notify-rust` は内部で適切に分岐)
- tray icon の dark/light モード追従(Windows 11 だとトレイ背景が変わるが、半透明 PNG 1 枚で許容)
- 「最小化したらタスクバーから消す」(タスクバー残し + tray 追加が標準的、minimize-to-tray は別オプション、G5+)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| ライブラリ | `tray-icon` 0.14 | crates.io でメンテナンス継続、winit と event loop 統合可能、Windows/Linux/macOS 同 API |
| 通知ライブラリ | `notify-rust` 4.x | 全 OS 抽象化、Windows 上は ToastXml 経由、依存軽量 |
| Registry 操作 | `winreg` 0.52 | 純 Rust、Windows 専用クレート(他 OS でビルド時に `cfg(windows)` で除外) |
| アイコン形式 | PNG 16×16 / 32×32(`tray-icon` の `Icon::from_rgba`)| ベクター形式は tray-icon が要求しない、PNG2RGBA 変換は `image` crate で起動時に 1 回 |
| Auto-start scope | HKCU(per-user)| 管理者権限不要、複数ユーザーが同一 PC で個別設定可 |
| Auto-start key 名 | `PrdtHost` | プロダクト名一意、変名する場合は古い key の cleanup を `set_enabled(false)` でカバー |
| Auto-start 引数 | `--headless` を付けて起動(GUI を出さずバックグラウンド常駐)| ログイン時に GUI 飛び出しは UX 悪い、ユーザーが必要時にトレイから Open すれば良い |
| 通知 backoff | 同種イベント 1 秒以内に複数発生したら最後の 1 回だけ表示 | スパム抑制、シンプルな debounce |
| HostApp + tray の同期 | `Arc<Mutex<HostStatus>>` を tray コードと共有、tray メニュー click → HostApp の状態フラグ更新 | 既存 G1 supervisor 手法を踏襲 |
| Hide-to-tray 挙動 | eframe の `viewport_close_requested` を捕捉し `viewport_close_visible(false)` で非表示。Quit メニューでのみ本物のクローズ | Windows 慣例、`x` で完全終了は誤操作リスク |
| Quit 時の確認 | 接続中(`HostState::Listening` && peers > 0)時のみ「本当に終了?」ダイアログ、それ以外は即終了 | UX 慣例 |

---

## Architecture

### モジュール / ファイル配置

```
crates/gui-host/
  Cargo.toml                    + tray-icon, notify-rust, winreg, image
  assets/
    tray-idle.png               16x16 + 32x32 PNG (1 ファイルに 2 解像度ペアでも、または別ファイル)
    tray-listening.png
    tray-error.png
  src/
    tray.rs                     (新) TrayController, アイコン load, メニュー構築, event 処理
    autostart.rs                (新) set_enabled / is_enabled (Windows registry)
    notif.rs                    (新) notify_connected / notify_disconnected / notify_error + debounce
    app.rs                      tray ハンドリング統合 (close_requested, menu events)
    settings.rs                 + Auto-start checkbox 行 (autostart::set_enabled 呼び出し)
    lib.rs                      mod 宣言追加

crates/gui-common/locales/
  en/main.ftl                   + tray-*, notif-*, settings-autostart-* IDs (13-15個)
  ja/main.ftl                   同
```

### Tray controller のライフサイクル

```rust
// gui-host::tray
pub struct TrayController {
    icon: TrayIcon,
    menu_channel: MenuEventReceiver,
    icon_idle: Icon,
    icon_listening: Icon,
    icon_error: Icon,
}

impl TrayController {
    pub fn new() -> Result<Self> { ... }       // 起動時 1 回
    pub fn set_state(&self, state: HostState) { ... }  // tray icon 切替
    pub fn poll_menu(&self) -> Option<TrayAction> { ... }  // 毎フレーム呼ぶ、event をドレイン
}

pub enum TrayAction {
    OpenSettings,
    StopListening,
    ShowLogs,
    Quit,
}
```

`HostApp::update` 内で `self.tray.poll_menu()` → 該当アクション処理:
- `OpenSettings` → `self.settings_open = true`
- `StopListening` → `self.stop_listening()` (既存 G1)
- `ShowLogs` → ログファイルパスを explorer で開く / または log tail パネルにフォーカス
- `Quit` → 確認ダイアログ → `frame.close()` 相当の終了 flag

### Hide-to-tray

eframe 0.28 では `Viewport::CloseRequested` を捕捉して `ViewportCommand::CancelClose` + `ViewportCommand::Visible(false)` で対応。tray メニューの `Open` または ウィンドウクリック等で `Visible(true)` で再表示。

### Notification debouncer

```rust
// gui-host::notif
pub struct Notifier {
    last_kind: Option<NotifKind>,
    last_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifKind { Connected, Disconnected, Error }

impl Notifier {
    pub fn fire(&mut self, kind: NotifKind, body: &str) {
        if self.last_kind == Some(kind)
            && self.last_at.elapsed() < Duration::from_secs(1)
        {
            return; // dedupe
        }
        let _ = notify_rust::Notification::new()
            .summary(t!("notif-host"))
            .body(body)
            .show();
        self.last_kind = Some(kind);
        self.last_at = Instant::now();
    }
}
```

### Auto-start (Windows)

```rust
// gui-host::autostart
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "PrdtHost";

#[cfg(windows)]
pub fn set_enabled(on: bool) -> std::io::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(RUN_KEY)?;
    if on {
        let exe = std::env::current_exe()?;
        // Quote the path to handle spaces, then append --headless.
        let cmd = format!("\"{}\" --headless", exe.display());
        key.set_value(VALUE_NAME, &cmd)?;
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn is_enabled() -> bool {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(VALUE_NAME))
        .is_ok()
}

#[cfg(not(windows))]
pub fn set_enabled(_on: bool) -> std::io::Result<()> { Ok(()) }
#[cfg(not(windows))]
pub fn is_enabled() -> bool { false }
```

### Settings UI (G1 既存 modal に行追加)

```
┌─ Settings ───────────────────────────────────────────┐
│  Bind:        [ 0.0.0.0:9000 ]                       │
│  ... (既存) ...                                       │
│                                                       │
│  Auto-start on login: [☑ enabled]                    │  ← G3 新規
│                                                       │
│  Language: [Auto / English / 日本語]                  │  ← G6 既存
│                                                       │
│      [ Cancel ]      [ Save ]                        │
└──────────────────────────────────────────────────────┘
```

Save ボタン → `autostart::set_enabled(local.host.auto_start)` を呼んでから config 保存。Cancel は何もしない。

### アイコンソース

3 つの PNG(各 16×16 + 32×32):

- `tray-idle.png`: グレースケールロゴ(おとなしい、Idle 状態)
- `tray-listening.png`: 緑のドット入りロゴ(Listening、active)
- `tray-error.png`: 赤いドット入りロゴ(エラー状態)

ビルド時に `image` crate で PNG → RGBA bytes へ変換、`tray-icon::Icon::from_rgba(bytes, w, h)` に渡す。

PNG ファイル自体は人間がアートワークを作って commit する。G3 では:

- 開発中は仮アイコン(緑/グレー/赤の単色四角 16×16)を `image` crate で生成して `assets/` に書き出す `build.rs` を入れる
- Phase 4 G4 / G5 でちゃんとしたデザインに差し替え

---

## Testing Strategy

### 1. `autostart` unit tests (Windows のみ実行)

```rust
#[cfg(windows)]
#[test]
fn enable_then_disable_round_trip() {
    // Caveat: this writes to the real HKCU\...\Run\PrdtHost. Acceptable
    // for dev machines, harmful in CI with persistent state. Gate with
    // an env var so CI can opt-in only on a clean container.
    if std::env::var("PRDT_TEST_AUTOSTART").is_err() {
        eprintln!("skipping: PRDT_TEST_AUTOSTART not set");
        return;
    }
    autostart::set_enabled(true).unwrap();
    assert!(autostart::is_enabled());
    autostart::set_enabled(false).unwrap();
    assert!(!autostart::is_enabled());
}
```

### 2. `notif::Notifier` unit test (debounce)

```rust
#[test]
fn debounce_swallows_repeats_within_one_second() {
    let mut n = Notifier::new_for_test();
    let count_before = n.test_fire_count;
    n.fire(NotifKind::Connected, "x");
    n.fire(NotifKind::Connected, "y");
    n.fire(NotifKind::Connected, "z");
    assert_eq!(n.test_fire_count - count_before, 1);
}

#[test]
fn different_kinds_do_not_dedupe() {
    let mut n = Notifier::new_for_test();
    n.fire(NotifKind::Connected, "x");
    n.fire(NotifKind::Error, "y");
    assert_eq!(n.test_fire_count, 2);
}
```

(Test build feature `test_fire` で実 OS 通知を出さずカウンタを増やすだけのモードにする。)

### 3. `tray` icon state mapping unit test

```rust
#[test]
fn host_state_maps_to_icon() {
    assert_eq!(icon_path_for_state(HostState::Idle), "tray-idle.png");
    assert_eq!(icon_path_for_state(HostState::Listening), "tray-listening.png");
    // Error state mapping if added to HostState
}
```

### 4. 手動 smoke

- host GUI 起動 → tray icon が表示される(Idle 状態)
- 「Start listening」押下 → アイコン緑(Listening)
- viewer 接続 → 通知ポップアップ「Viewer connected from ...」
- viewer 切断 → 通知「Viewer disconnected」
- ウィンドウ `x` → ウィンドウ消える、tray icon 残る
- tray 右クリック → メニュー → Open settings → 設定モーダル開く
- tray Quit → 終了
- Settings → Auto-start on login: ON → 保存 → `regedit` で `HKCU\...\Run\PrdtHost` 確認 → reboot 後にログイン → host が `--headless` で常駐(GUI 出ない)→ tray icon 表示確認

### 5. Workspace tests

`cargo test --workspace` で 249 既存 + 5-7 新規 = 256 程度パス。
clippy `--workspace --all-targets --all-features -- -D warnings` clean。

---

## Exit Criteria

- [ ] `crates/gui-host/Cargo.toml` に tray-icon / notify-rust / winreg / image を追加
- [ ] `crates/gui-host/assets/tray-{idle,listening,error}.png` 配置(仮アイコン可)
- [ ] `gui-host::tray` モジュール実装
- [ ] `gui-host::autostart` モジュール実装(Windows のみ実体、他 OS は no-op)
- [ ] `gui-host::notif` モジュール実装 + debounce test
- [ ] `HostApp` が tray を持つ、状態に応じてアイコン更新
- [ ] ウィンドウ `x` ボタンで hide-to-tray
- [ ] tray メニュー 4 項目すべて動作
- [ ] viewer 接続/切断/エラーで通知発火
- [ ] Settings に Auto-start チェックボックス追加、ON で registry 書き込み
- [ ] i18n IDs 13-15 個 (`tray-*`, `notif-*`, `settings-autostart-*`) を en + ja に追加
- [ ] workspace tests pass、clippy clean
- [ ] git tag `phase4-g3-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| `tray-icon` crate の event loop 統合が eframe と衝突 | tray クリック検出失敗 | 起動時に `tray-icon` を初期化 + `MenuEvent::receiver()` を `Arc<Mutex<...>>` で持ち、HostApp::update で polling。winit イベントループ統合は不要(channels だけ使う) |
| Auto-start で `current_exe()` が build dir を返す状況(`cargo run` で trail を有効化等) | reboot 後に古い path を実行、エラー | UX 上「dev mode で auto-start を切るな」というドキュメンテーション、registry に古い path が残ったら次回 GUI 起動時に上書き保存される |
| Windows Defender SmartScreen が `--headless` 起動を疑う | ログイン時に警告ポップ | コード署名(G5)で解消、G3 では unsigned 起動でも reg key には書ける |
| Hide-to-tray + ウィンドウ閉じが Windows 11 で `viewport_close` セマンティクス変更 | クローズが効かない | eframe 0.28 docs 参照、見つかった API 限定で実装。失敗時は close 抑制 + warn ログ |
| 通知ライブラリ `notify-rust` が古い Windows で動かない | 通知 silent fail | エラー無視、log 出すだけ。本機能は補助的 |
| 仮アイコン PNG が拙いと UX 悪い | 見た目印象 | G3 では仮アイコン許容、G4 / G5 で正式デザインに差し替え |
| HKCU registry 削除が NotFound でエラー扱い | enable/disable トグルでエラー表示 | spec の `set_enabled(false)` で NotFound を Ok 扱いにする実装(上記コード) |

---

## Open Questions(実装中に決めてよい)

- 通知ハンドラの threading(`notify-rust::Notification::show()` は blocking?)— 仕様上 fire-and-forget、blocking なら spawn_blocking 任せ
- Quit 時の「接続中ですが本当に終了?」ダイアログを G3 で入れるか G3.5 か — G3 では simple Quit、確認なしで OK(YAGNI、UX 改善は G3.5+)
- tray icon の tooltip 内容("PrdtHost - listening on 0.0.0.0:9000" 等)— 静的 "PrdtHost" のみで G3
- アイコン作成方法:`build.rs` で生成 vs 静的コミット — 静的コミット(再現性)

---

## References

- 親 spec: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(F4 system tray)
- G1 spec: `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`(`Config.host.auto_start` 既存 field)
- G6 spec: `docs/superpowers/specs/2026-04-25-phase4-g6-i18n-design.md`(i18n 仕組み再利用)
- `tray-icon`: <https://docs.rs/tray-icon/0.14/>
- `notify-rust`: <https://docs.rs/notify-rust/4>
- `winreg`: <https://docs.rs/winreg/0.52>
