# Phase 4 G6 — Internationalization (ja + en) Design

**Project**: power-remote-dt
**Phase**: 4 (GUI + 配布)、サブプラン G6
**Date**: 2026-04-25
**Status**: Draft (built on `phase4-g1-complete` master)
**Parent spec**: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(Phase 4 全体)

---

## Summary

`prdt-gui-host` と `prdt-gui-viewer` の UI 文字列を Mozilla Fluent ベースに切り替え、英語(`en`)と日本語(`ja`)の 2 言語をビルドに同梱する。OS のロケールから自動選択し、Settings から手動切替も可能。`%APPDATA%\prdt\locale\*.ftl` にユーザーが置いたファイルが embedded 版を上書きできる。

到達目標: G6 終了時点で Windows のロケールが日本語の環境では GUI が日本語で表示され、英語環境では英語、Settings の言語ドロップダウンで切替できる。文字列追加忘れは i18n 系の実行時 fallback("missing-string: <id>")で容易に検出可能。

---

## Scope

### In-scope (G6)

- `prdt-gui-common` に `i18n` モジュール追加:
  - `fluent_templates::static_loader!` で `en.ftl` / `ja.ftl` を crate にバンドル
  - `Locale` enum + `current_locale()`(初回:`Config.gui.locale` → 未設定なら `sys-locale` から OS detect)
  - `tr(&str) -> String`(引数なし)+ `tr_args(&str, FluentArgs) -> String`(引数あり)
  - 起動時に `%APPDATA%\prdt\locale\<lang>.ftl` を読んで bundled 版とマージ(ユーザー override)
- 言語ファイル:
  - `crates/gui-common/locales/en.ftl`(bundled、ベースライン)
  - `crates/gui-common/locales/ja.ftl`(bundled、日本語訳)
- `Config.gui.locale` フィールド追加(`String`、`""` = auto、`"en"`/`"ja"` = 強制)
- `prdt-gui-host` の全 UI 文字列を `tr("...")` に置換(ハードコード文字列ゼロを目標)
- `prdt-gui-viewer` の全 UI 文字列を `tr("...")` に置換
- 両 GUI の Settings に Language ドロップダウン追加(en / ja / Auto)
- 言語切替時の即時反映(eframe は次フレームで再描画されるので新文字列が見える)
- テスト:
  - 全 string ID が両ロケールに存在(欠損検出)
  - 引数付き文字列の placement 一致(`{ $name }` などのプレースホルダ)
  - `tr("nonexistent-id")` はパニックせず `"missing-string: nonexistent-id"` を返す

### Out (G6 では入れない)

- 3 言語目以降(中国語、韓国語、フランス語、…)— locale ファイルを置けば自動認識される設計だが、品質保証は別タスク
- ホスト/CLI ログメッセージの翻訳(英語のまま、運用ログ前提)
- アンインストーラ・MSI ダイアログの翻訳(G4 に依存、Phase 4 別サブプラン)
- 動的言語切替時のウィジェットサイズ再計算(eframe は自動リレイアウト)
- right-to-left レイアウト(ja/en 共に LTR、必要時 G6 後)
- ロケール固有の数値・日付フォーマット(現状 `%Y-%m-%dT%H:%M:%SZ` ISO のみ、G2+ の overlay で必要なら拡張)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| ライブラリ | `fluent_templates` 0.9 | `static_loader!` macro で `.ftl` 同梱、locale fallback chain 自動、`fluent_bundle` を直接触る必要なし |
| OS ロケール検出 | `sys-locale` 0.3 | クロスプラットフォーム、`String` 1 個取るだけの軽量 crate |
| 言語ファイル形式 | Fluent (`.ftl`) | parent spec 確定、bundle/argument/select すべて支持 |
| ID 命名 | kebab-case、画面 prefix | `host-welcome`、`host-button-start-listening`、`viewer-launcher-title`、`common-button-cancel` |
| デフォルト言語 | `en`(fallback chain の最終手段) | 開発者母国語、ja.ftl に翻訳漏れがあっても表示は崩れない |
| 自動検出ロジック | OS locale が `ja` または `ja-*` なら `ja`、それ以外は `en` | 2 言語のみなのでシンプル match |
| ユーザー override 場所 | `%APPDATA%\prdt\locale\<lang>.ftl` | parent spec 通り、起動時マージ |
| マージ方式 | bundle に bundled `.ftl` を `add_resource`、その後 user `.ftl` を `add_resource` で上書き | Fluent bundle は同 ID を後勝ちで上書き、シンプル |
| Settings UI | 「Language: [Auto / English / 日本語]」ドロップダウン、選択で `Config.gui.locale` 更新 + 保存 | ユーザーが言語混在環境で固定したいケースに対応 |
| 起動時の lazy init | `OnceLock<Locale>` + `OnceLock<FluentBundle>` | 起動時に 1 回だけ resolve |
| 切替時の再 init | `i18n::set_locale(new)` がグローバル状態を更新、次フレーム以降に反映 | 単純、locale 切替は頻繁ではない |
| missing-string の挙動 | `format!("missing-string: {id}")` を返す(パニックしない) | UI が「壊れた」見た目で残り、開発時に発見しやすい |
| テスト戦略 | 両 locale ファイルから ID 集合を抽出、対称差が空であることを assert | ID 漏れ自動検出 |

