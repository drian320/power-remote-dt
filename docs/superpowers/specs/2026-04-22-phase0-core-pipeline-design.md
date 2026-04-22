# Phase 0: Core Pipeline PoC — Design

**Project**: power-remote-dt (超低遅延マルチプラットフォーム・リモートデスクトップ、OSS 予定)
**Document**: Phase 0 (コアパイプライン PoC) 設計書
**Date**: 2026-04-22
**Status**: Draft (brainstorming 合意済、実装計画 (writing-plans) 未作成)

---

## Summary

本ドキュメントは、ゲームと業務の両方をカバーする超低遅延リモートデスクトップ `power-remote-dt` の **Phase 0 (コアパイプライン PoC)** の設計を定義する。目的は「**4K60 LAN 環境でエンドツーエンド <30ms を達成できるパイプラインが構築可能であること**」を実機実測で証明すること。

- 両端 Windows 11 + NVIDIA GPU、LAN 直接 IP 接続、1 モニタ、キーボード+マウスのみ。
- カスタム UDP トランスポート + NVENC/NVDEC 直叩き + D3D11 ゼロコピーレンダ(Moonlight/Sunshine 式)。
- Rust 製、Cargo workspace、6 クレートに分割。

Phase 0 達成後、Phase 1 で Linux 対応、Phase 2 で WAN/NAT 越え、Phase 3 で暗号化+付加機能、Phase 4 で GUI/配布、Phase 5 で公式インフラ、と段階的に進める。

---

## Context & Scope

### Project North Star

- **超低遅延**(gaming-tier)を犠牲にしないマルチプラットフォーム・リモートデスクトップ
- **公開 OSS / 配布プロダクト**(RustDesk 的ポジション)、最終的には全環境(Linux/Windows、NVIDIA/AMD/Intel/ソフトウェアフォールバック)対応
- 競合参照: Moonlight/Sunshine(低遅延)、Parsec(商用)、RustDesk(OSS)

### 6-Phase Decomposition

Phase 0 は全体設計の**最初の 1/6**。全体は以下の 6 フェーズに分割:

| Phase | 目的 | 本書との関係 |
|---|---|---|
| **0** | **コアパイプライン PoC — 低遅延達成の証明** | **本書のスコープ** |
| 1 | マルチプラットフォーム化(Linux 追加、Wayland 対応) | 後続 |
| 2 | WAN + NAT 越え + シグナリング | 後続 |
| 3 | E2E 暗号化 + 認証 + 音声 + クリップボード + ファイル転送 + マルチモニタ | 後続 |
| 4 | GUI + 配布(両 OS 用インストーラ、自動更新、コード署名) | 後続 |
| 5 | 運用 + 公開(公式リレー、ID サーバ、OSS 公開) | 後続 |

各フェーズは**独立した spec → plan → 実装サイクル**を持つ。Phase 0 が未達なら Phase 1 へ進まない。

### Phase 0 Scope

**In**:
- 両端 Windows 11
- LAN、直接 IP 接続(NAT 越えなし)
- 単一モニタ
- キーボード + マウス入力のみ(ゲームパッド、ペン入力は後続)
- H.265 (HEVC) コーデック、NVENC/NVDEC
- CLI 起動のみ(ポリッシュ GUI は Phase 4)

**Out**:
- 音声、クリップボード同期、ファイル転送、複数モニタ(Phase 3)
- 暗号化・認証(Phase 3)
- Linux、macOS、モバイル(Phase 1 以降)
- NAT 越え、シグナリング、ID ベース接続(Phase 2)
- AMD/Intel GPU、ソフトウェアエンコーダ(Phase 3 or 後続)
- AV1 エンコード(ハードが限定、Phase 3)
- HDR(`Windows.Graphics.Capture` への切替と同時、Phase 1)
- 適応ビットレート、自動輻輳制御(Phase 3)
- クラッシュレポータ、インストーラ、コード署名(Phase 4)

### Target Hardware (Phase 0 開発/計測用)

| ロール | OS | CPU | GPU | モニタ |
|---|---|---|---|---|
| Host PC | Windows 11 | Ryzen 5800 | **RTX 3070 Ti**(NVENC 7th gen, NVDEC 5th gen, AV1 decode ✓) | 3840×2160 |
| Viewer PC | Windows 11 | Ryzen 5700 | **GTX 1080**(NVENC 6th gen, NVDEC 3rd gen, AV1 decode ✗) | 3840×2160 |
| Network | 有線 1GbE LAN 想定 |

→ Phase 0 採用コーデックは **H.265 (HEVC)** 固定。AV1 はハード制約で不可。

### Performance Target (T4)

- **4K60 @ LAN (有線) エンドツーエンド < 30ms 中央値**(glass-to-glass、実機実測)
- 1080p/1440p は余裕で満たす想定(ベンチ B1〜B4 で検証)
- 無線 LAN は参考値のみ、Phase 0 の受入基準には含めない

---

## 1. Architecture

### 1.1 Process Layout

Phase 0 は **2 バイナリ**構成(常駐サービスにはしない):

```
┌─────────────────────┐                ┌─────────────────────┐
│   host (bin)        │                │   viewer (bin)      │
│   Win11 + RTX3070Ti │  ── UDP:9000 ──│   Win11 + GTX1080   │
│                     │                │                     │
│   デスクトップ配信   │                │   画面表示 + 入力   │
└─────────────────────┘                └─────────────────────┘
```

