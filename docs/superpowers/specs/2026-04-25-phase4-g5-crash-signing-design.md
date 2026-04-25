# Phase 4 G5 — Crash Reporter + Code Signing Scaffolding Design

**Project**: power-remote-dt
**Phase**: 4 (GUI + 配布)、サブプラン G5
**Date**: 2026-04-25
**Status**: Draft (built on `phase4-g4-complete` master)
**Parent spec**: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`

---

## Summary

Phase 4 最後のサブプラン。3 つの GUI バイナリ(`prdt-host` / `prdt-viewer` / `prdt-viewer-overlay`)に `std::panic::set_hook` ベースのクラッシュレポータを入れ、`%LOCALAPPDATA%\prdt\crashes\YYYYMMDD-HHMMSS.json` にダンプを書き出す。`prdt-host` 起動時に未送信のクラッシュレポートを検出した場合、Settings に「前回のセッションでクラッシュしました — レポート送信」UI を出す(送信は手動コピー or 後続タスクで自動化)。Authenticode コード署名は **scaffolding のみ**:`scripts/sign-msi.ps1` と `docs/sign-and-release.md` を整備し、cert 購入後に即実行できる状態にする。Cert は購入しない(Phase 5 公開直前判断)。

到達目標: G5 完了で、Rust panic がいずれかの GUI バイナリで起きたら、ファイルに記録され、次回起動時にユーザーが認知できる。Authenticode 署名は cert を入れれば 1 コマンドで実行できる状態。これで Phase 4 全体が完了し、ベータリリース可能。

---

## Scope

### In-scope (G5)

- 新クレート: なし(`gui-common::crashlog` モジュール追加)
- 新 dep: `serde_json`(既存 workspace dep)、`chrono`(タイムスタンプフォーマット)
- `prdt_gui_common::crashlog` モジュール:
  - `pub fn install_panic_hook(binary_name: &str)`:
    - `std::panic::set_hook(...)` を呼んで panic 発生時に JSON ダンプ
    - 既存の tracing-subscriber 経由のログ出力は維持(panic は tracing にも出る)
    - JSON 形式: `{ binary, version, timestamp_iso, panic_message, panic_location, recent_log_lines }`
    - 保存先: `%LOCALAPPDATA%\prdt\crashes\YYYYMMDD-HHMMSS-<binary>.json`
  - `pub fn list_pending_crashes() -> Vec<CrashReport>`:
    - `crashes/` ディレクトリを読み、`CrashReport { path, timestamp, binary, message }` 配列を返す
    - 起動時に呼んで「未送信のクラッシュ」検出に使う
  - `pub fn mark_acknowledged(path: &Path) -> io::Result<()>`:
    - クラッシュファイルをサブディレクトリ `crashes/acknowledged/` に移動
    - ユーザーが「了解」or「送信完了」を押したときの後始末
- `prdt-host` / `prdt-viewer` / `prdt-viewer-overlay` の `main()` で `install_panic_hook(env!("CARGO_PKG_NAME"))` を呼ぶ
- `gui-host` の起動シーケンス(`run_host_gui`)で `list_pending_crashes()` を呼び、結果が空でなければ `HostApp` の `pending_crashes` フィールドに格納
- `HostApp` の Settings 画面に「Pending crash reports」セクション追加(数件あれば各クラッシュの timestamp / binary / first line of message 表示、`Open folder` ボタンで explorer 起動、`Acknowledge` ボタンで該当ファイルを `acknowledged/` 配下に移動)
- ログ tail 連携:既存の `TailHandle::snapshot()` を利用して `recent_log_lines` を取得(直近 50 行)
- panic_hook 内の `TailHandle` への安全アクセス:`OnceLock<TailHandle>` で初期化済みなら使う、未初期化なら空配列
- `scripts/sign-msi.ps1`:`signtool.exe` を呼ぶ PowerShell スクリプト。引数 `-CertPath`, `-CertPassword`, `-MsiPath`、`-TimestampUrl`(デフォルト `http://timestamp.digicert.com`)。署名 + タイムスタンプ + 検証(`signtool verify`)
- `docs/sign-and-release.md`:cert 取得手順(EV vs OV 比較表)、`scripts/sign-msi.ps1` の使い方、リリース前チェックリスト
- `docs/build-msi.md` に Sign step 追記("Sign the MSI"セクション)
- i18n: `crashlog-pending-heading`、`crashlog-button-open-folder`、`crashlog-button-acknowledge`、`crashlog-no-pending` の 4 ID
- テスト:
  - `crashlog::install_panic_hook` を install → `panic!` を含む `catch_unwind` → ファイルが書かれることを assert
  - `list_pending_crashes` がディレクトリの内容を timestamp 降順で返す
  - `mark_acknowledged` がファイルを `acknowledged/` 配下に移動