---

## Architecture

### モジュール配置

```
crates/gui-common/
  Cargo.toml                 + fluent_templates, sys-locale, unic-langid
  locales/
    en.ftl                   英語ベースライン
    ja.ftl                   日本語訳
  src/
    i18n.rs                  static_loader! + tr() + tr_args() + set_locale() + current_locale()
    config.rs                + GuiConfig { locale: String } を追加
    lib.rs                   + pub use i18n::*
```

### Public API

```rust
// gui-common::i18n

pub enum Locale { En, Ja }

impl Locale {
    pub fn as_str(&self) -> &'static str { ... }    // "en" / "ja"
    pub fn from_config_str(s: &str) -> Option<Self> { ... }  // "" → None (auto), "en" / "ja" → Some
}

/// Detect from OS via sys-locale; fallback En.
pub fn detect_locale() -> Locale { ... }

/// Switch the active locale (cheap; sets a OnceLock-style global).
pub fn set_locale(l: Locale);

/// Get the current locale.
pub fn current_locale() -> Locale;

/// Translate a key. Returns "missing-string: <id>" when not found.
pub fn tr(id: &str) -> String;

/// Translate a key with named arguments.
pub fn tr_args(id: &str, args: &HashMap<&str, FluentValue>) -> String;
```

For ergonomic call sites we also export a small macro:

```rust
// macro_rules: t!("host-welcome") expands to tr("host-welcome")
//              t!("host-listening", bind => &cfg.host.bind) expands to tr_args(...)
#[macro_export]
macro_rules! t {
    ($id:literal) => { $crate::tr($id) };
    ($id:literal, $($k:ident => $v:expr),+ $(,)?) => {{
        let mut args = std::collections::HashMap::new();
        $(args.insert(stringify!($k), $crate::FluentValue::from($v));)+
        $crate::tr_args($id, &args)
    }};
}
```

(実際の引数構築は fluent_templates の API に合わせる。スペック上は「`t!("key")` と `t!("key", name => &val)` の 2 形式が使える」が伝わればよい。)

### locale 解決の優先順位

```
1. Config.gui.locale が "en" or "ja" → それを使う
2. Config.gui.locale が "" or 未設定 → sys-locale で OS から取得
   - "ja*" → Ja
   - それ以外(または取得失敗)→ En
```

### user-override .ftl の合流

起動時:

```rust
fn build_bundle(locale: Locale) -> FluentBundle {
    // 1. bundled resource (from static_loader!)
    let mut bundle = FluentBundle::new(vec![locale.langid()]);
    bundle.add_resource(BUNDLED[locale]).expect("bundled valid");

    // 2. user override at %APPDATA%\prdt\locale\<lang>.ftl (if exists)
    if let Some(p) = config_root().map(|d| d.join("locale").join(format!("{}.ftl", locale.as_str()))) {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(res) = FluentResource::try_new(text) {
                // FluentBundle::add_resource_overriding allows replacing existing IDs
                bundle.add_resource_overriding(res);
            } else {
                tracing::warn!(path = %p.display(), "user .ftl failed to parse; ignoring");
            }
        }
    }
    bundle
}
```

### Config 変更

`prdt-gui-common` の `Config` に新セクション:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GuiConfig {
    /// "" = auto-detect from OS locale, "en" / "ja" = forced
    #[serde(default)]
    pub locale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub gui: GuiConfig,    // NEW
    #[serde(default)]
    pub host: HostConfig,
    #[serde(default)]
    pub viewer: ViewerConfig,
}
```

既存設定ファイル(G1 で生成済み)は `[gui]` セクションが無いので serde の `#[serde(default)]` で空 `GuiConfig` が入る。互換性 OK。

### gui-host / gui-viewer の string ID 一覧(代表例)