- **host**: キャプチャ → エンコード → 送信 + 入力パケット受信 → `SendInput` で注入
- **viewer**: 受信 → デコード → レンダ + 入力キャプチャ → 送信
- **CLI 起動**: `host --bind 0.0.0.0:9000`、`viewer <host-ip>:9000`
- ポリッシュ UI は Phase 4。Phase 0 の目的は計測と低遅延検証。

### 1.2 Workspace / Crates

Cargo workspace、6 クレート構成:

```
power-remote-dt/
├── Cargo.toml                    # [workspace]
└── crates/
    ├── protocol/                 # (OS 非依存) ワイヤー形式、パケット型、共通型
    ├── transport/                # (OS 非依存) Transport trait + CustomUdp 実装
    ├── media-win/                # (Windows) DXGI キャプチャ / NVENC / NVDEC / D3D11
    ├── input-win/                # (Windows) RawInput キャプチャ / SendInput 注入
    ├── host/                     # [bin]    host バイナリ
    ├── viewer/                   # [bin]    viewer バイナリ
    └── latency-bench/            # [bin]    内部計測ツール(セクション 7 参照)
```

**設計根拠**:
- `protocol` / `transport` は OS 依存ゼロ → Phase 1 で無変更で macOS/Linux 再利用
- `media-win` は Phase 1 で `media-linux` を**兄弟**として並列追加(既存コードに触らない)
- `host` / `viewer` は**薄いバイナリ**(数百行)、実体はライブラリクレート側
- `latency-bench` は Phase 0 内で作成する計測バイナリ

### 1.3 External Dependencies

主要クレート選定:

| カテゴリ | クレート | 採用理由 |
|---|---|---|
| Windows API 全般 | `windows`(Microsoft 公式) | COM/D3D11/DXGI/WinAPI 全面カバー、type-safe |
| Async runtime | `tokio` | UDP/タスク管理のデファクト |
| ログ | `tracing` + `tracing-subscriber` | 構造化ログ、レイテンシ span |
| CLI 引数 | `clap` | 標準 |
| シリアライズ(制御/入力) | `bincode` | 手書き並み高速、バグ少 |
| NVENC SDK | **自前 FFI**(`nvEncodeAPI.h` を `bindgen`) | 既存クレート更新停滞リスク回避 |
| NVDEC SDK | **自前 FFI**(`cuviddec.h` を `bindgen`) | 同上 |
| FEC | `reed-solomon-erasure` | 実績、Moonlight と同方式 |
| ウィンドウ/イベント | `winit` | D3D11 swapchain と結合容易 |
| D3D11 ラッパ | `windows` crate 直 | `wgpu` 経由はゼロコピー崩すため不採用 |
| Atomic ポインタ | `arc_swap` | lock-free decoded ring 用 |
| バイト列 | `bytes` | Bytes / BytesMut |
| プロパティテスト | `proptest`(dev-only) | FEC、reassembler の網羅 |
| カバレッジ | `cargo-llvm-cov`(dev-only) | Rust stable サポート |

---

## 2. Module Boundaries & Trait Abstraction

### 2.1 Design Principle

**trait の境界は「ワイヤー上」に置く**。細粒度 trait(DesktopCapture / VideoEncoder を全て公開)は Phase 0 では**内部用途のみ**とし、公開境界は**送信単位**(EncodedFrame、InputEvent)で統一。

これにより:
- zero-copy の制約(GPU テクスチャを trait 跨ぎしない)を自然に守れる
- Phase 1 で Linux を追加する際、`media-linux` クレートが同じ 2 公開 trait を実装するだけで済む

### 2.2 Core Types (`protocol` crate)

```rust
pub struct EncodedFrame {
    pub seq: u64,                     // host 単調
    pub timestamp_host_us: u64,       // monotonic clock
    pub is_keyframe: bool,            // IDR
    pub nal_units: bytes::Bytes,      // H.265 NAL 連結
    pub width: u32,
    pub height: u32,
}

pub enum InputEvent {
    MouseMove { x: i32, y: i32, absolute: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseWheel { dx: i32, dy: i32 },
    Key { scancode: u32, pressed: bool },
}

pub enum WirePacket<'a> {
    Video(VideoPacket<'a>),
    Input(InputEvent),
    Control(ControlMessage),
}
```

### 2.3 Primary Traits (公開境界、5 個)

```rust
pub trait VideoProducer: Send {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError>;
    fn request_idr(&mut self);
    fn set_target_bitrate(&mut self, bps: u32);
}

pub trait VideoConsumer: Send {
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError>;
    fn needs_idr(&self) -> bool;
}

pub trait Transport: Send {
    async fn send(&mut self, pkt: &[u8]) -> Result<(), TransportError>;
    async fn recv(&mut self) -> Result<bytes::Bytes, TransportError>;
}

pub trait InputCapturer: Send {
    async fn next_event(&mut self) -> Result<InputEvent, InputError>;
}

pub trait InputInjector: Send {
    fn inject(&mut self, ev: InputEvent) -> Result<(), InputError>;
}
```

### 2.4 Internal Traits (非公開、`media-win` 内部のみ)

```rust
trait DesktopCapture { async fn next(&mut self) -> Result<D3D11Texture>; }
trait VideoEncoder  { async fn encode(&mut self, tex: D3D11Texture) -> Result<EncodedFrame>; }
trait VideoDecoder  { async fn decode(&mut self, ef: EncodedFrame) -> Result<D3D11Texture>; }
trait VideoRenderer { async fn present(&mut self, tex: D3D11Texture) -> Result<()>; }
```

