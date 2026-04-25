# Phase 4 G4 — MSI Installer + Auto-Update Design

**Project**: power-remote-dt
**Phase**: 4 (GUI + 配布)、サブプラン G4
**Date**: 2026-04-25
**Status**: Draft (built on `phase4-g3-complete` master after merge)
**Parent spec**: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`

---

## Summary

Phase 4 で構築した GUI 群を、Windows のエンドユーザーが普通の MSI でインストールできる形にする。インストール先は `%LOCALAPPDATA%\prdt\`(per-user、UAC 不要)。スタートメニューにショートカット、デスクトップショートカットは optional。インストール完了後、`prdt-host` 起動時に GitHub Releases を週 1 回チェック、新版があればダウンロード → in-place 上書き → 再起動を促す。

到達目標: G4 終了時点で、別の Windows 10/11 マシンに `prdt-setup-vX.Y.Z.msi` を渡してダブルクリックでインストール完了し、スタートメニューから host / viewer を起動できる。新バージョンを GitHub に release すると、稼働中の host から「Update available」通知が出る。

---

## Scope

### In-scope (G4 — Windows のみ)

- ビルドツール: `cargo-wix`(WiX Toolset 経由 MSI 生成)
- 構成ファイル: `wix/main.wxs`(WiX XML テンプレ)
- インストール内容:
  - `prdt-host.exe`、`prdt-viewer.exe`、`prdt-viewer-overlay.exe` を `%LOCALAPPDATA%\prdt\bin\` に配置
  - アイコン `.ico`(G3 の PNG から `image` で生成、または手動作成)
  - スタートメニューショートカット: `Power Remote Desktop (Host)` / `Power Remote Desktop (Viewer)`
  - インストーラオプション: 「デスクトップにショートカットを作成」チェックボックス(default off)
  - アンインストール: registry / config.toml / known-hosts / host-key.bin は **残す**(再インストール時に設定が消えると不便)
- バージョンリソース:
  - `Cargo.toml` workspace `version` から MSI バージョン文字列を生成
  - `winres` か `embed-resource` でホスト/ビューアー .exe に Windows バージョンリソース埋め込み(プロパティで「version: 0.0.1」と表示)
- 自動更新:
  - `self_update` 0.41 + GitHub Releases バックエンド
  - `prdt-host` 起動時に「最後にチェックした時刻」を読んで、7 日以上経っていれば async でチェック(ブロックしない)
  - 新版があれば tray notification + Settings に「Update available: vX.Y.Z [Install]」行
  - Install 押下 → `self_update` で MSI ダウンロード → 起動した MSI に処理を委譲 → 自身は終了
  - チャネル: `stable` のみ(beta 等は G5+)
- アイコン: `prdt-icon.ico`(256×256 + 128×128 + 64×64 + 32×32 + 16×16 のマルチ解像度)
- i18n: 自動更新 UI 用 ID を gui-common に追加(`update-available`、`update-button-install`、`update-checking`、`update-up-to-date`、`update-error`)
- テスト:
  - `update::compare_versions("0.0.1", "0.0.2")` → newer
  - `update::compare_versions("0.0.2", "0.0.1")` → older
  - `update::should_check_now(last_checked: SystemTime, interval_days: u32)` → 真偽
  - 統合テスト:仮の GitHub Releases モック(or `httpmock`)で「新バージョンあり → DownloadUrl 取得」をエンドツーエンド確認
- ビルド手順ドキュメント: `docs/build-msi.md`(WiX Toolset インストール、`cargo wix init` / `cargo wix` 実行手順)
- CI: 別タスク(parent spec で Phase 5 想定)、G4 ではローカルビルド手順のみ

### Out (G5+ / 別 Phase)

- Authenticode コード署名(G5、cert 購入が必要)
- WinGet パッケージ登録(Phase 5)
- Windows Store 提出(Phase 5)
- Linux .deb / .rpm / Flatpak(Phase 1+ で別途)
- macOS .pkg / .dmg(Phase 5+)
- 差分更新(delta patch)— 初期は full binary replace のみ
- 自動更新の rollback(失敗時に旧版に戻す)— Phase 5+ 検討
- ベータチャネル / nightly チャネル
- アンインストール時の鍵 / 接続履歴削除オプション
- per-machine インストール(管理者権限必須、企業展開向け、Phase 5+)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| インストーラ生成 | `cargo-wix` 0.3 | 公式公認、WiX 3.x 経由、Cargo workspace と相性良い |
| WiX バージョン | WiX Toolset 3.14 | LTS、`cargo-wix` のデフォルト互換 |
| インストール scope | per-user(`%LOCALAPPDATA%\prdt\`)| UAC 不要、企業 PC 等の制限ユーザーでも導入可能 |
| アンインストール挙動 | プログラム削除のみ、設定/鍵/履歴は残す | 再インストール時の継続性、ユーザーは別途手動削除可能 |
| 自動更新ライブラリ | `self_update` 0.41 | GitHub Releases ネイティブ対応、shasum 検証あり |
| 更新チェック頻度 | 7 日 + 起動時 1 回 / Settings から手動チェック | バランス、毎起動チェックは UX うるさい |
| 更新インストール方式 | 新 MSI ダウンロード → `msiexec /i ... /qb /norestart` 起動 → 旧 process 終了 | MSI が in-place upgrade、設定は維持 |
| バージョン番号付与 | `Cargo.toml` workspace `version`、ビルド時に WiX に渡す | DRY、リリース時 1 箇所変更 |
| バージョンリソース | `winres` 0.1.x で .exe に埋め込み | エクスプローラーのプロパティ表示で正規アプリらしさ |
| ショートカット名 | i18n 化しない、固定英語 | MSI に i18n 入れるのは複雑、英語名で統一 |
| アイコン .ico 作成 | G4 の `build.rs` で `image` crate を使って PNG → ICO | 別ツール不要、再現性 |
| アイコン PNG | G3 のトレイ用 PNG(64×64 もしくは独自に 256×256 作成) | アセット集約、G3 のロゴデザインを継承 |
| 配布チャネル | GitHub Releases 公開、tag = `vX.Y.Z` | parent spec 確定 |
| MSI ファイル名 | `prdt-setup-vX.Y.Z.msi`(`prdt-setup-` プレフィックス + workspace version) | 一貫性 |
| 「新バージョン通知」UI | Settings の上部に薄黄色の banner、tray notification は G3 の `notify-rust` 経由で 1 回だけ | しつこくない、Settings を開けば常時見える |
| エラー時 fallback | 自動更新失敗時は warn ログ + Settings に再試行ボタン | サイレント失敗を避ける |

---

## Architecture

### モジュール / ファイル配置

```
wix/
  main.wxs                       新規 — WiX XML テンプレ(`cargo wix init` で雛形生成、編集)
  License.rtf                    新規 — MIT or GPL ライセンス本文 RTF
  