```
common-button-cancel = Cancel
                     = キャンセル
common-button-save = Save
                   = 保存
common-button-copy = Copy
                   = コピー

host-window-title = Power Remote Desktop — Host
                  = Power Remote Desktop — ホスト
host-welcome-heading = Welcome
                     = ようこそ
host-welcome-body = Generate a host key to start. The key uniquely identifies this machine to viewers.
                  = ホスト鍵を生成して開始してください。この鍵はこのマシンを viewer に対して一意に識別します。
host-button-generate-key = Generate host key
                         = ホスト鍵を生成
host-status-idle = Status: Idle
                 = 状態: 待機中
host-status-listening = Status: ● Listening on { $bind }
                      = 状態: ● { $bind } で待ち受け中
host-button-start-listening = Start listening
                            = 待ち受け開始
host-button-stop = Stop
                 = 停止
host-button-settings = Settings…
                     = 設定…
host-pubkey-label = Public key:
                  = 公開鍵:
host-recent-activity = Recent activity:
                     = 最近の動き:
host-key-file-label = Key file: { $path }
                    = 鍵ファイル: { $path }
host-error-key-load = key load failed: { $error }
                    = 鍵の読み込みに失敗しました: { $error }
host-error-qr = qr generation failed: { $error }
              = QR 生成に失敗しました: { $error }

viewer-window-title = Power Remote Desktop — Viewer
                    = Power Remote Desktop — ビューアー
viewer-launcher-heading = Saved connections
                        = 保存済み接続先
viewer-no-connections = (no saved connections)
                      = (保存済み接続先がありません)
viewer-button-add = + Add new connection
                  = + 新規接続を追加
viewer-button-connect = Connect
                      = 接続
viewer-button-quit = Quit
                   = 終了
viewer-decoder-label = Decoder:
                     = デコーダー:
viewer-form-label = Label:
                  = ラベル:
viewer-form-mode = Mode:
                 = モード:
viewer-form-mode-direct = Direct
                        = 直接
viewer-form-mode-signaling = Signaling
                           = シグナリング
viewer-form-addr = Address (host:port):
                 = アドレス (host:port):
viewer-form-host-id = Host ID (e.g. 123-456-789):
                    = ホスト ID (例: 123-456-789):
viewer-form-pubkey = Public key (base64; leave empty for TOFU):
                   = 公開鍵 (base64、空欄なら TOFU):
viewer-settings-decoder-mf = MF (default)
                           = MF (既定)
viewer-settings-decoder-nvdec = NVDEC (zero-copy)
                              = NVDEC (zero-copy)
viewer-settings-recv-dir = Receive directory:
                         = 受信ディレクトリ:
viewer-settings-signaling-url = Signaling URL:
                              = シグナリング URL:
viewer-settings-resolution = Default resolution:
                           = 既定解像度:
viewer-settings-fps = Default fps:
                    = 既定 fps:

settings-window-title = Settings
                      = 設定
settings-bind = Bind:
              = バインド:
settings-monitor = Monitor:
                 = モニター:
settings-bitrate = Bitrate (Mbps):
                 = ビットレート (Mbps):
settings-outgoing = Outgoing dir:
                  = 送信ディレクトリ:
settings-signaling-optional = Signaling URL (optional):
                            = シグナリング URL (任意):
settings-language = Language:
                  = 言語:
settings-language-auto = Auto
                       = 自動
```

完全な ID 集合は実装時に固める(~50 個程度の見込み)。

### Locale 切替の Settings UI

Settings modal の末尾に Language 行を追加:

```
Language: [▼ Auto] [▼ English] [▼ 日本語]
```

選択 → `Config.gui.locale = "auto" | "en" | "ja"` に書き戻し → save → `i18n::set_locale(...)` 呼ぶ → 次回 update() で再描画。

---

## Testing Strategy

### 1. ID 完全性 unit test

`crates/gui-common/src/i18n.rs` の `#[cfg(test)]`:

```rust
#[test]
fn locale_files_have_same_ids() {
    let en = parse_ftl(include_str!("../locales/en.ftl"));
    let ja = parse_ftl(include_str!("../locales/ja.ftl"));
    let en_ids: HashSet<_> = en.iter().map(|m| m.id.clone()).collect();
    let ja_ids: HashSet<_> = ja.iter().map(|m| m.id.clone()).collect();
    let only_en: Vec<_> = en_ids.difference(&ja_ids).collect();
    let only_ja: Vec<_> = ja_ids.difference(&en_ids).collect();
    assert!(
        only_en.is_empty() && only_ja.is_empty(),
        "Locale ID mismatch.\n  Only in en: {only_en:?}\n  Only in ja: {only_ja:?}",
    );
}
```

`parse_ftl` は fluent_syntax で簡単に書ける(または fluent_templates の internal API)。

### 2. Placeholder 一致 unit test

各 ID について en と ja の placeholders(`{ $name }`)集合が一致することを確認:

```rust
#[test]
fn placeholders_match_across_locales() {
    let en = parse_ftl(include_str!("../locales/en.ftl"));
    let ja = parse_ftl(include_str!("../locales/ja.ftl"));
    for m in &en {
        let ja_m = ja.iter().find(|x| x.id == m.id).expect("id exists in ja");
        let en_p = extract_placeholders(&m.value);
        let ja_p = extract_placeholders(&ja_m.value);
        assert_eq!(en_p, ja_p, "Placeholder mismatch for {}", m.id);
    }
}
```