用途: モック差し替えによる単体テスト、将来の OBS 的機能追加時の拡張ポイント。**Phase 0 では外部に公開しない**。

### 2.5 Phase 0 Concrete Implementations

| trait | 実装 | 置き場所 |
|---|---|---|
| `VideoProducer` | `DxgiNvencProducer` | `media-win` |
| `VideoConsumer` | `NvdecD3D11Consumer` | `media-win` |
| `Transport` | `CustomUdpTransport` | `transport` |
| `InputCapturer` | `WinRawInputCapturer` | `input-win` |
| `InputInjector` | `WinSendInputInjector` | `input-win` |

### 2.6 Async Trait Approach

- **Rust stable `async fn in trait` を採用**(Rust 1.75+)
- `dyn VideoProducer` が必要な箇所(`main` の初期化時のみ)は `async-trait` マクロを併用
- ホットパスは generic + monomorphization で `dyn` を避ける

### 2.7 Binary Skeleton Sketches

`host` / `viewer` バイナリは**~100 行規模**、実体はライブラリクレート側。擬似コード:

```rust
// crates/host/src/main.rs
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let transport = CustomUdpTransport::bind(args.bind).await?;
    let (mut tx, mut rx) = transport.split();
    let mut producer: Box<dyn VideoProducer> =
        Box::new(DxgiNvencProducer::new(args.monitor, args.bitrate)?);
    let mut injector: Box<dyn InputInjector> =
        Box::new(WinSendInputInjector::new()?);

    let video = tokio::spawn(async move {
        loop {
            let frame = producer.next_frame().await?;
            tx.send_video(frame).await?;
        }
    });
    let input = tokio::spawn(async move {
        while let Ok(pkt) = rx.recv().await {
            if let WirePacket::Input(ev) = decode(pkt)? { injector.inject(ev)?; }
        }
    });
    tokio::try_join!(video, input)?;
    Ok(())
}
```

---

## 3. Thread & Async Model

### 3.1 Principles

1. **ブロッキング/GPU 同期コードは専用 OS スレッド**(tokio ワーカに載せない)
2. **tokio はネットワーク I/O と制御系のみ**
3. **MMCSS "Games" でキャプチャ/レンダスレッドを優先度ブースト**(プロセス全体の高優先度は使わない)
4. **バックプレッシャは「古いフレームを捨てる」**(溜めない、block しない)

### 3.2 Host Thread Layout

```
┌──────────────────────────────────────────────────────────────────┐
│  Host Process                                                    │
│                                                                  │
│  [ Main thread ]                                                 │
│    └─ tokio runtime(multi-thread, 3 workers)                    │
│       ├─ Task: UDP send loop(drain encoded_frame_rx)             │
│       ├─ Task: UDP recv loop(push input_event_tx)                │
│       ├─ Task: input inject loop(drain input_event_rx)           │
│       └─ Task: control loop(IDR 要求, ping 応答)                 │
│                                                                  │
│  [ Capture+Encode thread ] ← 専用 OS スレッド、MMCSS "Games"     │
│    └─ loop:                                                      │
│         1. DXGI AcquireNextFrame(timeout=16ms)                   │
│         2. NVENC encode(zero-copy D3D11 texture)                 │
│         3. bounded mpsc send(cap=2, try_send)                    │
│         例外: 満杯 → 新フレーム破棄 + IDR フラグ                 │
└──────────────────────────────────────────────────────────────────┘
```

### 3.3 Viewer Thread Layout

```
┌──────────────────────────────────────────────────────────────────┐
│  Viewer Process                                                  │
│                                                                  │
│  [ Main thread = Window + Render ] ← MMCSS "Games"               │
│    └─ winit event loop + D3D11 swapchain 所有                    │
│       ├─ WM_INPUT(RawInput) → input_event_tx                     │
│       └─ VSync 駆動で decoded_ring から最新テクスチャを Present  │
│                                                                  │
│  [ Decode thread ] ← 専用 OS スレッド、MMCSS "Games"             │
│    └─ loop:                                                      │
│         1. nal_rx.recv()(FEC 復元後の EncodedFrame)              │
│         2. NVDEC decode → ID3D11Texture2D                        │
│         3. decoded_ring.store(texture)(最新のみ保持)              │
│                                                                  │
│  [ tokio runtime(2 workers)]                                     │
│    ├─ Task: UDP recv → FEC → nal_tx                              │
│    ├─ Task: UDP send loop(drain input_event_rx)                  │
│    └─ Task: control loop(IDR 送信, ping)                         │
└──────────────────────────────────────────────────────────────────┘
```

### 3.4 Frame Time Budget (4K60 = 16.67ms/frame)

| ステージ | スレッド | 目標 |
|---|---|---|
| DXGI AcquireNextFrame | Capture thread | 1〜3ms |
| NVENC encode 投入+完了 | Capture thread | 5〜10ms |
| パケタイズ + FEC | tokio UDP send task | <1ms |
| ネット伝送(LAN) | — | <1ms |
| UDP recv + FEC 復元 | tokio recv task | <1ms |
| NVDEC decode | Decode thread | 3〜8ms |
| Present(vsync) | Main thread | 0〜16.67ms |
| **合計** | — | **15〜40ms** |

### 3.5 Input Path Budget (Viewer → Host)

| ステージ | 目標 |
|---|---|
| RawInput → mpsc | <0.5ms |
| UDP send | <1ms |
| LAN 伝送 | <1ms |
| UDP recv → inject | <0.5ms |
| `SendInput` | <1ms |
| **合計** | **<5ms** |

