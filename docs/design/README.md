# 詳細設計書インデックス

`docs/PLAN.md` のフェーズ計画に対する、機能単位の詳細設計書を置く。
実装の着手前に設計を書き、実装で判明した差分は同じ文書へ反映する（設計と実装を一致させ続ける）。

> **注記（再生デバイス）**: 以下の設計書には当時の検証環境である **spotifyd** への言及が残るが、
> 現在は再生デバイスを **mac の公式 Spotify アプリ（Connect デバイス）に確定**し、spotifyd（librespot
> ベースの非公式クライアント）は**スコープ外**とした。設計上「spotifyd」と書かれた対象デバイスは
> 「公式 Spotify アプリ」に読み替える（`transfer_playback` の対象が変わるだけで、コード・API は同一）。
> 経緯は [../PLAN.md](../PLAN.md) の「追加要望・設計メモ」を参照。

## Phase 3 — 読み取り系コマンド

| 文書 | 対象 | 概要 |
| --- | --- | --- |
| [auth-client.md](./auth-client.md) | `auth::authed_client` | キャッシュ済みトークンを読み、認証済みクライアントを返す共通ヘルパ |
| [status.md](./status.md) | `spoterm status` | Now Playing（曲/アーティスト/進捗/デバイス）表示 |
| [search.md](./search.md) | `spoterm search <query>` | track/album/artist を検索して一覧表示 |
| [devices.md](./devices.md) | `spoterm devices` | 利用可能デバイス一覧（spotifyd が見えるかの実地検証を含む） |

## Phase 4 — 再生コントロール

| 文書 | 対象 | 概要 |
| --- | --- | --- |
| [playback.md](./playback.md) | `play`/`pause`/`next`/`prev`/`toggle`/`vol` | アクティブデバイスへの再生操作 |
| [device-use.md](./device-use.md) | `spoterm device use <name>` | 指定デバイス（spotifyd 等）へ再生をトランスファー |

## Phase 5 — プレイリスト & ライブラリ

| 文書 | 対象 | 概要 |
| --- | --- | --- |
| [playlist.md](./playlist.md) | `spoterm playlist ls\|play` | プレイリスト一覧・名前指定で再生 |
| [lib.md](./lib.md) | `spoterm lib` | 保存済みトラック/アルバム一覧（読み取り専用） |

## 共通ヘルパ

| 文書 | 対象 | 概要 |
| --- | --- | --- |
| [match-name.md](./match-name.md) | `match_name`（`src/match_name.rs`） | 名前照合（完全一致優先→部分一致）。`device use` / `playlist play` で共用 |

## 共通方針

- **純粋関数を分離してテストする**: API 応答（rspotify のモデル型）を組み立てるのはテストで扱いにくいため、
  表示整形は「プリミティブ（`&str`/数値）を受け取り `String` を返す純粋関数」に切り出し、単体テストの対象にする。
  コマンド本体は「API 呼び出し → モデルをプリミティブへ写像 → 整形関数」を繋ぐ薄い層に留める。
- **空状態を明示する**: 再生なし / ヒット 0 / デバイス 0 は、黙って何も出さず必ずメッセージを出す（silent failure を作らない）。
- **副作用なし**: Phase 3 は読み取り専用。再生状態を変えない（`play`/`vol` 等は Phase 4）。
- **モジュール構成**: `src/commands/{status,search,devices}.rs` にコマンド、`src/format.rs` に横断的な整形関数。
