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

### Phase 3 — 読み取り系コマンド（Premium 無関係で安全）✅
副作用のない読み取り専用コマンド。詳細設計は [docs/design/](./design/README.md) を参照。
- [x] `auth::authed_client`：キャッシュ済みトークンを読む認証済みクライアント共通ヘルパ（[設計](./design/auth-client.md)）
- [x] `status`：Now Playing 表示（[設計](./design/status.md)）
- [x] `search`：track/album/artist 検索（[設計](./design/search.md)）
- [x] `devices`：デバイス一覧（[設計](./design/devices.md)）＋ spotifyd が Web API に見えるか実地検証（**可視を確認**）
- [x] 整形の純粋関数（`src/format.rs`）に単体テスト、`fmt`/`clippy -D warnings` 通過、実 API 疎通確認

### Phase 4 — 再生コントロール ✅
詳細設計は [design/playback.md](./design/playback.md) / [design/device-use.md](./design/device-use.md)。手順は [manual-tests.md](./manual-tests.md)。
- [x] `play`（無引数=再開 / クエリ=検索して再生）/ `pause` / `next` / `prev` / `toggle` / `vol`
- [x] `device use` で spotifyd へトランスファー（`match_device` の名前照合を単体テスト）
- [x] `fmt`/`clippy -D warnings` 通過、単体テスト 28 件、ECC rust-reviewer 反映（clone 除去・空クエリ/空列ガード・`Config::load` 巻き上げ）
- [x] 実 API の再生テスト実施（`device use`→`play`→`status`/`next`/`prev`/`vol`/`pause` すべて動作確認）
  - **`status` バグ修正**: `/me/player` はトラックに `external_ids` を返さず rspotify の `FullTrack` 解析が
    失敗し `PlayableItem::Unknown` に落ちる。生 JSON から表示値を取り出すフォールバックを追加（`track_from_json`）。
  - **既知の制約**: `toggle` の短時間連続実行は Connect の状態伝播遅延で誤判定し得る（[manual-tests.md](./manual-tests.md) 参照）。

### Phase 5 — プレイリスト & ライブラリ ✅
詳細設計は [design/playlist.md](./design/playlist.md) / [design/lib.md](./design/lib.md) / [design/match-name.md](./design/match-name.md)。
- [x] `playlist ls`：プレイリスト一覧（曲数・URI・先頭50件、超過時は総数注記）
- [x] `playlist play <name>`：名前照合して `start_context_playback` で再生（アクティブデバイス対象）
- [x] `lib`：保存済みトラック・アルバム一覧（各先頭20件、超過時は見出しに内訳）
- [x] リファクタ：名前照合を共通ヘルパ `src/match_name.rs`（`device use` と共用）へ抽出、
      一覧整形を `format::render_entry`（search / playlist / lib で共用）へ集約
- [x] `fmt`/`clippy -D warnings` 通過、単体テスト 33 件、ECC rust-reviewer 反映
      （`pl.id` の不要 clone を借用化・`NEED_DEVICE_HINT` を `commands/mod.rs` に集約・
      `page.items` の move を借用化・該当なし文言を純粋関数 `no_match_message` に抽出しテスト追加）
- [ ] 実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 5 手順）— **ユーザー実機で実施予定**

### Phase 6 — TUI 化
「段階的」方針に沿い、まず Now Playing を出し（6.0）、以降サブフェーズで機能を足していく。
各サブフェーズは「オーバーレイ/ビュー追加 → 既存 API・純粋関数を再利用 → 単体テスト → ECC レビュー → 実機確認」の流れで進める。

#### Phase 6.0 — Now Playing ダッシュボード ✅
- [x] `spoterm tui`：`ratatui` + `crossterm` で Now Playing をライブ表示（`src/tui/`）
  - 曲名/アーティスト/アルバム/進捗ゲージ/デバイス/音量を表示。認証は既存 `auth::authed_client` を再利用。
  - `POLL_INTERVAL=2s` で `current_playback` を再取得、合間は `view::interpolate_progress` でローカル補間。
  - キー操作：`space`=トグル / `n`=次 / `p`=前 / `+`,`-`=音量±5 / `r`=更新 / `q`,`Esc`,`Ctrl-C`=終了。
  - パニックフックで端末復元、API エラーはステータス行に出してループ継続（silent failure 禁止）。
  - `Unknown` トラックのフォールバックは `status::track_from_json` を crate 内共有して再利用（DRY）。
