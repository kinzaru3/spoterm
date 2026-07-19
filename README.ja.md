# spoterm

[English](README.md) | **日本語**

公式 Spotify Web API で作る、ターミナル向けの高速な Spotify CLI / TUI。

spoterm は再生コントロール・検索・ライブラリ閲覧・ライブな「Now Playing」表示（対応端末では
**アルバムのカバーアート**付き）を行います。認証は **Authorization Code + PKCE**（端末に client
secret を置かない）で、通信は公式 Web API のみ。音源のダウンロードや Spotify SDK の同梱は行いません。

再生（音を出す部分）は、あなたが動かしている **Spotify Connect デバイス**（公式 Spotify アプリ）で
行われ、spoterm はそのデバイスに「何を再生するか」を指示するだけです。

## 機能

- **Now Playing TUI**（`spoterm tui`）：曲/アーティスト/アルバム・進捗バー・音量・**カバーアート**。
- **再生コントロール**：再生 / 一時停止 / 次 / 前 / シーク / 音量（CLI・TUI 両方）。
- **検索して再生**。
- **ライブラリ閲覧・再生**：プレイリスト / 保存トラック / 保存アルバム。
- **デバイス選択**：Connect デバイス一覧から再生を転送。
- **お気に入り**：現在曲をライブラリに保存 / 解除。

## 必要なもの

- **Spotify Premium**（Web API の再生制御に必須）。
- 再生先の **Spotify Connect デバイス**（同一アカウントでログイン・起動中の公式 Spotify アプリ）。
- **自分の Spotify アプリの Client ID**（無料・下記セットアップ参照）。各ユーザーが自分のアプリを登録します。
- ビルドする場合は Rust ツールチェーン（Rust 1.85+ / edition 2024）。
- 実画像のカバーアートには画像プロトコル対応端末（iTerm2 / kitty / WezTerm / Ghostty）。
  非対応端末では自動的にカラー半ブロックにフォールバックします。

## インストール

ソースからビルド（crates.io 公開までの間）:

```sh
cargo install --git https://github.com/kinzaru3/spoterm
# または
git clone https://github.com/kinzaru3/spoterm && cd spoterm && cargo install --path .
```

## セットアップ

1. [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) で **アプリを作成**し、
   **Client ID** を控える。
2. アプリ設定で以下の **Redirect URI** を追加:
   ```
   http://127.0.0.1:8888/callback
   ```
3. **Client ID を環境変数で指定**（または `.env` ファイル。`.env.example` 参照）:
   ```sh
   export SPOTERM_CLIENT_ID=あなたの_client_id
   # 任意（既定は http://127.0.0.1:8888/callback）
   # export SPOTERM_REDIRECT_URI=http://127.0.0.1:8888/callback
   ```
4. **ログイン**（ブラウザで同意 → トークンはローカルにキャッシュ）:
   ```sh
   spoterm login
   ```

> **なぜ自分の Client ID？** Spotify の development mode は 1 アプリのユーザー数を制限するため、
> 各ユーザーが自分の登録アプリで spoterm を使います。PKCE なので client secret は不要で、
> トークンは OS の設定ディレクトリに `0600` で保存されます。

## 使い方

### ワンショットコマンド

```sh
spoterm status                 # Now Playing（曲/アーティスト/進捗/デバイス）
spoterm search <query>         # トラック/アルバム/アーティスト検索
spoterm play [query]           # 再開、または検索して再生
spoterm pause | next | prev | toggle
spoterm vol <0-100>            # 音量設定
spoterm devices                # 利用可能な Connect デバイス一覧
spoterm device use <name>      # 指定デバイスへ再生を転送
spoterm playlist ls            # プレイリスト一覧
spoterm playlist play <name>   # 名前指定でプレイリスト再生
spoterm lib                    # 保存トラック / アルバム一覧
```

### 対話型 TUI

```sh
spoterm tui
```

| キー | 動作 |
|---|---|
| `space` | 再生 / 一時停止 |
| `n` / `p` | 次 / 前の曲 |
| `←` / `→` | 5 秒シーク（戻る / 進む） |
| `+` / `-` | 音量 ±5 |
| `s` | 現在曲を保存 / 解除 |
| `/` | 検索して再生 |
| `2` | ライブラリ閲覧（プレイリスト / 保存トラック / アルバム） |
| `d` | デバイス選択（再生を転送） |
| `r` | 更新 |
| `?` | ヘルプ |
| `q` / `Esc` / `Ctrl-C` | 終了 |

## カバーアート

TUI は端末が対応する最適なプロトコル（kitty / iTerm2 / Sixel）でアルバムアートを描画し、
非対応端末ではカラー半ブロックにフォールバックします（必ず何か表示されます）。

**tmux 内**では passthrough を有効にしないと画像プロトコルが落とされます。実画像を出すには
tmux 設定に以下を追加してください:

```tmux
set -g allow-passthrough on
```

## 注意

- **個人・非商用利用**。spoterm は公開 Spotify Web API のクライアントであり、Spotify の SDK・
  コンテンツ・client secret を再配布しません。
- カバーアートとトラックメタデータは併せて表示します（Spotify 開発者ガイドラインに準拠）。

## ライセンス

[MIT](LICENSE) © kinzaru3

本プロジェクトは Spotify とは無関係で、Spotify による承認も受けていません。「Spotify」は Spotify AB の商標です。
