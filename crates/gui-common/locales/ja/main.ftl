# 共通ボタン・ラベル
common-button-cancel = キャンセル
common-button-save = 保存
common-button-copy = コピー
common-button-browse = 参照…

# ホストウィンドウ
host-window-title = Power Remote Desktop — ホスト
host-welcome-heading = ようこそ
host-welcome-body = ホスト鍵を生成して開始してください。この鍵はこのマシンを viewer に対して一意に識別します。
host-button-generate-key = ホスト鍵を生成
host-key-file-label = 鍵ファイル: { $path }
host-status-idle = 状態: 待機中
host-status-listening = 状態: ● { $bind } で待ち受け中
host-button-start-listening = 待ち受け開始
host-button-stop = 停止
host-button-settings = 設定…
host-pubkey-label = 公開鍵:
host-recent-activity = 最近の動き:
host-error-key-load = 鍵の読み込みに失敗しました: { $error }
host-error-qr = QR 生成に失敗しました: { $error }
host-error-config-save = 設定の保存に失敗しました: { $error }
host-error-autostart = 自動起動の切り替えに失敗: { $error }

# ホスト設定
host-settings-bind = バインド:
host-settings-monitor = モニター:
host-settings-bitrate = ビットレート (Mbps):
host-settings-outgoing = 送信ディレクトリ:
host-settings-signaling-optional = シグナリング URL (任意):
settings-autostart-label = ログイン時に自動起動

# Viewer ランチャー
viewer-window-title = Power Remote Desktop — ビューアー
viewer-launcher-heading = 保存済み接続先
viewer-no-connections = (保存済み接続先がありません)
viewer-host-entry = { $label } — { $detail } ({ $mode })
viewer-button-add = + 新規接続を追加
viewer-button-connect = 接続
viewer-button-quit = 終了
viewer-decoder-label = デコーダー:
viewer-error-config-save = 設定の保存に失敗しました: { $error }

# Viewer 接続追加フォーム
viewer-form-title = 接続先を追加
viewer-form-label = ラベル:
viewer-form-mode = モード:
viewer-form-mode-direct = 直接
viewer-form-mode-signaling = シグナリング
viewer-form-addr = アドレス (host:port):
viewer-form-host-id = ホスト ID (例: 123-456-789):
viewer-form-pubkey = 公開鍵 (base64、空欄で TOFU):

# Viewer 設定
viewer-settings-title = Viewer 設定
viewer-settings-decoder-mf = MF (既定)
viewer-settings-decoder-nvdec = NVDEC (zero-copy)
viewer-settings-resolution = 既定解像度:
viewer-settings-fps = 既定 fps:
viewer-settings-recv-dir = 受信ディレクトリ:
viewer-settings-signaling-url = シグナリング URL:

# 設定共通
settings-window-title = 設定
settings-language = 言語:
settings-language-auto = 自動
settings-language-english = English
settings-language-japanese = 日本語

# Viewer overlay (Phase 4 G2)
overlay-window-title = Power Remote Desktop — オーバーレイ
overlay-host-label = 接続先: { $host }
overlay-stats-latency = レイテンシ
overlay-stats-samples = サンプル: { $n }
overlay-stats-decoder = デコーダー: { $name }
overlay-stats-connecting = 接続中…
overlay-button-resume = 再開
overlay-button-disconnect = 切断

# トレイ + 通知 (Phase 4 G3)
tray-tooltip = PrdtHost
tray-menu-open = 設定を開く
tray-menu-stop = 待ち受けを停止
tray-menu-show-logs = ログを開く
tray-menu-quit = 終了
notif-connected = Viewer 接続: { $detail }
notif-disconnected = Viewer が切断されました
notif-error = ホストエラー: { $detail }

# 自動アップデート (Phase 4 G4)
update-section-heading = アップデート
update-button-check = アップデートを確認
update-button-install = インストール
update-checking = アップデートを確認中…
update-up-to-date = 最新版を利用中です。
update-available = 新しいバージョン: { $version }
update-error = アップデート確認に失敗: { $error }

# クラッシュレポータ (Phase 4 G5)
crashlog-pending-heading = 前回のセッションでクラッシュしました ({ $n } 件)
crashlog-button-open-folder = クラッシュフォルダを開く
crashlog-button-acknowledge = すべて確認済みにする
crashlog-no-pending = 未送信のクラッシュレポートはありません。
crashlog-row-format = { $timestamp }  { $binary }  「{ $message }」