- [x] `fmt`/`clippy -D warnings` 通過、単体テスト 41 件（TUI 純粋関数 5 件追加）
- [x] 実端末での実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 6 手順）— **ユーザー実機で確認済み（2026-07-19）**

#### Phase 6.1 — 検索して再生（Search overlay）✅
- [x] `/` で検索入力モード → クエリ入力 → `search`（Track・上限10）で結果リスト表示（既存 `search` API 再利用）
- [x] `↑`/`↓` で候補選択、`Enter` で `start_uris_playback` 再生、`Esc` で入力へ戻る/オーバーレイを閉じる
  - モードは `Mode::{Normal, Search}`、検索は `Input`/`Results` の 2 フェーズ。キー処理は同期で
    クエリ/選択を更新し `SearchAction` を返す→本体が非同期実行（借用競合を回避）。
  - 再生可能な（URI あり）トラックのみ候補化（`track_to_hit`）。Ctrl-C は全モードで終了。
- [x] 空クエリは無視、0 ヒット・検索失敗はメッセージ表示（silent failure 禁止）
- [x] 行整形/補足文言は純粋関数 `view::search_row` / `view::search_hint` に切り出して単体テスト、`fmt`/`clippy -D warnings` 通過、単体テスト 44 件
- [x] ECC rust-reviewer 反映：再生失敗をオーバーレイ内メッセージで表示（`play_uri`/`play_track` 分離で silent failure 解消）・
      入力へ戻る際に古い結果/選択をクリア・補足文言を純粋関数化
- [ ] 実端末での実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 6.1 手順）— **ユーザー実機で実施予定**

#### Phase 6.2 — ライブラリ / プレイリスト閲覧・再生（Browse view）✅
- [x] Now Playing で `2` → ライブラリ閲覧オーバーレイ（新モジュール `src/tui/browse.rs`）
  - タブ = プレイリスト / 保存トラック / 保存アルバム。`←`/`→` でタブ切替、`↑`/`↓` 選択、`Enter` 再生、`Esc` 戻る。
  - 取得は既存コマンドと同じ API（`current_user_playlists_manual` / `current_user_saved_tracks_manual` /
    `current_user_saved_albums_manual`、先頭ページ 50/20/20）を再利用。
  - 再生：トラックは URI 単体（`start_uris_playback`）、プレイリスト/アルバムはコンテキスト（`start_context_playback`）。
  - キー処理は同期で選択更新→`BrowseAction` を返し本体が非同期実行（検索と同じ借用回避パターン）。
    `browse.rs` は `App` に触れずデータ取得・再生・キー変換のみ担当。
- [x] 空タブ・取得失敗・再生失敗はオーバーレイ内メッセージで表示（silent failure 禁止／`draw_browse` が描画）
- [x] 行整形は検索と共通の純粋関数 `view::search_row` を再利用、選択ハイライト（ratatui `List`/`ListState`）
- [x] `fmt`/`clippy -D warnings` 通過、単体テスト 44 件
- [ ] 実端末での実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 6.2 手順）— **ユーザー実機で実施予定**

#### Phase 6.3 — デバイス選択（Device picker）✅
- [x] `d` でデバイス一覧オーバーレイ（新モジュール `src/tui/devices.rs`・`devices` コマンドと同じ `device()` 再利用）、
      `Enter` で `transfer_playback(id, Some(true))`（spotifyd 等へ）。キー処理は同期で選択更新→`DeviceAction` を返し
      本体が非同期実行（`browse`/`search` と同じ借用回避パターン）。デバイスは出入りするためキャッシュしない。
- [x] アクティブデバイスの明示（`● (active)`／非アクティブ `○`）。行整形は純粋関数 `view::device_row` に切り出して
      単体テスト。転送成功で `last_poll=None` にして即ポーリング → Now Playing のデバイス行へ反映。