crates/gui-host/
  Cargo.toml                     + self_update, semver, ureq (transitive), build = "build.rs"
  build.rs                       新規 — winres でバージョンリソース埋め込み
  resources/
    prdt-icon.ico                G4 で生成・コミット
  src/
    update.rs                    新規 — UpdateChecker, async check(),
                                  compare_versions, should_check_now
    settings.rs                  Update banner / "Check for updates" ボタン追加

crates/gui-viewer/
  Cargo.toml                     + winres build dep + build.rs (バージョンリソースのみ)
  build.rs                       新規

crates/viewer-overlay/
  Cargo.toml                     + winres build dep + build.rs
  build.rs                       新規

crates/gui-common/locales/
  en/main.ftl                    + update-* IDs (5個)
  ja/main.ftl                    同

Cargo.toml (workspace)
  [workspace.metadata.wix]       + cargo-wix 設定 (output dir, etc.)

docs/
  build-msi.md                   新規 — WiX 環境構築 + ビルド実行手順
```

### MSI 生成フロー

```
cargo build --release
  → target/release/{prdt-host.exe, prdt-viewer.exe, prdt-viewer-overlay.exe}
  → build.rs が winres でバージョンリソース埋め込み済

cargo wix
  → wix/main.wxs を WiX 3.14 で読んで MSI 生成
  → 出力: target/wix/prdt-setup-vX.Y.Z.msi