### Out (Phase 5+ / 別タスク)

- ネイティブ exception minidump(DXGI / NVENC の C0000005、`minidump-writer` 統合は別タスク)
- 自動送信(GitHub Issues / Sentry / 自前 endpoint)
- PII 検知 / マスキング
- クラッシュレポート集計 dashboard
- Authenticode 証明書の **購入**(EV: ~$300/年、OV: ~$100/年、Phase 5 公開判断)
- 実際の署名実行(cert がないので smoke できない、scaffolding のみ)
- `signtool` を build.rs から自動呼び出し(opt-in にする、cert 配置先は CI シークレット管理)
- viewer / overlay 側の Pending crashes UI(host が supervisor 役を担う、viewer は再起動毎に host が UI を出す)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| panic_hook 実装場所 | `gui-common::crashlog`(共通) | 3 binary で同じロジック、DRY |
| 保存先 | `dirs::cache_dir()/prdt/crashes/`(`%LOCALAPPDATA%\prdt\Caches\prdt\crashes` 等) | 既存パス慣例、ユーザー権限のみ |
| ファイル名 | `YYYYMMDD-HHMMSS-<binary>-<pid>.json` | 名前衝突防止、降順ソート可能、読みやすい |
| JSON フォーマット | `{ binary, version, timestamp_iso, panic_message, panic_location, recent_log_lines: [String] }` | デバッグに必要十分、PII 自動マスキング無し(ローカル保存のみなので問題小) |
| recent_log_lines の取得 | `OnceLock<TailHandle>` 経由 | gui-host は G1 で TailLayer 設置済、viewer/overlay は未設置 → 空配列で OK |
| timestamp ライブラリ | `chrono` 0.4 | 既存依存可能性高い、`time` crate でもよいが ISO8601 出力が `chrono` 直 |
| binary 識別 | `env!("CARGO_PKG_NAME")` でビルド時に埋め込み | 動的 args 解析より確実 |
| 起動時のクラッシュ検出 | gui-host の `run_host_gui` 内で `list_pending_crashes()`、結果を `HostApp::pending_crashes` に格納 | viewer は launcher が短命、host が常駐なので host で UI |
| Pending UI 配置 | Settings 内の最初の section(Update banner と並ぶ位置) | UX:Settings を開く動機を兼ねる |
| Acknowledge 後の挙動 | ファイルを `acknowledged/` サブディレクトリに移動(削除しない) | ユーザーが必要なときに参照可能、削除はユーザー手動 |
| 署名スクリプト言語 | PowerShell(`.ps1`) | Windows 標準、`signtool.exe` 呼び出しに最適 |
| 署名コマンドフォーマット | `signtool sign /f $cert /p $pass /t $ts /td sha256 /fd sha256 /v $msi` + `signtool verify /pa /v $msi` | SHA256 タイムスタンプ + verify、業界標準 |
| 署名対象 | MSI のみ(各 .exe は MSI に同梱されるが、別途 `signtool sign` も可能 — G5 では MSI のみ、後で .exe 個別署名は cert 購入後判断) | 最小スコープ |
| Tag 後 release 公開順 | tag → cert 取得 → 署名 → 公開、または unsigned で先行公開も可 | 運用判断、scaffolding は両対応 |

---

## Architecture

### モジュール / ファイル配置

