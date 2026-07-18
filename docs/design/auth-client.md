# 詳細設計: `auth::authed_client`

## 目的

キャッシュ済みトークン（`token.json`）を読み込み、**認証済みの `AuthCodePkceSpotify`** を返す共通ヘルパ。
Phase 3 以降の全 API コマンド（`status`/`search`/`devices`/…）がこれを入口に使う。

## 呼び出し元

- `src/commands/status.rs`, `src/commands/search.rs`, `src/commands/devices.rs`（Phase 3）
- Phase 4 以降の再生系コマンドも同じヘルパを再利用する。

## シグネチャ

```rust
// src/auth.rs
pub async fn authed_client(cfg: &Config) -> Result<AuthCodePkceSpotify>
```

## 処理フロー

1. `build_client(cfg)?` で未認証クライアントを組み立てる（`token_cached: true`、`token_refreshing` は既定 `true`）。
2. `spotify.read_token_cache(true).await?` でキャッシュを読む。
   - `allow_expired = true`：期限切れでも refresh_token を得るために読み込む。
   - スコープがキャッシュに含まれない場合や未ログイン時は `None` が返る。
3. `None` の場合は「未ログインです。先に `spoterm login` を実行してください」を返して終了（親切なエラー）。
4. 取得した `Token` をクライアントへセット：`*spotify.get_token().lock().await = Some(token);`
5. 返す。以降の API 呼び出しで期限切れなら `token_refreshing: true` により自動リフレッシュされる
   （PKCE の refresh は `client_id` のみで可能。`build_client` で設定済み）。

## トークンファイル権限（0600）の維持

- `login` 時に `token.json` を `0600` に設定済み。
- 自動リフレッシュ時 rspotify は `write_token_cache` で**既存ファイルへ書き込む**（`O_TRUNC`）。
  POSIX では既存ファイルへの truncate 書き込みは mode を保持するため、`0600` は維持される。
- → 追加の chmod は不要。ただしレビューで懸念が出れば再検討する（設計判断としてここに記録）。

## エラーハンドリング

| 状況 | 挙動 |
| --- | --- |
| 未ログイン（キャッシュなし/スコープ不足） | `bail!` で再ログインを促すメッセージ |
| キャッシュ読込 I/O エラー | `.context()` を付けて伝播 |
| リフレッシュ失敗（API 呼び出し時） | 各コマンドの `?` で伝播、原因を `.context()` 付与 |

## テスト方針

- `authed_client` 自体は I/O（ファイル・時計）に依存するため単体テストは薄い。
  未ログイン時に分かりやすいエラーを返すことを、キャッシュ不在の一時 config ディレクトリで確認する
  （実 API は叩かない）。
- 実 API 疎通は各コマンドの手動検証と Phase 7 の `wiremock` に委ねる。
