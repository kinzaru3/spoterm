# 詳細設計: `spoterm search <query>`

## 目的

キーワードで track / album / artist を検索し、種別ごとに上位ヒットを一覧表示する。
Phase 4 の `play <query>`（検索して再生）へ渡す URI をユーザーが確認できる土台にもなる。

## 呼び出し元 / 依存

- `src/main.rs` の `Command::Search { query }` から `commands::search::run(&cfg, &query).await?`。
  `query: Vec<String>` は空白で join して 1 本のクエリ文字列にする（clap で `required=true`）。
- `auth::authed_client` を使用。
- `format::truncate` で長い名称を切り詰める。

## 使用 API

`search(q, SearchType, market, include_external, limit, offset)` を種別ごとに 3 回、または
`search_multiple(q, [Track, Album, Artist], ...)` で 1 回。→ **`search_multiple` を採用**（往復 1 回で済む）。

- `market`: `None`（ユーザー国が優先）。
- `include_external`: `None`。
- `limit`: `SEARCH_LIMIT = 5`（2026-02 以降 API 上限は 10、既定 5。控えめに 5）。
- `offset`: `None`。

戻り値 `SearchMultipleResult` の各 `Option<Page<...>>`：
- `tracks: Page<FullTrack>`（`name`, `artists`, `uri`）
- `albums: Page<SimplifiedAlbum>`（`name`, `artists`, `uri`）
- `artists: Page<FullArtist>`（`name`, `uri`）

## 出力仕様

```
🎵 Tracks
  1. <track>  —  <artists>            spotify:track:...
  2. ...
💿 Albums
  1. <album>  —  <artists>            spotify:album:...
🎤 Artists
  1. <artist>                          spotify:artist:...
```

- 各種別ヒット 0 件のときはその見出しをスキップ、全種別 0 件なら
  `"<query>" に一致する結果はありませんでした` を出す。
- 名称は端末幅を意識して `truncate(name, 40)` 程度で省略（`…`）。

## 純粋関数（テスト対象）

`src/format.rs`:
- `truncate(s: &str, max: usize) -> String` … `max` を超えたら末尾を `…` に。境界（ちょうど / 短い / 超過）を検証。
  マルチバイトを壊さないよう `chars()` 単位で数える。

`src/commands/search.rs`:
- `render_line(index, title, subtitle, uri) -> String` … 1 行分を整形する純粋関数。
  `subtitle`（アーティスト連結）が空なら省略。

## テスト

- `truncate`: `("hello", 10) -> "hello"`, `("hello", 5) -> "hello"`, `("hello", 4) -> "hel…"`, マルチバイト。
- `render_line`: subtitle あり/なしの 2 ケース。