```
crates/gui-common/
  Cargo.toml                + chrono workspace dep
  src/
    crashlog.rs             (新) install_panic_hook / list_pending_crashes / mark_acknowledged
                            + CrashReport struct + 直近 N 行取得 helper
    lib.rs                  + pub mod crashlog + re-exports

crates/host/src/main.rs     + crashlog::install_panic_hook("prdt-host")
                            (既存 panic_hook の置換、tracing 出力との二重化を整理)
crates/viewer/src/main.rs   + crashlog::install_panic_hook("prdt-viewer")
crates/viewer-overlay/src/main.rs
                            + crashlog::install_panic_hook("prdt-viewer-overlay")

crates/gui-host/src/
  app.rs                    + pending_crashes: Vec<CrashReport> field
  lib.rs                    起動時 list_pending_crashes() を呼んで HostApp に渡す
  settings.rs               + Pending crash reports section(Update banner の上に配置)

crates/gui-common/locales/
  en/main.ftl               + 4 crashlog-* IDs
  ja/main.ftl               同

scripts/
  sign-msi.ps1              (新) signtool sign + verify ラッパー、引数 CertPath/CertPassword/MsiPath

docs/
  sign-and-release.md       (新) Cert 取得手順 + sign-msi.ps1 使い方 + チェックリスト
  build-msi.md              (修正) "Sign the MSI" セクション追記、sign-and-release.md にリンク
```

### `crashlog::install_panic_hook` の実装方針

```rust
// crates/gui-common/src/crashlog.rs

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::TailHandle;

static TAIL: OnceLock<TailHandle> = OnceLock::new();

/// Register a TailHandle so install_panic_hook can include the most recent
/// log lines in the dump. Optional — if not called, recent_log_lines is
/// empty in the JSON.
pub fn register_tail(tail: TailHandle) {
    let _ = TAIL.set(tail);
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct CrashReport {
    pub binary: String,
    pub version: String,
    pub timestamp_iso: String,
    pub panic_message: String,
    pub panic_location: String,
    pub recent_log_lines: Vec<String>,
}

/// Install a panic hook that dumps a JSON crash report to
/// `dirs::cache_dir()/prdt/crashes/`.
pub fn install_panic_hook(binary_name: &'static str, version: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let report = build_report(binary_name, version, info);
        if let Err(e) = write_report(&report) {
            // We're already in a panic; tracing is best-effort.
            eprintln!("crashlog: failed to write report: {e}");
        }
        // Also let the existing tracing subscriber log it normally.
        tracing::error!(
            binary = report.binary,
            location = report.panic_location,
            message = report.panic_message,
            "PANIC"
        );
    }));
}

fn build_report(binary: &str, version: &str, info: &std::panic::PanicHookInfo) -> CrashReport {
    let panic_message = match info.payload().downcast_ref::<&'static str>() {
        Some(s) => (*s).to_string(),
        None => info
            .payload()
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "panic with non-string payload".to_string()),
    };
    let panic_location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_else(|| "unknown".to_string());
    let recent_log_lines = TAIL
        .get()
        .map(|h| h.snapshot())
        .unwrap_or_default();
    CrashReport {
        binary: binary.to_string(),
        version: version.to_string(),
        timestamp_iso: chrono::Utc::now().to_rfc3339(),
        panic_message,
        panic_location,
        recent_log_lines,
    }
}

fn write_report(report: &CrashReport) -> std::io::Result<PathBuf> {
    let dir = crashes_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no cache dir")
    })?;
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!(
        "{}-{}-{}.json",
        stamp,
        report.binary,
        std::process::id()
    ));
    let json = serde_json::to_string_pretty(report)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// `dirs::cache_dir()/prdt/crashes/`
pub fn crashes_dir() -> Option<PathBuf> {
    crate::config_root().map(|d| d.join("crashes"))
}

/// List unacknowledged reports, newest first.
pub fn list_pending_crashes() -> std::io::Result<Vec<CrashReport>> {
    let Some(dir) = crashes_dir() else {
        return Ok(Vec::new());
    };
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths.reverse(); // newest first
    let mut out = Vec::new();
    for p in paths {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(r) = serde_json::from_str::<CrashReport>(&s) {
                out.push(r);
            }
        }
    }
    Ok(out)
}

/// Move a report into `crashes/acknowledged/` so it doesn't appear in
/// `list_pending_crashes` again.
pub fn mark_acknowledged(timestamp_iso: &str, binary: &str) -> std::io::Result<()> {
    let Some(dir) = crashes_dir() else {
        return Ok(());
    };
    let acked = dir.join("acknowledged");
    std::fs::create_dir_all(&acked)?;
    // Find the matching file by reading each pending report's contents.
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if !p.is_file() || p.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }
        let s = std::fs::read_to_string(&p)?;
        if let Ok(r) = serde_json::from_str::<CrashReport>(&s) {
            if r.timestamp_iso == timestamp_iso && r.binary == binary {
                let dest = acked.join(p.file_name().expect("file name"));
                std::fs::rename(&p, dest)?;
                return Ok(());
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no matching crash report",
    ))
}
```