```

### `wix/main.wxs` 概要

```xml
<?xml version='1.0' encoding='windows-1252'?>
<Wix xmlns='http://schemas.microsoft.com/wix/2006/wi'>
  <Product Id='*' Name='Power Remote Desktop' UpgradeCode='<GUID>'
           Language='1033' Codepage='1252' Version='$(var.Version)'
           Manufacturer='power-remote-dt'>
    <Package Id='*' Keywords='Installer' Description='Power Remote Desktop'
             Manufacturer='power-remote-dt' InstallerVersion='450'
             Languages='1033' Compressed='yes' SummaryCodepage='1252'
             InstallScope='perUser' />

    <Media Id='1' Cabinet='media1.cab' EmbedCab='yes' />

    <Directory Id='TARGETDIR' Name='SourceDir'>
      <Directory Id='LocalAppDataFolder'>
        <Directory Id='AppRoot' Name='prdt'>
          <Directory Id='APPLICATIONFOLDER' Name='bin'>
            <Component Id='HostExe' Guid='*'>
              <File Id='HostExe' Source='target/release/prdt-host.exe'
                    KeyPath='yes' Checksum='yes' />
            </Component>
            <Component Id='ViewerExe' Guid='*'>
              <File Id='ViewerExe' Source='target/release/prdt-viewer.exe'
                    KeyPath='yes' Checksum='yes' />
            </Component>
            <Component Id='OverlayExe' Guid='*'>
              <File Id='OverlayExe' Source='target/release/prdt-viewer-overlay.exe'
                    KeyPath='yes' Checksum='yes' />
            </Component>
          </Directory>
        </Directory>
      </Directory>

      <Directory Id='ProgramMenuFolder'>
        <Directory Id='AppShortcutFolder' Name='Power Remote Desktop'>
          <Component Id='HostShortcut' Guid='*'>
            <Shortcut Id='HostShortcut' Name='Host' Description='Run as host'
                      Target='[APPLICATIONFOLDER]prdt-host.exe'
                      WorkingDirectory='APPLICATIONFOLDER'
                      Icon='AppIcon.ico' IconIndex='0' />
            <RemoveFolder Id='AppShortcutFolder' On='uninstall' />
            <RegistryValue Root='HKCU' Key='Software\prdt\Installed'
                           Name='installed' Type='integer' Value='1'
                           KeyPath='yes' />
          </Component>
          <Component Id='ViewerShortcut' Guid='*'>
            <Shortcut Id='ViewerShortcut' Name='Viewer'
                      Target='[APPLICATIONFOLDER]prdt-viewer.exe'
                      WorkingDirectory='APPLICATIONFOLDER'
                      Icon='AppIcon.ico' IconIndex='0' />
            <RegistryValue Root='HKCU' Key='Software\prdt\Installed'
                           Name='viewer' Type='integer' Value='1'
                           KeyPath='yes' />
          </Component>
        </Directory>
      </Directory>
    </Directory>

    <Feature Id='Default' Title='Power Remote Desktop' Level='1'>
      <ComponentRef Id='HostExe' />
      <ComponentRef Id='ViewerExe' />
      <ComponentRef Id='OverlayExe' />
      <ComponentRef Id='HostShortcut' />
      <ComponentRef Id='ViewerShortcut' />
    </Feature>

    <Icon Id='AppIcon.ico' SourceFile='crates/gui-host/resources/prdt-icon.ico' />
    <Property Id='ARPPRODUCTICON' Value='AppIcon.ico' />

    <UIRef Id='WixUI_InstallDir' />
    <UIRef Id='WixUI_ErrorProgressText' />

    <WixVariable Id='WixUILicenseRtf' Value='wix/License.rtf' />
  </Product>
</Wix>
```

`UpgradeCode` は固定 GUID(初回 `cargo wix init` で生成、変更しない。アップグレード判定キー)。`Product Id='*'` は毎リリース新 GUID(自動)。

### `self_update` 統合(gui-host のみ)

```rust
// gui-host::update
use std::time::{Duration, SystemTime};

pub struct UpdateState {
    pub last_checked: Option<SystemTime>,
    pub latest_release: Option<self_update::update::Release>,
    pub status: CheckStatus,
}

pub enum CheckStatus {
    Idle,
    Checking,
    UpToDate,
    Available(String),  // version
    Error(String),
}

pub fn should_check_now(last: Option<SystemTime>, interval_days: u32) -> bool {
    match last {
        None => true,
        Some(t) => SystemTime::now()
            .duration_since(t)
            .map(|d| d > Duration::from_secs(interval_days as u64 * 86_400))
            .unwrap_or(true),
    }
}

pub fn compare_versions(current: &str, latest: &str) -> Option<std::cmp::Ordering> {
    use semver::Version;
    let c = Version::parse(current.trim_start_matches('v')).ok()?;
    let l = Version::parse(latest.trim_start_matches('v')).ok()?;
    Some(c.cmp(&l))
}