### 3.6 VSync

- **デフォルト**: `IDXGISwapChain::Present(1, 0)`
- **`--no-vsync`** オプション: `Present(0, ALLOW_TEARING)` で最低遅延モード(ティアリングあり)
- 両方計測して Phase 0 完了時のレポートに記載

### 3.7 Synchronization Primitives

| 用途 | 採用 |
|---|---|
| 単方向キュー | `tokio::sync::mpsc`(bounded/unbounded) |
| broadcast(IDR 要求等) | `tokio::sync::broadcast` |
| decoded ring(最新 1 枚) | `arc_swap::ArcSwapOption<D3D11Texture>` |
| カウンタ | `std::sync::atomic::*` |
| D3D11 | `ID3D11Device` 自由スレッド、`ID3D11DeviceContext` 単一固定 |

`std::sync::Mutex` は起動時初期化以外、ホットパスで禁止。

### 3.8 Startup & Shutdown Order

**起動**:
1. Transport(UDP bind + Hello 交換)
2. Producer/Consumer(デバイス初期化)
3. Input 系
4. Capture/Render スレッド起動

**停止**:
- `Ctrl-C` → tokio `select!` で shutdown シグナル受信
- チャネル drop で専用スレッドが自然終了
- Panic = `abort`(Cargo プロファイルで設定)

---

## 4. Data Flow

### 4.1 Video Frame Lifecycle (Host → Viewer)

ステージ `S1〜S10`(Performance Target の `T4` とは別概念):

Host 側:
1. **S1** DXGI `AcquireNextFrame` → `ID3D11Texture2D` 取得、`timestamp_host_us`/`seq` 付与
2. **S2** NVENC `EncodePicture`(zero-copy)→ H.265 NAL を `EncodedFrame` に詰める
3. **S3** パケタイズ: NAL を 1200B 単位に分割 → Reed-Solomon FEC(k=8, m=2 デフォルト)付加 → `VideoPacket` 群
4. **S4** UDP 送信(同一 socket から順次 sendto)

Viewer 側:
5. **S6** UDP recv → seq ごとに集約(reassembler)
6. **S7** FEC 復元。k 個以上到着で復元可、不足 + キーフレーム喪失なら IDR 要求
7. **S8** Decode thread へ push(bounded mpsc)
8. **S9** NVDEC decode → `ID3D11Texture2D`(NV12)
9. **S10** Main thread が VSync 駆動で `decoded_ring.load()` → D3D11 swapchain present

各段で保持する型:

| 段 | 型 | サイズ |
|---|---|---|
| S1 出力 | `ID3D11Texture2D` (BGRA8, 3840×2160) | ~32MB GPU 上 |
| S2 出力 | `EncodedFrame { nal_units: Bytes }` | 30〜150KB |
| S3 出力 | `Vec<VideoPacket>`(FEC 後) | (8+2)×1200B = 12KB 目安 |
| S7 出力 | `EncodedFrame`(再構築) | S2 と同じ |
| S9 出力 | `ID3D11Texture2D` (NV12) | ~12MB GPU 上 |

### 4.2 Input Event Lifecycle (Viewer → Host)

1. WM_INPUT(RawInput)→ `InputEvent` 変換、`timestamp_viewer_us`/`input_seq` 付与
2. `input_event_tx.send(ev)`(unbounded mpsc)
3. UDP send task が `WirePacket::Input` としてシリアライズ、sendto
4. Host UDP recv → inject task へ
5. `SendInput` 呼び出し → OS 入力キューへ注入

**規則**:
- コアレッシングしない(複数イベント集約しない)
- 再送しない
- 順序保証しない(マウス絶対座標、キーの対称性だけ確認)
- **stuck key 対策**: viewer フォーカス離脱時に全 key release 送信

### 4.3 IDR Request Loop

**発火トリガ**:
- FEC 復元失敗
- フレーム組立タイムアウト(100ms)
- NVDEC デコード失敗
- セッション開始直後(初回 IDR)
- 再接続直後
- 5 秒キーフレーム未到達の受動タイマ

**処理**:
- Viewer → Host: `ControlMessage::RequestIdr`
- Host: `producer.request_idr()` → 次 NVENC 呼び出しで `NV_ENC_PIC_FLAG_FORCEIDR`
- レート制限: 同一 100ms 窓で 1 回のみ処理

### 4.4 Session Handshake (Hello / HelloAck)

Phase 0 は認証・暗号化なし。接続確認のみ:

```
Viewer                              Host
──────                              ────
Hello{ proto=1, req_wxh, req_fps,
       codec=H265 }                 → recv Hello → 初期化
                                    HelloAck{ session_id, host_mono_base_us,
◄───                                          neg_wxh, neg_fps, neg_bitrate }
```

- Hello タイムアウト 3 秒、3 回失敗で viewer 終了
- プロトコルバージョン不一致は明示エラー終了(F12)

### 4.5 Ping / Pong

- Viewer → Host: `Ping { seq, viewer_ts_us }`
- Host → Viewer: `Pong { seq, viewer_ts_us, host_ts_us }`
- Viewer: `rtt = now - viewer_ts_us`、`clock_offset_est = host_ts_us - viewer_ts_us - rtt/2`
- 1Hz 送信、3 秒無応答で "disconnected" 表示、10 秒で終了(F8)

### 4.6 Session Termination

