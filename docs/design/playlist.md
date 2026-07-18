# 詳細設計: `spoterm playlist ls|play`

## 目的

ログインユーザーのプレイリストを一覧表示（`ls`）し、名前を指定して再生開始する（`play`）。
再生は Phase 4 と同じく「アクティブデバイスへ送る」方式（`device_id=None`）。事前に
`device use <name>`（[device-use.md](./device-use.md)）でデバイスを選んでおく。

## 呼び出し元 / 依存

- `src/main.rs` の `Command::Playlist { action }` → `commands::playlist::{ls, play}`。
- `auth::authed_client` を使用。
- 名前照合は共通ヘルパ `match_name`（[match-name.md](./match-name.md)）を使う（`device use` と同じロジック）。
- 表示整形は `format::render_entry`（search / lib と共通）。

## 使用 API

- `current_user_playlists_manual(limit: Option<u32>, offset: Option<u32>) -> Page<SimplifiedPlaylist>`
  - `SimplifiedPlaylist`: `name: String`, `id: PlaylistId<'static>`, `items: PlaylistTracksRef { total: u32 }`。
    `tracks` は deprecated のため曲数は `items.total` を使う。
- `start_context_playback(context: PlayContextId, device_id, offset, position)`
  - プレイリスト再生は `PlayContextId::Playlist(id)` を渡す。`device_id=None`（アクティブデバイス）。

## ページング方針

- 1 ページ最大 50 件（API 上限）。`ls`/`play` とも先頭 50 件のみ扱う（KISS）。
- `Page.total` が取得件数を超える場合は「先頭 N 件のみ表示（全 M 件）」と明示する。
  `play` で該当なし かつ total>取得数 のときも、全件を見ていない旨を添える。

## `ls` 出力仕様

- 0 件: `プレイリストがありません` を出す（silent failure 禁止）。
- 各行: `format::render_entry(index, name, "<total>曲", uri)`
  例 `  1. My Mix  —  120曲    spotify:playlist:xxxxx`
- 末尾に総数の注記（表示件数 < 全件 のときのみ）。

## `play <name>` 出力仕様

- `match_name` の結果で分岐:
  - `Found(i)`: `start_context_playback(Playlist(id))` → `▶ 再生: <name>`。
    アクティブデバイスが無いと API が失敗するため、失敗時は `device use` を促すヒントを添える。
  - `None`: `'<query>' に一致するプレイリストがありません。spoterm playlist ls で確認してください`
    （先頭 50 件しか見ていない場合はその旨も添える）
  - `Ambiguous(idxs)`: 候補名を列挙し、より具体的な指定を促す。

## テスト

- 純粋関数のみを単体テスト（実 API はモックしない方針を踏襲）:
  - `format::render_entry`（format）: subtitle 有無の 2 系統。
  - `match_name`（[match-name.md](./match-name.md)）: 完全/部分/曖昧/該当なし。
  - `playlist::no_match_message`: 先頭ページのみ照合した場合の注記有無。
- 実 API の再生確認は [manual-tests.md](../manual-tests.md) に手順を追加。
