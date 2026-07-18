# 詳細設計: `spoterm devices`

## 目的

Spotify Connect の**利用可能デバイス一覧**を表示する。Phase 4 の `device use <name>`（spotifyd へ再生転送）の前提。
本コマンドで、懸案の「**ホストの spotifyd (`MacBook-spotifyd`) が Web API のデバイス一覧に出るか**」を実地検証する。

## 呼び出し元 / 依存

- `src/main.rs` の `Command::Devices` から `commands::devices::run(&cfg).await?`。
- `auth::authed_client` を使用。

## 使用 API

`device()` → `Vec<Device>`（`GET /me/player/devices`）

`Device`:
- `id: Option<String>`
- `is_active: bool`
- `name: String`
- `_type: DeviceType`（`Computer`/`Smartphone`/`Speaker`/…）
- `volume_percent: Option<u32>`
- `is_restricted: bool`（true だと Web API から操作不可）

## 出力仕様

```
利用可能なデバイス:
  ● <name> [Computer]  vol 65%   (active)
  ○ <name> [Speaker]   vol 40%
```

- `is_active` を `●`/`○` で示す。
- `volume_percent = None` は `vol -`。
- `is_restricted = true` は末尾に `(操作不可)` を付す。
- **デバイス 0 件**: `再生可能なデバイスがありません。Spotify アプリまたは spotifyd を起動してください` を出す。

## 純粋関数（テスト対象）

`src/commands/devices.rs`:
- `render_device(name, type_label, vol, is_active, is_restricted) -> String`
  … プリミティブから 1 行を組み立てる純粋関数。`DeviceType` → ラベルの写像はコマンド本体で行う。

## テスト

- `render_device`: active/非 active、vol あり/なし、restricted の各分岐で期待文字列を検証。

## 実地検証（このフェーズの調査事項）— 検証済み ✅

`cargo run -- devices` を実行して確認した結果（2026-07-18）:

```
利用可能なデバイス:
  ○ saitoshingoのMacBook Pro [Computer]  vol 61%
  ○ MacBook-spotifyd [Computer]  vol 89%
```

- **結論: `MacBook-spotifyd` は Web API の devices 一覧に出る**。discovery(zeroconf) 方式のままで可視。
  → 公式アプリでの事前アクティブ化や OAuth 方式への切替は**不要**。
- 両デバイスとも非アクティブ（`○`）なのは「何も再生していない」状態と整合（`is_active=false`）。
- **Phase 4 の前提として確定**：`device use MacBook-spotifyd` は取得済み `id` へ
  `transfer_playback` すれば実現できる見込み。