- Viewer `Ctrl-C` → `Bye` 送信 → Host はセッション停止、プロセス生存(次 Hello 受理可能)
- Host `Ctrl-C` → Host プロセス終了 → Viewer は Ping タイムアウトで自己終了

### 4.7 Buffers & Queues

| 場所 | 種別 | サイズ | 溢れ時 |
|---|---|---|---|
| Host encoded_frame channel | mpsc bounded | 2 | 古いドロップ + IDR フラグ |
| Host input_event channel | mpsc unbounded | — | (発生しない想定) |
| Net UDP OS buffer | — | OS 既定 | OS が落とす |
| Viewer reorder buffer(seq 集約) | `HashMap<u64, partial>` | 最大 8 seq 分 | 8 seq 古いは破棄 |
| Viewer nal channel | mpsc bounded | 2 | 古いドロップ + IDR 要求 |
| Viewer decoded_ring | `ArcSwap<Texture>` | 1 | 最新で上書き |

---

## 5. Transport Protocol

### 5.1 Principles

- UDP シングルポートで映像・入力・制御を多重化
- 固定ヘッダ + 可変ペイロード、バイナリ
- **再送しない**(ロスは FEC か IDR で回復)
- 輻輳制御は Phase 0 最小(静的 CBR + 計測のみ)
- `magic` + `version` バイトで明示的な壊しポイント

### 5.2 Common Header (16B)

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | `magic` (`0x52`) |
| 1 | 1 | `version` (`0x01`) |
| 2 | 1 | `packet_type` (0=Video, 1=Input, 2=Control) |
| 3 | 1 | `flags` |
| 4 | 8 | `session_id` |
| 12 | 4 | `payload_len` |
| 16 | N | payload(型別) |

### 5.3 VideoPacket Payload

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `frame_seq` |
| 8 | 8 | `timestamp_host_us` |
| 16 | 2 | `chunk_idx` |
| 18 | 2 | `source_chunks` (k) |
| 20 | 2 | `parity_chunks` (m) |
| 22 | 1 | `video_flags`(bit0=is_keyframe, bit1=is_parity) |
| 23 | 1 | reserved |
| 24 | 2 | `payload_bytes`(チャンク内有効長) |
| 26 | ≤1200 | `chunk_payload` |

- `chunk_idx ∈ [0, k + m)`
- 1 フレーム最大 32 チャンク、超過は IDR 要求 + ビットレート低下

### 5.4 InputPacket Payload

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `input_seq` |
| 8 | 8 | `timestamp_viewer_us` |
| 16 | 1 | `event_kind` (0=MouseMove, 1=MouseBtn, 2=Wheel, 3=Key) |
| 17 | N | event_body |

| event_kind | body | サイズ |
|---|---|---|
| MouseMove | `x:i32, y:i32, absolute:u8` | 9B |
| MouseBtn | `button:u8, pressed:u8` | 2B |
| Wheel | `dx:i32, dy:i32` | 8B |
| Key | `scancode:u32, pressed:u8` | 5B |

### 5.5 ControlPacket Payload

| control_kind | body | 向き |
|---|---|---|
| 0 Hello | proto, req_w, req_h, req_fps, codec | V→H |
| 1 HelloAck | session_id, host_mono_base_us, neg_w, neg_h, neg_fps, neg_bitrate | H→V |
| 2 Bye | — | 両方向 |
| 3 Ping | ping_seq, viewer_ts_us | V→H |
| 4 Pong | ping_seq, viewer_ts_us, host_ts_us | H→V |
| 5 RequestIdr | — | V→H |
| 6 SetBitrate | target_bps | 両方向 |
| 7 Stats | loss_rate, fps, bitrate(optional) | 両方向(debug) |

### 5.6 Serialization

- Phase 0 は `bincode`(fixint + little-endian)
- `magic`/`version`/`payload_len` は手動フォーマット、内側ペイロードは `bincode`
- Phase 3 でベンチを見て差し替え判断

### 5.7 FEC Strategy

- **Reed-Solomon、デフォルト `(k=8, m=2)`**(`reed-solomon-erasure` クレート)
- 1 フレーム単位で適用(跨がない、レイテンシ優先)
- 送信: k + m チャンクを同 socket から連続 sendto
- 受信: 同一 `frame_seq` チャンクを集め、k 個以上揃ったら復元
- `--fec=8,2` デフォルト、`--fec=off` でバイパス(Phase 0 で LAN 計測時比較)

### 5.8 Loss Detection & Response

Viewer reassembler:

```rust
struct FrameAssembler {
    frame_seq: u64,
    first_chunk_arrived_at: Instant,
    chunks: HashMap<u16, Vec<u8>>,
    expected_source: u16,
    expected_parity: u16,
    flags: VideoFlags,
}
```

- 最初のチャンク到着から **100ms** でタイムアウト、破棄 + IDR 要求
- 現在処理中 seq - 8 以前の古いフレームは受け取らない(遅延累積防止)

### 5.9 Congestion Control

- **Phase 0**: 静的 CBR、手動 `SetBitrate` のみ
- 計測: loss 率、RTT、ジッタを `tracing` 出力
- **自動調整は Phase 3**(BBR 風 or GCC 風を後で判断)

### 5.10 Security

- **Phase 0**: 暗号化なし
- `session_id` 検証のみ(不一致パケットは破棄、他人の UDP 到達に対する最低限の自己保護)
- **Phase 3 で Noise Protocol または QUIC (TLS 1.3) を被せる**