pub async fn check_async() -> CheckStatus {
    // Run self_update::backends::github::Update::configure() in spawn_blocking
    // (it does sync HTTP). Compare current_version vs release.version.
    let result = tokio::task::spawn_blocking(|| {
        self_update::backends::github::Update::configure()
            .repo_owner("power-remote-dt")
            .repo_name("power-remote-dt")
            .bin_name("prdt-host")
            .current_version(env!("CARGO_PKG_VERSION"))
            .build()
    })
    .await;
    match result {
        Ok(Ok(updater)) => {
            // Get latest release
            match tokio::task::spawn_blocking(move || updater.get_latest_release()).await {
                Ok(Ok(rel)) => {
                    match compare_versions(env!("CARGO_PKG_VERSION"), &rel.version) {
                        Some(std::cmp::Ordering::Less) => CheckStatus::Available(rel.version),
                        _ => CheckStatus::UpToDate,
                    }
                }
                Ok(Err(e)) => CheckStatus::Error(format!("github: {e}")),
                Err(e) => CheckStatus::Error(format!("join: {e}")),
            }
        }
        Ok(Err(e)) => CheckStatus::Error(format!("configure: {e}")),
        Err(e) => CheckStatus::Error(format!("join: {e}")),
    }
}
```

### Settings UI

```
┌─ Settings ───────────────────────────────────────────┐
│  ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓   │  ← G4 banner
│  ┃ ⚠ Update available: v0.0.2  [Install]          ┃   │
│  ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛   │
│                                                       │
│  Bind:        [ 0.0.0.0:9000 ]                       │
│  ... (既存) ...                                       │
│  Auto-start: [ ☑ ]                                   │  ← G3
│  Language:   [ Auto ▼ ]                              │  ← G6
│                                                       │
│  Updates:                                             │  ← G4
│  Last checked: 2026-04-25 14:32                      │
│  [ Check for updates now ]                            │
│                                                       │
│      [ Cancel ]      [ Save ]                        │
└──────────────────────────────────────────────────────┘
```

「Install」押下 → `self_update::backends::github::Update::download_to_temp_dir()` で MSI ダウンロード → `Command::new("msiexec").args(["/i", path, "/qb", "/norestart"]).spawn()` → 自身プロセス終了。msiexec が in-place upgrade、UpgradeCode が同じなので 既存ファイル置換。

### バージョンリソース埋め込み

`build.rs` (各 GUI bin crate に同様のものを置く):

```rust
fn main() {
    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("../gui-host/resources/prdt-icon.ico");
        res.set("FileDescription", "Power Remote Desktop");
        res.set("ProductName", "Power Remote Desktop");
        // CARGO_PKG_VERSION 経由で workspace から拾う
        if let Err(e) = res.compile() {
            eprintln!("winres compile failed: {e}");
        }
    }
}
```

(viewer / viewer-overlay は別 build.rs で同様に設定。アイコン path は相対参照可。)

---

## Testing Strategy

### 1. `update::compare_versions` unit tests

```rust
#[test]
fn newer_version_is_greater() {
    let r = compare_versions("0.0.1", "0.0.2");
    assert_eq!(r, Some(std::cmp::Ordering::Less));
}

#[test]
fn older_version_is_lesser() {
    let r = compare_versions("0.0.2", "0.0.1");
    assert_eq!(r, Some(std::cmp::Ordering::Greater));
}

#[test]
fn equal_versions_compare_equal() {
    let r = compare_versions("0.0.1", "0.0.1");
    assert_eq!(r, Some(std::cmp::Ordering::Equal));
}

#[test]
fn v_prefix_tolerated() {
    let r = compare_versions("v0.1.0", "v0.2.0");
    assert_eq!(r, Some(std::cmp::Ordering::Less));
}

#[test]
fn invalid_version_returns_none() {
    assert_eq!(compare_versions("not.a.version", "0.0.1"), None);
}
```

### 2. `update::should_check_now` unit tests

```rust
#[test]
fn never_checked_should_check() {
    assert!(should_check_now(None, 7));
}

#[test]
fn just_checked_should_not_check() {
    let now = SystemTime::now();
    assert!(!should_check_now(Some(now), 7));
}