- [x] アクティブ無し時の操作（再生/音量）失敗時に「d でデバイスを選択」と案内。操作不可/ID 無しデバイスは
      転送前に弾いてメッセージ表示（silent failure 禁止）。空一覧・取得失敗・転送失敗も補足行に表示。
- [x] `fmt`/`clippy -D warnings`／`cargo test`（`device_row` 4 件追加、48 件）・ECC rust-reviewer 反映（0 CRITICAL/HIGH）
- [x] 実端末での実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 6.3 手順）— **ユーザー実機で確認済み（2026-07-19）**

#### Phase 6.4 — シーク & 現在曲のお気に入り（Seek + save）
- [ ] `←`/`→` で 5〜10 秒シーク（`seek_track`）。進捗ゲージは補間ではなく即時反映
- [ ] `s` で現在曲をライブラリに保存/解除（`save_tracks` / `current_user_saved_tracks_contains`）、状態を表示
- [ ] 連続シークのデバウンス／Connect 状態伝播遅延への配慮

#### Phase 6.5 — UI 仕上げ & 内部改善
- [ ] `?` ヘルプオーバーレイ（キー一覧）、フッターの簡略化
- [ ] ワイド文字（絵文字/全角）の表示幅計算を `unicode-width` 等で厳密化（現状 char 数で近似）
- [ ] ステータス行の一定時間後クリア、色/強調の調整
- [ ] `authed_client` を毎ポーリング再構築している点を見直し（クライアント/トークンを `App` に保持し失効時のみ更新）

> 各サブフェーズは独立した小さめの PR で進める想定。優先度・順序は要望に応じて入れ替え可。

### Phase 7 — テスト & 配布
- [ ] `wiremock` で API モックの単体テスト、CI（GitHub Actions）
- [ ] リリースバイナリ（GitHub Releases）、`cargo install`、将来的に Homebrew tap

## 追加要望・設計メモ（随時追記）

実装中に判明した方針変更や、当初プランへの追加要望をここに集約する。

- **詳細設計書を `docs/design/` に機能単位で作成**（Phase 3 から運用）。実装前に設計 → 実装差分は同文書へ反映。
- **表示整形は純粋関数に分離**：rspotify モデルはテストで組み立てにくいため、整形は
  プリミティブ入出力の純粋関数（`src/format.rs` 他）に切り出して単体テストする。
- **空状態を必ず明示**：再生なし / ヒット 0 / デバイス 0 は黙らずメッセージを出す（silent failure 禁止）。
- **トークンのリフレッシュは自前制御（rspotify の不具合回避）**：rspotify 0.16.1 は毎リクエスト前の
  `auto_reauth` で `refetch_token → write_token_cache` を実行するが、Spotify の PKCE リフレッシュ応答が
  `refresh_token` を省略すると `null` で上書き保存し、以降リフレッシュ不能になる（実機で発生・修正済み）。
  対策として `token_refreshing=false` で自動更新を無効化し、`auth::authed_client` が期限切れ時のみ明示的に
  更新して旧 `refresh_token` を保持（`preserve_refresh_token`）、`0600` で保存する（`restrict_token_perms`）。
- **spotifyd 可視性（Phase 3 devices で検証済み ✅）**：discovery(zeroconf) の spotifyd が Web API の
  devices 一覧に出るか未確定だった件。詳細は [design/devices.md](./design/devices.md)。
  - 検証結果（2026-07-18）: `MacBook-spotifyd` は **一覧に出た**。discovery 方式のままで可視で、
    公式アプリでの事前アクティブ化や OAuth 方式への切替は不要。Phase 4 の `device use` はこの id への
    `transfer_playback` で実装できる見込み。

## 次の一手
Phase 6.0（Now Playing）・6.1（検索）・6.2（ライブラリ閲覧）・6.3（デバイス選択）は実装・自動テスト・
実機確認まで完了。次は Phase 6.4（シーク & 現在曲のお気に入り）に着手する。