### 5.11 MTU Handling

- payload 1200B 固定(IPv4 MTU 1500 から IP/UDP/base/video ヘッダ引いた安全側)
- IP fragmentation 禁止(`IP_DONTFRAG`)
- `--mtu 1000` オプション(VPN 等の狭い環境向け)

### 5.12 Extensibility

- `frame_seq: u64` なので余裕
- 将来 `stream_id` は `flags` の上位 4bit に入れて拡張(ヘッダ変更なし、マルチモニタ対応の布石)

### 5.13 Transport Crate Public API

```rust
pub struct CustomUdpTransport {
    socket: Arc<tokio::net::UdpSocket>,
    session_id: u64,
    peer_addr: SocketAddr,
}

impl CustomUdpTransport {
    pub async fn bind(addr: SocketAddr) -> Result<Self>;
    pub async fn send_video(&self, frame: EncodedFrame) -> Result<()>;
    pub async fn send_input(&self, ev: InputEvent) -> Result<()>;
    pub async fn send_control(&self, msg: ControlMessage) -> Result<()>;
    pub async fn recv(&self) -> Result<WirePacket>;
}
```

- `send_video`: チャンク化 + FEC + sendto ループ
- `recv`: 1 UDP recv → ヘッダパース → 映像チャンクは reassembler、完成フレームは `WirePacket::Video` として返す

---

## 6. Error & Degradation Handling

### 6.1 Failure Mode Catalog

| # | モード | 場所 | 頻度 | 対応 |
|---|---|---|---|---|
| F1 | UDP 1〜2 パケット損失 | Net | 常時 | FEC 復元、ダメなら IDR |
| F2 | UDP 大量損失(フレーム丸ごと) | Net | ときどき | 破棄 + IDR |
| F3 | フレーム組立タイムアウト(100ms) | Viewer | 軽微 | 破棄 + IDR |
| F4 | NVENC エンコード失敗 | Host | 稀 | リトライ 1、失敗で encoder 再初期化 + IDR |
| F5 | NVDEC デコード失敗 | Viewer | 稀 | decoder フラッシュ + IDR |
| F6 | DXGI Duplication 失効 | Host | ときどき | duplication 再取得 + IDR |
| F7 | D3D11 デバイスロスト(TDR) | 両側 | 稀 | デバイス再作成、全パイプライン再起動 |
| F8 | ネット断(ping 3 秒無応答) | 両側 | ときどき | "Reconnecting"、10 秒で終了 |
| F9 | ネット復旧後再接続 | 両側 | F8 後 | Hello 再送で自動復帰 |
| F10 | MTU 不一致 | Viewer | 起動時 | Hello 段階で検出、エラー終了 |
| F11 | session_id 不一致 | 両側 | 悪意/誤配線 | 破棄、warn |
| F12 | プロトコルバージョン不一致 | Hello | 起動時 | エラー終了 + 明示メッセージ |
| F13 | MMCSS 取得失敗 | 両側 | 環境依存 | warn、通常優先度で続行 |
| F14 | 入力 stuck key | Host | 稀 | viewer フォーカス離脱で全 release |
| F15 | RawInput 取得失敗 | Viewer | 環境依存 | WM_MOUSE/WM_KEY フォールバック |

### 6.2 Recovery Levels

```
L0 通常
 ↓ F1, F14
L1 ソフトリカバリ: IDR 要求 / stuck key 送信。パイプライン維持
 ↓ F4, F5
L2 コンポーネント再初期化: NVENC or NVDEC のみ
 ↓ F6
L3 キャプチャ再接続: DXGI Duplication のみ再取得
 ↓ F7
L4 デバイス全再作成: D3D11 全コンポーネント再初期化
 ↓ F8
L5 セッション再確立: Hello やり直し
 ↓ 失敗
EXIT プロセス終了
```

単方向、下方にのみ進む。

### 6.3 Sample Recovery Code (Capture thread)

```rust
loop {
    let frame = match duplication.acquire_next_frame(Duration::from_millis(16)) {
        Ok(f) => f,
        Err(DxgiError::AccessLost) | Err(DxgiError::ModeChanged) => {
            tracing::warn!("duplication lost, re-acquiring");
            duplication = DesktopDuplication::acquire(&d3d_device)?;
            idr_flag.store(true, Ordering::Relaxed);
            continue;
        }
        Err(DxgiError::DeviceRemoved) => return Err(CaptureError::DeviceLost),
        Err(DxgiError::Timeout) => continue,
    };

    match nvenc.encode(frame, idr_flag.swap(false, ...)) {
        Ok(encoded) => {
            if encoded_tx.try_send(encoded).is_err() {
                idr_flag.store(true, Ordering::Relaxed);
            }
        }
        Err(NvEncError::Recoverable) => {
            nvenc.reset()?;
            idr_flag.store(true, Ordering::Relaxed);
        }
        Err(NvEncError::Fatal) => return Err(CaptureError::EncoderFatal),
    }
}
```

### 6.4 Metrics (tracing JSON 出力、1Hz)

- `fps_captured` / `encoded` / `received` / `decoded` / `presented`
- `bitrate_sent_bps` / `bitrate_recv_bps`
- `rtt_us` / `clock_offset_us`
- `udp_packets_sent` / `received` / `lost_estimated`
- `fec_recovered_frames` / `fec_failed_frames`
- `idr_requested_count` / `idr_sent_count`
- `glass_to_glass_p50/p95/p99_us` ← **最重要**
- `encode_ms_p50/p95` / `decode_ms_p50/p95`
- `frame_drops_by_reason`

