# spoterm

Spotify Web API で作る Spotify CLI アプリ。実装計画は [docs/PLAN.md](docs/PLAN.md)。

再生（音を出す部分）は **mac の公式 Spotify アプリ**を Spotify Connect デバイスとして使う。
spoterm は公式 Web API 経由で「そのデバイスで再生して」と指示するだけで、音源のダウンロードや
再生エンジンの実装は行わない（100% 公式 API 構成）。

## 開発環境

ホスト(mac)を汚さないため、Rust 開発は Docker コンテナ内で行う。
コンテナ内の spoterm とホストの公式 Spotify アプリは Spotify クラウド経由で連携する。
コンテナ内で編集する場合のエディタは `vim`。

### 初回セットアップ

```sh
cp .env.example .env      # SPOTERM_CLIENT_ID を自分の Client ID に書き換える
docker compose build
docker compose up -d
```

> Rancher Desktop 利用時、`docker` が見つからない場合は `~/.rd/bin` を PATH に追加。

### コンテナに入る / 使う

```sh
docker compose exec dev bash     # シェル
docker compose exec dev vim      # コンテナ内で編集する場合
docker compose exec dev cargo build
```

コンテナ内で使えるもの: `cargo` / `rustc` / `rust-analyzer` / `clippy` / `rustfmt`、
`vim`、`rg` / `fd`。
ビルド成果物・cargo キャッシュは named volume に隔離される（ホスト非汚染）。

### 停止

```sh
docker compose down          # コンテナ停止（volume は保持）
docker compose down -v       # volume も削除（プラグイン等をリセット）
```

## 再生デバイス（ホスト側の公式 Spotify アプリ）

音を出す再生デバイスには **mac の公式 Spotify アプリ**を使う。Premium アカウントでログインして
起動しておくと、spoterm の `devices` 一覧や TUI のデバイス選択（`d`）に Connect デバイスとして現れ、
`transfer_playback` で再生を移せる。出力先（スピーカー/AirPods 等）は公式アプリ側の設定に従う。

> **spotifyd はスコープ外**：以前は再生デバイスに spotifyd（librespot ベースの非公式クライアント）を
> 使う構成も検討したが、公式アプリで足りるため採用しない。spoterm 自体は librespot に一切依存しない。
