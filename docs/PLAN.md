# spoterm 実装プラン

Spotify Web API + spotifyd を使った Spotify CLI アプリ。

## 決定事項（ヒアリング結果）

| 項目 | 決定 |
| --- | --- |
| 言語 | **Rust**（※ローカル未インストール → Phase 0 で導入） |
| 機能スコープ | 再生コントロール / 検索して再生 / Now Playing 表示 / プレイリスト管理 / ライブラリ一覧・再生 |
| UI 形式 | **段階的**（まずワンショットコマンド → 後で対話型 TUI） |
| 配布 | 将来的に公開（OSS 前提の設計にする） |
| アカウント | Spotify **Premium**（再生制御 API・spotifyd 再生が可能） |
| Dev アプリ | 登録済み（Client ID 取得済み） |

## アーキテクチャ

```
[あなた] --キー操作--> [spoterm CLI/TUI] --Web API--> [Spotify サーバ]
                                                          |
                                                  再生指示(device_id)
                                                          v
                                              [spotifyd = Connect デバイス] --> 🔊
```

- **spotifyd** はバックグラウンドで動く「再生デバイス」。spoterm は Web API 経由で
  `spotifyd` デバイスへ再生をトランスファーして音を鳴らす。
- 認証は **Authorization Code + PKCE**（Client Secret を端末に置かなくてよい＝公開向き）。

## 開発環境（Docker）

ホスト(mac)を汚さないため、Rust の開発・ビルドは **Docker コンテナ内**で行う。
spotifyd と spoterm は Spotify クラウド経由で連携するため、この分離が可能。

```
[ホスト mac]                          [Docker コンテナ (Linux)]
  spotifyd (brew, 音を出す) 🔊          rust toolchain + spoterm 開発
  ブラウザ (OAuth 同意画面)             neovim (ホストの設定を持ち込み)
        ↕ Spotify クラウド ↕                    ↕
        └──────── どちらも api.spotify.com と通信 ────────┘
```

- **spotifyd はホスト(mac)で起動**。コンテナ内だと mac のスピーカーに音を出せないため。
- **Claude(Bash)からは `docker compose exec` で操作**する。
- 形式は **docker-compose + Dockerfile**。compose でマウント・ポート・named volume を宣言。
- **nvim は設定のみ持ち込み**：`~/.config/nvim` をマウント。プラグインは OS/arch 差で
  mac のものを共有できないため、コンテナ内に新規導入し `named volume` に隔離する。

compose の主な内容（想定）:
| 項目 | 設定 |
| --- | --- |
| project mount | `./ → /workspace`（ソース） |
| nvim config | `~/.config/nvim → /root/.config/nvim`（ro） |
| nvim data | named volume `/root/.local/share/nvim`, `/root/.local/state/nvim` |
| cargo cache | named volume `/usr/local/cargo`（再ビルド高速化） |
| OAuth port | `127.0.0.1:8888:8888`（PKCE の redirect をホストブラウザから転送） |

## 技術選定（Rust クレート）

| 用途 | クレート |
| --- | --- |
| Spotify API クライアント | `rspotify`（PKCE 認証・トークンキャッシュ対応） |
| 非同期ランタイム | `tokio` |
| CLI 引数パース | `clap`（derive） |
| 設定ファイル | `directories` + `toml` / `serde` |
| 認証情報の保存 | トークンは rspotify のキャッシュ、機密は `keyring`（OS キーチェーン） |
| エラー処理 | `anyhow` / `thiserror` |
| TUI（Phase 6） | `ratatui` + `crossterm` |
| テスト（API モック） | `wiremock` |

## コマンド設計（ワンショット期）

```
spoterm login              # PKCE 認証（ブラウザ起動→ローカルで redirect を受け取る）
spoterm status            # Now Playing（曲名/アーティスト/進捗）
spoterm search <query>    # 曲/アルバム/アーティスト検索
spoterm play [query]      # 検索 or 再開して再生
spoterm pause | next | prev | toggle
spoterm vol <0-100>
spoterm devices           # 利用可能デバイス一覧（spotifyd を含む）
spoterm device use <name> # spotifyd へ再生をトランスファー
spoterm playlist ls | play <name>
spoterm lib               # 保存済みトラック/アルバム一覧・再生
```

## フェーズ計画

### Phase 0 — 環境セットアップ
- [ ] `Dockerfile`（Rust ベースイメージ + neovim + ビルド依存）
- [ ] `docker-compose.yml`（上表のマウント・ポート・named volume）
- [ ] コンテナ起動 & `docker compose exec` で shell / nvim が使えることを確認
- [ ] ホスト側 `spotifyd` の設定（`~/.config/spotifyd/spotifyd.conf`、Premium ログイン、起動確認）
- [ ] Spotify Dashboard で **Redirect URI** に `http://127.0.0.1:8888/callback` を追加
- [ ] Client ID を環境変数（`.env`）or 設定ファイルへ

### Phase 1 — プロジェクト骨組み ✅
- [x] `cargo init`、依存追加（clap/anyhow/serde/toml/directories, edition 2024）
- [x] `clap` で全サブコマンド構造（`src/cli.rs`）＋ 未実装スタブ（`src/main.rs`）
- [x] 設定ローダ（`src/config.rs`：`SPOTERM_CLIENT_ID` / `SPOTERM_REDIRECT_URI` を env から、
      client_id はマスク表示、XDG 設定ディレクトリ）
- [x] `cargo build` / `--help` / `login`(config疎通) / `vol` 範囲(0-100) / clippy 警告0 を検証

### Phase 2 — 認証（PKCE）✅
- [x] `spoterm login`：認可URL → ローカル 8888 で redirect 捕捉 → トークン取得・キャッシュ（`src/auth.rs`）
- [x] トークン自動リフレッシュ（`token_cached`＋refresh_token 取得済み。読み込みヘルパは Phase 3）
- [x] セキュリティ強化（state 照合 / token.json 0600 / timeout・パス検証・上限読取）＋テスト10件
- [x] 実地ログイン成功（rspotify 0.16.1・PKCE・rustls）

### Phase 3 — 読み取り系コマンド（Premium 無関係で安全）
- [ ] `status` / `search` / `devices`

### Phase 4 — 再生コントロール
- [ ] `play` / `pause` / `next` / `prev` / `toggle` / `vol`
- [ ] `device use` で spotifyd へトランスファー

### Phase 5 — プレイリスト & ライブラリ
- [ ] `playlist ls|play` / `lib`（保存済みトラック・アルバム）

### Phase 6 — TUI 化
- [ ] `ratatui` で Now Playing 画面・検索・選曲・キー操作

### Phase 7 — テスト & 配布
- [ ] `wiremock` で API モックの単体テスト、CI（GitHub Actions）
- [ ] リリースバイナリ（GitHub Releases）、`cargo install`、将来的に Homebrew tap

## 次の一手
Phase 0 の環境セットアップから着手する（Rust 導入 → Redirect URI 追加 → spotifyd 起動確認）。