出力: `tracing-subscriber` JSON フォーマッタで stderr、`--metrics-log path.jsonl` でファイル追記。

### 6.5 Debug Overlay (Viewer)

- `F1` トグル
- 表示: glass-to-glass、RTT、fps、bitrate、loss率、FEC 復元/失敗数
- 実装: DirectWrite 直叩き or `egui_directx11` 重ね(Phase 0 中に実装時選択)

### 6.6 Startup Validation

**Host**:
1. D3D11 device 作成(既定アダプタ)
2. NVENC 利用可否チェック → 不可ならエラー終了
3. DXGI Output 選択 + Duplication 取得
4. UDP bind
5. `listening on ...` ログ

**Viewer**:
1. D3D11 device 作成
2. NVDEC 利用可否チェック(コーデックサポート確認)
3. winit window + swapchain 作成
4. UDP socket 作成
5. Hello 送信 → 3 秒 × 3 回タイムアウトで終了

GPU 選択は `--adapter=N` オプション、マルチ GPU 対応は Phase 1 以降。

### 6.7 Crash Handling

- Rust panic = `abort`(Cargo プロファイル設定)
- Panic 時は tracing flush → 非ゼロ終了
- **クラッシュダンプ収集は Phase 0 範囲外**(Phase 4 で追加)

### 6.8 Explicit "Won't Do" (Phase 0)

- 自動再試行ループ
- パケット再送(TCP 的挙動)
- 適応的ビットレート変更(Phase 3)
- 複数モニタキャプチャ(Phase 1)
- HDR(Phase 1)
- クラッシュダンプ収集(Phase 4)

---

## 7. Testing Strategy

### 7.1 Test Categories

| カテゴリ | 保証対象 | 規模 |
|---|---|---|
| Unit | モジュールロジック(parse / FEC / reassembler / シリアライズ) | 中 |
| Integration (loopback) | クレート境界を跨ぐ動作 | 中 |
| Latency Benchmark | **4K60 LAN < 30ms 達成証明** | 小 |
| Performance / soak | 連続 1 時間ドロップ/リークなし | 小 |
| Manual smoke | 実機 2 台で体感 | 小 |

### 7.2 Unit Test Scope

- `protocol`: serialize/deserialize round-trip、異常値 parse、magic/version/session_id 検証
- `transport`: FEC encode/decode の網羅(k+m 組み合わせ、プロパティテスト)、reassembler タイムアウト、古い seq 破棄、MTU 超過エラー
- `media-win`: 内部 trait にモック(単色/カウンタ生成)、ロジックのみ検証
- `input-win`: InputEvent → `SendInput` 構造体変換、stuck key 検知タイマ

ツール:
- `cargo test` 標準
- `proptest` は FEC/reassembler のみ
- フェイク: `media-win` 内部 trait のモック実装

### 7.3 Integration Loopback Test

1 プロセス内で GPU 使用、NIC 不使用:

```
FakeCapture(色変化パターン) → NVENC(実) → InProcTransport(人工損失/遅延注入) →
NVDEC(実) → FakeRenderer(CPU 読み戻し + 値検証)
```

- ピクセル検証: 左上 16×16 に `frame_seq` をエンコード → デコード後読み戻し
- 摂動: `--drop-rate=0.05`、`--latency=20ms`
- 60fps × 60s = 3600 フレームを 1 分で検証

### 7.4 Latency Measurement Harness

**3 階層**(Recovery Levels L0〜L5 とは別概念、接頭辞 `M` で区別):

**M1: 内部タイムスタンプ**(常時)
- フレームごとに capture/encode/send/recv/fec/decode/present の `_us` を tracing で出力
- `host_to_present_us` = viewer present 時刻 - host capture 時刻 + clock_offset
- クロック精度 ±2ms 程度の系統誤差、ソフトウェア処理時間の目安として使う

**M2: 同一マシン loopback**
- Host と Viewer を同一物理マシン同一プロセスで起動(`latency-bench --mode=in-process`)
- クロック誤差ゼロ、ネット伝送 0ms
- **目標基準**: P95 `host_to_present_us` < 18ms

**M3: 真の glass-to-glass**

**M3a. カメラ同時撮影法**(Phase 0 必須、推奨):
- Host にミリ秒カウンタ全画面表示
- Viewer で表示される様子と Host を同時に 1 台のカメラ(スマホ 240fps スローモ)で撮影
- 動画から host 側数字 vs viewer 側数字の差を読む
- 20 フレームを手動サンプリング、中央値を出す
- 精度: 4.16ms、30 回計測で十分

**M3b. 専用ハードウェア**(任意):
- フォトダイオード + マイコン、点滅光量差から測定
- 精度 <1ms

**M3c. ベンダツール**(任意):
- NVIDIA LDAT / OSRTT

### 7.5 Benchmark Scenarios

| ID | 条件 | 期待値 |
|---|---|---|
| B1 | 1080p60, loopback, FEC off | M1/M2 p95 < 10ms |
| B2 | 1080p60, LAN 有線, FEC off | M1 p95 < 18ms、M3a 中央 < 25ms |
| B3 | 1080p60, LAN 有線, FEC (8,2) | M1 p95 < 20ms |
| B4 | 1440p60, LAN 有線, FEC off | M1 p95 < 20ms、M3a 中央 < 28ms |
| **B5** | **4K60, LAN 有線, FEC off** | **M1 p95 < 22ms、M3a 中央 < 30ms** ← **Phase 0 受入基準** |
| B6 | 4K60, LAN 有線, FEC (8,2), 5% 人工損失 | 目視「プレイ可能」、破綻せず継続 |
| B7 | 4K60, LAN Wi-Fi, FEC (8,2) | 参考値、基準外 |
| B8 | B5 条件で 1 時間連続 | ドロップ < 0.1%、メモリ安定、エラーログなし |