`config_root()` は G1 で実装済(`dirs::config_dir().join("prdt")`)。**ただし** Phase 4 G2 の overlay IPC が `cache_dir()` を使っており、本 spec の crashes も `cache_dir()` ベースが自然。`config_root()` はこのまま `config_dir()` を返し、別途 `cache_root()` も追加するか、crashlog 内で `dirs::cache_dir()` を直接呼ぶか — 後者で実装する。

Spec 修正:`crashes_dir()` の中で `dirs::cache_dir().map(|d| d.join("prdt").join("crashes"))` を直接呼ぶ。

### Settings UI の Pending crashes section

```
┌─ Settings ───────────────────────────────────────────┐
│  Last session crashed (2 reports):                    │  ← G5 banner
│  - 2026-04-25T14:32:01Z  prdt-host  "...panic msg"   │
│  - 2026-04-25T13:11:45Z  prdt-viewer  "...panic msg" │
│  [Open crashes folder]   [Acknowledge all]            │
│  ──────────                                           │
│                                                       │
│  ⚠ Update available: v0.0.2 [Install]                 │  ← G4
│                                                       │
│  Bind:        [ 0.0.0.0:9000 ]                       │
│  ... (既存) ...                                       │
└──────────────────────────────────────────────────────┘
```

「Open crashes folder」→ `dirs::cache_dir()/prdt/crashes/` を `explorer` で開く。「Acknowledge all」→ pending な全レポートを `acknowledged/` 配下に移動。

### `scripts/sign-msi.ps1`

```powershell
# Phase 4 G5 — Sign a Power Remote Desktop MSI with Authenticode.
# Requires Windows SDK signtool.exe in PATH.
param(
    [Parameter(Mandatory=$true)] [string]$CertPath,
    [Parameter(Mandatory=$true)] [string]$CertPassword,
    [Parameter(Mandatory=$true)] [string]$MsiPath,
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [string]$Description = "Power Remote Desktop"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $CertPath)) {
    throw "Certificate file not found: $CertPath"
}
if (-not (Test-Path $MsiPath)) {
    throw "MSI not found: $MsiPath"
}

$signtool = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
if (-not $signtool) {
    throw "signtool.exe not in PATH. Install Windows SDK or add the SDK bin dir to PATH."
}

Write-Host "Signing $MsiPath..."
& $signtool sign `
    /f $CertPath `
    /p $CertPassword `
    /t $TimestampUrl `
    /td sha256 `
    /fd sha256 `
    /d $Description `
    /v `
    $MsiPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool sign failed (exit $LASTEXITCODE)"
}

Write-Host "Verifying signature..."
& $signtool verify /pa /v $MsiPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool verify failed (exit $LASTEXITCODE)"
}

Write-Host "Successfully signed and verified $MsiPath"
```

### `docs/sign-and-release.md`(構成)

- Authenticode certificate sources(EV vs OV 比較、Sectigo / DigiCert / SSL.com 等のリンク)
- ハードウェアトークン手順(EV cert は USB トークン必須)
- `scripts/sign-msi.ps1` の使い方
- リリース前チェックリスト:
  1. `version` を `Cargo.toml` で bump
  2. `cargo run -p prdt-gui-host --bin mkicon`
  3. `cargo build --release -p prdt-host -p prdt-viewer -p prdt-viewer-overlay`
  4. `cargo wix --no-build`
  5. `scripts/sign-msi.ps1 -CertPath ... -MsiPath target/wix/prdt-setup-vX.Y.Z.msi`
  6. `git tag -a vX.Y.Z` + `git push --tags`
  7. `gh release create vX.Y.Z target/wix/prdt-setup-vX.Y.Z.msi`

