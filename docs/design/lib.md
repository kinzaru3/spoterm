# 詳細設計: `spoterm lib`

## 目的

ログインユーザーのライブラリ（保存済みトラック・保存済みアルバム）を一覧表示する。
Phase 5 の時点では**読み取り専用**（再生は `play <query>` / `playlist play` で行う）。

## 呼び出し元 / 依存

- `src/main.rs` の `Command::Lib` → `commands::lib::run`。
- `auth::authed_client` を使用。
- 表示整形は `format::render_entry`（search / playlist と共通）。

## 使用 API

- `current_user_saved_tracks_manual(market, limit, offset) -> Page<SavedTrack>`
  - `SavedTrack.track: FullTrack`（`name`, `artists: Vec<SimplifiedArtist>{name}`, `id: Option<TrackId>`）
- `current_user_saved_albums_manual(market, limit, offset) -> Page<SavedAlbum>`
  - `SavedAlbum.album: FullAlbum`（`name`, `artists: Vec<SimplifiedArtist>{name}`, `id: AlbumId`）
- `market` は `None`（トークンのユーザー国を利用）。

## ページング方針

- トラック・アルバムとも先頭 20 件のみ表示（KISS）。`Page.total` が超える場合は
  「先頭 N 件（全 M 件）」と各セクション見出しに明示する。

## 出力仕様

- search と同じ 2 セクション表示:
  - `🎵 保存済みトラック` … `render_entry(i, track.name, join_artists, uri)`
  - `💿 保存済みアルバム` … `render_entry(i, album.name, join_artists, uri)`
- URI が無いトラック（ローカル曲など）は URI 空で表示（`render_entry` が空 URI を許容）。
- 両方 0 件: `ライブラリに保存済みのトラック/アルバムはありません` を出す（silent failure 禁止）。
  片方だけ 0 件のセクションは見出しごと省略する。

## テスト

- 純粋関数のみ単体テスト（実 API はモックしない方針）:
  - `format::render_entry`（format 側でテスト済み）。
- 実 API の確認は [manual-tests.md](../manual-tests.md) に手順を追加。