### 7.6 Load / Soak Testing

- B8: 1 時間連続実行、1 秒粒度メトリクスログ
- 途中で手動で: ウィンドウ切替、解像度変更、RDP 割込み → F6/F7 復帰観察
- リソース監視: `GetProcessMemoryInfo` で 10 秒毎 `WorkingSet` ログ

### 7.7 Manual Smoke (Phase 0 リリース判定前 30 分)

1. Host で 4K60 動画再生、Viewer で視聴(映像のみ、音声は Phase 3)
2. 軽めゲーム(OSU! 等)で入力主観評価
3. 長文 Web スクロール — 文字鮮明さ・破綻確認
4. マウス画面端での動作(座標クランプ、絶対座標変換の境界)
5. Alt-Tab / UAC / 解像度変更 → F6 復帰
6. Viewer Ctrl-C → Host 10 秒後待機復帰 → 再度 Viewer 起動で接続(F9)

### 7.8 CI Strategy

| テスト | GitHub Actions (`windows-latest`, no GPU) | Dev machine |
|---|---|---|
| Unit 全クレート | ✓ | ✓ |
| Integration loopback(fake) | ✓ | ✓ |
| Integration loopback(実 NVENC/NVDEC) | ✗ | ✓ |
| `latency-bench` M2 | ✗ | ✓ |
| `latency-bench` M3a | ✗ | ✓(手動、カメラ) |

- CI は 10 分以内、軽量テストのみ
- Self-hosted runner 導入は Phase 0 範囲外

### 7.9 Coverage Targets

- `protocol` / `transport`: >85% 行カバレッジ
- `media-win` / `input-win`: 重要ロジックのみユニットテスト、関数カバレッジで測定
- 全体: >60% 行カバレッジ
- ツール: `cargo-llvm-cov`

---

## Phase 0 Exit Criteria

以下**全部**を満たしたら Phase 0 完了:

- [ ] B1〜B5 のレイテンシベンチ期待値達成、**B5 で M3a カメラ実測 < 30ms 中央値**
- [ ] B6 で 5% ロス条件でも映像継続(破綻しない)
- [ ] B8 で 1 時間連続実行、メモリ安定、クラッシュなし
- [ ] 手動スモーク 6 項目すべて期待通り
- [ ] Unit/Integration テスト全パス
- [ ] カバレッジ目標達成(`protocol`/`transport` >85%、全体 >60%)
- [ ] 計測結果を `docs/superpowers/specs/phase0-benchmark-results.md` に記録

**未達の場合**: Phase 1 着手禁止。低遅延パイプラインが未証明のまま Linux 対応を始めない。

---

## Explicit Deferrals (Out of Scope for Phase 0)

| 領域 | 先送り先 |
|---|---|
| Linux / macOS / モバイル | Phase 1 以降 |
| Wayland 対応 | Phase 1 |
| マルチモニタ | Phase 3 |
| HDR | Phase 1(`Windows.Graphics.Capture` 切替と同時) |
| AMD / Intel GPU / ソフトウェアエンコーダ | Phase 3 or 後続 |
| AV1 エンコード | Phase 3 |
| WAN / NAT 越え / シグナリング / ID 接続 | Phase 2 |
| E2E 暗号化 / 認証 | Phase 3 |
| 音声、クリップボード、ファイル転送 | Phase 3 |
| 適応ビットレート、自動輻輳制御 | Phase 3 |
| ポリッシュ GUI、インストーラ、自動更新、コード署名 | Phase 4 |
| クラッシュダンプ収集 | Phase 4 |
| 公式リレー / ID サーバ運用 | Phase 5 |
| OSS 公開、ドキュメント整備 | Phase 5 |

---

## Glossary

- **Glass-to-glass latency**: Host 側画面更新(ピクセル発光)から Viewer 側画面反映(ピクセル発光)までの実時間
- **IDR**: Instantaneous Decoder Refresh。H.264/H.265 のキーフレーム。単独で復号開始可能
- **FEC**: Forward Error Correction。Reed-Solomon で冗長パリティを付加、受信側で欠損復元
- **MMCSS**: Multimedia Class Scheduler Service。Windows のマルチメディア向けスレッド優先度ブースト
- **DXGI Desktop Duplication**: Windows のデスクトップ画面キャプチャ API(`IDXGIOutputDuplication`)
- **NVENC / NVDEC**: NVIDIA GPU 上のハードウェアビデオエンコーダ / デコーダ
- **D3D11**: Direct3D 11、Windows の GPU API。本書では NV12/BGRA8 テクスチャの zero-copy パスで使用
- **TDR**: Timeout Detection and Recovery。Windows のグラフィックドライバが長時間無応答時の自動リセット
- **L0〜L5**: 本書 6.2 で定義する劣化/復帰レベル(Recovery Levels)
- **M1/M2/M3**: 本書 7.4 で定義するレイテンシ測定階層(Measurement tiers)
- **T4 目標**: 4K @ 60fps、LAN 遅延 <30ms(Phase 0 受入基準)

---

*End of Phase 0 Design.*
