# 詳細設計書インデックス

`docs/PLAN.md` のフェーズ計画に対する、機能単位の詳細設計書を置く。
実装の着手前に設計を書き、実装で判明した差分は同じ文書へ反映する（設計と実装を一致させ続ける）。

## Phase 3 — 読み取り系コマンド

| 文書 | 対象 | 概要 |
| --- | --- | --- |
| [auth-client.md](./auth-client.md) | `auth::authed_client` | キャッシュ済みトークンを読み、認証済みクライアントを返す共通ヘルパ |
| [status.md](./status.md) | `spoterm status` | Now Playing（曲/アーティスト/進捗/デバイス）表示 |
| [search.md](./search.md) | `spoterm search <query>` | track/album/artist を検索して一覧表示 |
| [devices.md](./devices.md) | `spoterm devices` | 利用可能デバイス一覧（spotifyd が見えるかの実地検証を含む） |

## 共通方針

- **純粋関数を分離してテストする**: API 応答（rspotify のモデル型）を組み立てるのはテストで扱いにくいため、
  表示整形は「プリミティブ（`&str`/数値）を受け取り `String` を返す純粋関数」に切り出し、単体テストの対象にする。
  コマンド本体は「API 呼び出し → モデルをプリミティブへ写像 → 整形関数」を繋ぐ薄い層に留める。
- **空状態を明示する**: 再生なし / ヒット 0 / デバイス 0 は、黙って何も出さず必ずメッセージを出す（silent failure を作らない）。
- **副作用なし**: Phase 3 は読み取り専用。再生状態を変えない（`play`/`vol` 等は Phase 4）。
- **モジュール構成**: `src/commands/{status,search,devices}.rs` にコマンド、`src/format.rs` に横断的な整形関数。
