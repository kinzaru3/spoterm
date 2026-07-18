# 詳細設計: `spoterm status`

## 目的

現在の再生状況（Now Playing）を表示する。曲名・アーティスト・アルバム・進捗・再生中デバイスを一目で確認できる。

## 呼び出し元 / 依存

- `src/main.rs` の `Command::Status` から `commands::status::run(&cfg).await?` を呼ぶ。
- `auth::authed_client` で認証済みクライアントを取得。
- `format::{format_ms, join_artists}` を整形に使う。

## 使用 API

`current_playback(country, additional_types)` → `Option<CurrentPlaybackContext>`（`GET /me/player`）

- 引数は両方 `None`（マーケットはトークンのユーザー国が優先されるため不要、追加タイプも既定の track のみ）。
- `None`（=再生セッションなし）と、`is_playing: false`（=一時停止中）を区別して扱う。

## 参照するモデル項目

`CurrentPlaybackContext`:
- `is_playing: bool`
- `progress: Option<Duration>`
- `device: Device`（`name`, `volume_percent`）
- `item: Option<PlayableItem>` → `PlayableItem::Track(FullTrack)` を主対象
  - `FullTrack`: `name`, `artists: Vec<SimplifiedArtist>`（`name`）, `album.name`, `duration: Duration`
  - `PlayableItem::Episode` / `Unknown` はフォールバック表示（タイトルのみ or 「(対応外の再生アイテム)」）

## 出力仕様

```
▶ 再生中   （is_playing=true。false なら「⏸ 一時停止」）
  曲       : <track name>
  アーティスト: <artist1, artist2>
  アルバム : <album name>
  進捗     : 1:23 / 3:07
  デバイス : <device name> (vol 65%)
```

- 再生セッションなし: `再生中の曲はありません（spoterm play で再生を開始できます）`
- Episode: 曲/アルバム行の代わりにエピソード名と番組名。
- `progress` や `volume_percent` が `None` の場合は該当項目を `-` 表示。

## 純粋関数（テスト対象）

`src/format.rs`:
- `format_ms(ms: u128) -> String` … ミリ秒を `m:ss`（例 `187000 -> "3:07"`, `0 -> "0:00"`, 60分超は `mm:ss` 継続）。
- `join_artists(names: &[String]) -> String` … `", "` 連結。空なら `"(不明なアーティスト)"`。

`src/commands/status.rs`:
- `render_track(name, artists, album, progress_ms, duration_ms, device, vol, playing) -> String`
  … プリミティブのみを受け取り表示ブロックを組み立てる純粋関数。API 応答からの写像はコマンド本体で行う。

## テスト

- `format_ms`: `0 -> "0:00"`, `5000 -> "0:05"`, `65000 -> "1:05"`, `187000 -> "3:07"`, `3_600_000 -> "60:00"`。
- `join_artists`: 単数 / 複数 / 空。
- `render_track`: 再生中/一時停止、進捗あり/なしの分岐で期待文字列を検証。