#[test]
fn old_check_should_check_again() {
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    assert!(should_check_now(Some(old), 7));
}
```

### 3. MSI ビルド smoke

`cargo wix` 実行 → `target/wix/prdt-setup-v0.0.1.msi` ができる → ファイルサイズが妥当(数十 MB、Noto Sans JP フォント込み)。clean Win10/11 VM でダブルクリックインストール → スタートメニューから host / viewer 起動可能 → アンインストールで `%LOCALAPPDATA%\prdt\bin\` が消える、設定は残る。

このテストはローカル / Phase 5 CI で手動実行、自動化は G4 で目指さない。

### 4. self_update 統合 smoke

GitHub に v0.0.1 / v0.0.2 を release(空ファイル含むダミーでも可)→ host を v0.0.1 ビルドで起動 → 「Check for updates」押下 → 「Available: v0.0.2」banner 表示 → 「Install」押下 → MSI ダウンロード → 起動。G4 では実 release を切るが、テストは手動。

### 5. workspace tests + clippy

`cargo test --workspace` で 5 新規テスト = 261 程度 pass。
clippy `--workspace --all-targets --all-features -- -D warnings` clean。

---

## Exit Criteria

- [ ] `wix/main.wxs` + `wix/License.rtf` 配置
- [ ] `crates/gui-host/resources/prdt-icon.ico` 作成
- [ ] `crates/{gui-host,gui-viewer,viewer-overlay}/build.rs` で winres バージョンリソース埋め込み
- [ ] `gui-host::update` モジュール実装(compare_versions / should_check_now / async check)
- [ ] Settings に Update banner + 手動「Check for updates」ボタン
- [ ] Update check は workspace `Cargo.toml` `version` を current 版として参照
- [ ] `cargo wix` で `target/wix/prdt-setup-v0.0.1.msi` がビルドできる
- [ ] MSI を実機 Win10/11 VM でインストール → 起動 → アンインストール のループ確認(手動)
- [ ] i18n IDs 5 個(`update-*`)を en + ja に追加
- [ ] `docs/build-msi.md` でビルド手順を文書化(WiX 3.14 インストール、`cargo wix init` を 1 度実行、`cargo wix` で MSI 生成)
- [ ] workspace tests pass(8-9 新規 update テスト含む)、clippy clean
- [ ] git tag `phase4-g4-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| WiX Toolset の手動インストール要求(`cargo-wix` は wix3.exe を呼ぶだけ) | 開発環境セットアップ手間 | `docs/build-msi.md` で WiX 3.14 ダウンロード手順を明記 |
| `winres` が build.rs から `rc.exe`(Windows SDK)を呼ぶ → SDK 未インストールだと失敗 | dev build 失敗 | build.rs 内で `if cfg!(target_os = "windows")` ガード + `compile()` の Err を warn にして fail させない(リソース無し binary も動く) |
| MSI UpgradeCode を変更してしまう | 同一プロダクトの新版が「別アプリ」扱いで 2 重インストール | spec で UpgradeCode を固定値 GUID として明記、変更禁止のコメント |
| `self_update` が GitHub API rate limit に当たる(unauthenticated 60/h) | 起動時 check 失敗続発 | エラーは warn のみ、UI には「Error: rate limit, retry later」表示。手動 check で同じエラーになるが許容 |
| アイコン .ico 作成不備で MSI でアイコン表示されない | UX 悪い | `image` crate で multi-resolution ICO 生成 + 手動確認 |
| `current_exe` が MSI 上書き中で悪さする | self_update 失敗 | self_update のドキュメント通り、新 MSI を msiexec に渡して旧プロセスを exit、msiexec が上書き |
| バージョン不一致(`Cargo.toml` 0.0.1 と GitHub release v0.0.2)でロジック取り違え | check が常に「Available」で連発 | spec で「workspace version とリリースタグは一致させる」運用ルール、`compare_versions` は同値で UpToDate |
| アンインストール時に config.toml も消したいユーザー | 不便な UX | アンインストーラに「Remove user data」チェックボックス追加(MSI WiX で `Property Id='REMOVE_USER_DATA'` + Custom Action)→ G4 では default 未実装(parent spec の Open Question)、再インストール継続性優先 |

---

## Open Questions(実装中に決めてよい)

- 自動アップデート check の実装場所:gui-host::update 専用 vs gui-common 共有 → gui-host 専用(viewer は短命なので update check 不要、parent spec 整合)
- `self_update` の SHA256 検証(GitHub Releases に shasum を置く)— G4 で標準対応、Releases に SHA256 ファイルを upload する運用
- ダウンロード進捗 UI — シンプルに「Installing…」表示のみ、進捗バーは G5+
- WiX ダイアログの言語 — 英語固定(parent spec 範囲外)
- バナーの dismiss(「now thanks」)— G4 では未対応、毎起動に出す。気になれば G5+

---

## References

- 親 spec: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(F5 auto-update, F6 MSI installer)
- G3 spec: `docs/superpowers/specs/2026-04-25-phase4-g3-tray-design.md`(`notify-rust` 経由の通知再利用)
- `cargo-wix`: <https://github.com/volks73/cargo-wix>
- `self_update`: <https://docs.rs/self_update/0.41>
- `winres`: <https://docs.rs/winres>
- WiX Toolset 3.14: <https://wixtoolset.org/releases/v3-14-0/>
