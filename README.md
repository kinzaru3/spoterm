# spoterm

Spotify Web API + spotifyd で作る Spotify CLI アプリ。実装計画は [docs/PLAN.md](docs/PLAN.md)。

## 開発環境

ホスト(mac)を汚さないため、Rust 開発は Docker コンテナ内で行う。
spotifyd はホスト側で起動し、コンテナ内の spoterm とは Spotify クラウド経由で連携する。
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

## spotifyd（ホスト側）

音を出す再生デバイス。ホスト(mac)で起動する。設定は `~/.config/spotifyd/spotifyd.conf`。
Premium アカウントでログインして起動しておくと、spoterm から Connect デバイスとして選べる。