# 統合ホーム (RustDesk式1画面)
nav-home = ホーム
nav-settings = 設定
nav-logs = ログ

# ログ画面（ローリングGUIログファイルのアプリ内テール表示）
logs-heading = ログ
logs-card-eyebrow = アクティビティ
logs-file-path-label = ログファイル
logs-empty = まだログ出力はありません。共有を開始するか接続すると記録されます。
logs-no-path = ログディレクトリを解決できませんでした。

home-identity-error = 識別情報の初期化に失敗しました: { $error }
home-identity-error-hint = ホスト鍵ファイルと設定ディレクトリの権限を確認し、再起動してください。

# このデバイス
home-this-device-title = このデバイス
home-device-id-label = デバイスID
home-unprovisioned = 未プロビジョニング — シグナリングサーバーに接続できません
home-unprovisioned-no-url = シグナリングサーバーのURLが未設定です。下に入力して保存すると、IDが発行されます。
home-signaling-url-label = シグナリングサーバー URL
home-button-save-signaling = 保存してIDを取得
home-button-retry = 再試行
home-provisioning = プロビジョニング中…
home-provisioned = プロビジョニングが完了しました。
home-provision-failed = プロビジョニングに失敗しました: { $error }
home-pin-label = PIN
home-pin-show = 表示
home-pin-hide = 非表示
home-pin-none = PINは未設定です（認証モードがPINではありません）。
home-button-regenerate = 再生成
home-button-generate-pin = PINを生成
home-fingerprint-label = 鍵フィンガープリント
home-fingerprint-hint = なりすまし防止のため、相手のデバイスと別経路で照合してください。
home-button-show-qr = QRを表示
home-button-hide-qr = QRを隠す
home-sharing-label = このデバイスを共有
home-sharing-on = 共有中
home-sharing-off = 停止中
home-button-start-sharing = 共有を開始
home-button-stop-sharing = 共有を停止

# デバイスに接続
home-connect-title = デバイスに接続
home-peer-id-label = 相手のID（9桁）
home-peer-pin-label = PIN
home-peer-pin-hint = 相手のデバイスに表示されているPIN
home-button-connect = 接続
home-button-disconnect = 切断
home-connect-need-id = 相手のIDを入力してください。
home-connect-need-signaling = シグナリングサーバーのURLが未設定です。設定画面で入力してください。
home-connect-need-host = ホストアドレス（host:port）を入力してください。
home-connect-launched = ビューアを起動しました（pid { $pid }）。
home-connect-connecting = 接続中… { $target }（pid { $pid }）
home-connect-active = セッション実行中 { $target }（pid { $pid }）
home-connect-disconnected = 切断しました（正常終了）。
home-connect-failed = ビューアが異常終了しました（{ $detail }）。相手のID/PIN・シグナリング設定・デコーダ設定を確認してください。
home-connect-exit-code = 終了コード { $code }
home-connect-exit-signal = シグナルにより終了
home-connect-already-active = 既にセッションが1件実行中です（同時接続は1件まで）。「切断」してから接続してください。
home-connect-single-session-note = 同時に接続できるセッションは1件までです。
home-host-elevation-skipped-viewer = 接続中のセッションを維持するため、管理者昇格をスキップして通常権限で共有を開始しました（タスクマネージャー等の管理者ウィンドウには入力できません。切断後に共有を開始し直すと昇格します）。
home-advanced = 詳細設定
home-advanced-host = ホスト host:port（直結モード）
home-advanced-pubkey = ホスト公開鍵（base64）
home-codec-label = コーデック
home-decoder-label = デコーダー
home-button-connect-direct = 接続（直結）
home-recent-title = 最近の接続
home-recent-empty = （履歴はありません）
home-button-connect-short = 接続
home-recent-remove = 削除

# 同意プロンプト（ホスト側・未知ピアの接続承認モーダル）
consent-heading = 接続リクエスト
consent-device-key-label = デバイスキー:
consent-label-optional = ラベル（任意）:
consent-permissions-heading = このセッションの許可権限:
consent-permission-input = 入力（キーボード/マウス）
consent-permission-clipboard = クリップボード
consent-permission-file-transfer = ファイル転送
consent-permission-audio = 音声
consent-remember = このデバイスを記憶する
consent-auto-deny = { $seconds }秒後に自動拒否
consent-deny = 拒否（Esc）
consent-allow = 許可
consent-allow-armed-in = { $seconds }秒後に許可可能