### 3. tr() round-trip

```rust
#[test]
fn tr_returns_translated_string() {
    set_locale(Locale::En);
    assert_eq!(tr("common-button-cancel"), "Cancel");
    set_locale(Locale::Ja);
    assert_eq!(tr("common-button-cancel"), "キャンセル");
}

#[test]
fn tr_args_substitutes_placeholders() {
    set_locale(Locale::En);
    let mut args = HashMap::new();
    args.insert("bind", FluentValue::from("0.0.0.0:9000"));
    let s = tr_args("host-status-listening", &args);
    assert!(s.contains("0.0.0.0:9000"));
}

#[test]
fn missing_id_returns_marker_string() {
    let s = tr("definitely-not-a-real-id");
    assert!(s.starts_with("missing-string:"));
}
```

### 4. detect_locale() smoke

```rust
#[test]
fn detect_locale_returns_some_locale() {
    let l = detect_locale();
    matches!(l, Locale::En | Locale::Ja);  // doesn't panic, returns one of two
}
```

### 5. user override

`%APPDATA%\prdt\locale\en.ftl` を temp dir に書いて load → 同 ID を override → `tr` がユーザー版を返す。

### 6. gui-host / gui-viewer 文字列移行回帰

Tasks の最後に手動 smoke:
- `prdt-host.exe` を Japanese system locale で起動 → 日本語表示
- `prdt-viewer.exe --help` → ヘルプは英語のまま(CLI は in-scope ではない)
- Settings → Language: English → 英語表示に切替

### 7. clippy / fmt

`cargo clippy --workspace --all-targets --all-features -- -D warnings` clean、新規ファイル fmt clean

---

## Exit Criteria

- [ ] `crates/gui-common/locales/{en.ftl,ja.ftl}` 作成、~50 ID
- [ ] `gui-common::i18n` モジュール実装、5 unit tests pass
- [ ] `Config.gui.locale: String` 追加、既存 config.toml が壊れない
- [ ] `gui-host` の全 UI 文字列を `t!("…")` で置換
- [ ] `gui-viewer` の全 UI 文字列を `t!("…")` で置換
- [ ] Settings に Language ドロップダウン追加(両 GUI で)
- [ ] 言語切替が次フレームで反映
- [ ] OS ロケールから自動検出
- [ ] 文字列移行漏れチェック(`grep -nE 'ui\.(label|heading|button)\("'` で空 = 達成)
- [ ] workspace 全テスト pass
- [ ] clippy clean
- [ ] git tag `phase4-g6-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| `fluent_templates::static_loader!` が egui 0.28 / async tokio と衝突 | コンパイル不可 | Mozilla 公式チュートリアルに沿った最小構成、binary search で衝突点特定 |
| Noto Sans JP が日本語で正しく描画されない(G1 で system TTF を流用したため) | UI 文字化け | G6 の Exit に「日本語表示確認」を入れる、必要なら Noto Sans CJK JP の実際のサブセットに差し替え(別タスク) |
| ID 命名衝突(viewer と host で同じ "button-save" が違う訳になる) | 表示混乱 | `common-`/`host-`/`viewer-`/`settings-` prefix 強制、命名 lint を test 化 |
| Settings の Language 切替直後に古い文字列が残るウィンドウタイトル | 切替体感悪い | eframe の `viewport_title` 更新 API を切替時に呼ぶ(検討、必要に応じて) |
| ユーザーが置いた壊れた `.ftl` が起動失敗を起こす | アプリ起動不可 | `FluentResource::try_new` 失敗時は warn ログ出して bundled 版にフォールバック |
| 引数付き文字列のプレースホルダー漏れ(`{ $bind }` が ja で `{ $port }` になっている) | 文字列が崩れる | placeholders_match_across_locales test で機械検出 |

---

## Open Questions(実装中に決めてよい)

- Language ドロップダウンの並び("Auto / English / 日本語" or "Auto / 日本語 / English")— UX 慣例的には "Auto / 母国語(現在ロケール)/ English / その他"
- 「Auto」を内部表現で `""` にするか `"auto"` にするか — `""` で `#[serde(default)]` と相性よく統一
- ja.ftl の語尾(です・ます調 / 命令形)— です・ます調統一(設定アプリの慣習)
- HTML エスケープが必要か — egui は raw text 描画なので不要(XSS なし)
- フォントサブセット(G1 で Noto Sans JP 9MB が同梱されている)— G6 では触らない、G6 完了後に削減検討

---

## References

- 親 spec: `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`
- G1 spec: `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`
- Mozilla Fluent: <https://projectfluent.org/>(Context7 で参照可)
- `fluent_templates`: <https://docs.rs/fluent-templates/0.9>
- `sys-locale`: <https://docs.rs/sys-locale/0.3>