### existing panic_hook の整理

- `crates/host/src/main.rs` に既存の `std::panic::set_hook(Box::new(|info| { tracing::error!(panic = %info, "PANIC"); }));` がある(G3 / 既存)
- これを `crashlog::install_panic_hook(...)` に置換。既存の tracing 出力は crashlog の hook 内で行うので機能維持
- 同パターンを viewer / overlay にも適用

---

## Testing Strategy

### 1. crashlog round-trip unit test

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// install_panic_hook は global state を触るので、テスト並列実行時に
    /// 他テストと衝突する。env var で opt-in にする。
    #[test]
    fn install_then_panic_writes_report() {
        if std::env::var("PRDT_TEST_CRASHLOG").is_err() {
            eprintln!("skipping: set PRDT_TEST_CRASHLOG=1 to opt in");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Override the crashes_dir for the test by setting an env var
        // that the impl honors; or use a feature-gated test-only path.
        std::env::set_var("PRDT_CRASHLOG_DIR", dir.path());

        install_panic_hook("test-bin", "0.0.1-test");
        let result = std::panic::catch_unwind(|| {
            panic!("test panic message");
        });
        assert!(result.is_err());

        let pending = list_pending_crashes_in(dir.path()).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].binary, "test-bin");
        assert!(pending[0].panic_message.contains("test panic message"));
    }
}
```

`PRDT_CRASHLOG_DIR` env var support:`crashes_dir()` を `if let Ok(p) = std::env::var("PRDT_CRASHLOG_DIR") { return Some(PathBuf::from(p)); }` でテスト override 可能にする。

`list_pending_crashes_in(&Path)` という pub(crate) 関数を切り出して `crashes_dir()` を引数で受け取れるようにする。

### 2. CrashReport JSON schema test

```rust
#[test]
fn crash_report_json_round_trip() {
    let r = CrashReport {
        binary: "prdt-host".into(),
        version: "0.0.1".into(),
        timestamp_iso: "2026-04-25T12:00:00Z".into(),
        panic_message: "boom".into(),
        panic_location: "src/main.rs:42".into(),
        recent_log_lines: vec!["INFO: started".into()],
    };
    let json = serde_json::to_string(&r).unwrap();
    let back: CrashReport = serde_json::from_str(&json).unwrap();
    assert_eq!(r.binary, back.binary);
    assert_eq!(r.recent_log_lines.len(), 1);
}
```

### 3. mark_acknowledged moves file

```rust
#[test]
fn mark_acknowledged_moves_file() {
    let dir = tempfile::tempdir().unwrap();
    let r = CrashReport { /* ... */ };
    let path = dir.path().join("20260425-120000-prdt-host-1234.json");
    std::fs::write(&path, serde_json::to_string(&r).unwrap()).unwrap();

    mark_acknowledged_in(dir.path(), &r.timestamp_iso, &r.binary).unwrap();

    assert!(!path.exists(), "original file should be moved");
    let acked = dir.path().join("acknowledged");
    let entries: Vec<_> = std::fs::read_dir(&acked).unwrap().collect();
    assert_eq!(entries.len(), 1);
}
```

### 4. 手動 smoke

- `prdt-host.exe` 起動 → Settings の「Pending crash reports」セクション空 → OK
- 故意に panic を起こす(例:`prdt-host --simulate-panic` 等の hidden flag、または release build に手を入れて temp panic)→ ファイルが書かれることを確認
- 再起動 → Settings に banner 表示
- 「Open crashes folder」→ explorer 開く
- 「Acknowledge all」→ banner 消える、ファイルは `acknowledged/` 配下にある

### 5. 署名スクリプト smoke(cert なしで dry-run)

cert がないので `scripts/sign-msi.ps1 -CertPath nonexistent.pfx ...` を実行 → "Certificate file not found" でエラー。これでスクリプト本体の構文チェックは可能。

cert 取得後の本物 smoke は別タスク(運用判断)。

### 6. workspace tests + clippy

`cargo test --workspace` で 266 既存 + 3 新規 = 269 程度パス。
clippy `--workspace --all-targets --all-features -- -D warnings` clean。

---

## Exit Criteria

- [ ] `crates/gui-common/src/crashlog.rs` 実装
- [ ] `chrono` workspace dep 追加 + gui-common の `[dependencies]` に追加
- [ ] `prdt_gui_common::crashlog` re-exports from lib.rs
- [ ] 3 GUI binary main.rs に `install_panic_hook` 呼び出し追加
- [ ] gui-host の `run_host_gui` で `list_pending_crashes()` を起動時に呼ぶ
- [ ] `HostApp::pending_crashes` field + Settings に Pending crashes section
- [ ] Settings に「Open folder」/「Acknowledge all」ボタン
- [ ] i18n IDs 4 個(`crashlog-*`)
- [ ] `scripts/sign-msi.ps1` 配置
- [ ] `docs/sign-and-release.md` 作成
- [ ] `docs/build-msi.md` に Sign step 追記
- [ ] crashlog unit tests(round-trip + JSON schema + mark_acknowledged)pass
- [ ] workspace tests pass、clippy clean
- [ ] git tag `phase4-g5-complete`(= **Phase 4 全体完了**)

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| `set_hook` を多重 install したら最後の一つだけ有効 | crashlog が黙って消える | 各 binary main の冒頭で 1 回だけ呼ぶ(複数モジュールで呼ばない) |
| panic 時に `dirs::cache_dir()` が失敗 | ダンプ書けず、stderr のみ | `eprintln!` で fallback、tracing も生きる |
| `OnceLock<TailHandle>::set` を host が呼んで viewer が呼ばないと viewer の crashlog に log lines が空 | 部分情報のみ | viewer/overlay は最初から log lines 空で OK、parent spec の "直近 50 行" は best-effort |
| ファイル名衝突(同一秒に 2 binary panic)| 上書き | ファイル名に PID を含める(spec 確定) |
| `PRDT_CRASHLOG_DIR` env を本番で誤設定 | 想定外パスに書く | doc 化、本番では unset 推奨 |
| Cert 購入が遅延 → unsigned で公開 | SmartScreen 警告で初期 UX 悪い | OV cert 購入 or self-signed + ユーザー教育 / Phase 5 判断 |
| signtool タイムスタンプサーバ不通 | 署名失敗 | スクリプトで複数サーバ rotate 可能(`-TimestampUrl` 引数で差し替え) |
| クラッシュレポート PII(ユーザー名がログに含まれる)漏洩 | プライバシー懸念 | ローカル保存のみ、自動送信無し → 漏洩経路無し。Phase 5 で送信統合時に再検討 |

---

## Open Questions(実装中に決めてよい)

- crashlog ファイルの保存期間 / 自動削除 — G5 では削除しない、ユーザー手動 or Phase 5
- `recent_log_lines` の上限 — 50 行(parent spec 通り)、文字列長 4KB 程度
- `Acknowledge all` で同時に「フォルダ削除」もする?— No、移動のみ
- viewer / overlay 側の Pending crashes UI — host のみ表示で十分(parent spec 整合、host が常駐 supervisor)
- panic_hook で stderr に出ている既存 tracing log を上書きしない — 既存 `tracing_subscriber` は別 layer なので影響なし

---

## References

- 親 spec: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(F7 crash reporter、G5 署名)
- G1 spec: `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`(`TailHandle` / `config_root`)
- G3 spec: `docs/superpowers/specs/2026-04-25-phase4-g3-tray-design.md`(panic_hook の置き換え対象)
- G4 spec: `docs/superpowers/specs/2026-04-25-phase4-g4-msi-update-design.md`(`docs/build-msi.md` 修正対象)
- Authenticode: <https://learn.microsoft.com/en-us/windows/win32/seccrypto/cryptography-tools>
- `signtool`: <https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool>
- Sectigo OV cert: <https://www.sectigo.com/ssl-certificates-tls/code-signing>
