# spoterm 実装プラン

Spotify Web API を使った Spotify CLI アプリ。再生デバイスは mac の公式 Spotify アプリ（Connect）を使う。

## 決定事項（ヒアリング結果）

| 項目 | 決定 |
| --- | --- |
| 言語 | **Rust**（※ローカル未インストール → Phase 0 で導入） |
| 機能スコープ | 再生コントロール / 検索して再生 / Now Playing 表示 / プレイリスト管理 / ライブラリ一覧・再生 |
| UI 形式 | **段階的**（まずワンショットコマンド → 後で対話型 TUI） |
| 配布 | 将来的に公開（OSS 前提の設計にする） |
| アカウント | Spotify **Premium**（再生制御 API・Connect 再生が可能） |
| Dev アプリ | 登録済み（Client ID 取得済み） |

## アーキテクチャ

```
[あなた] --キー操作--> [spoterm CLI/TUI] --Web API--> [Spotify サーバ]
                                                          |
                                                  再生指示(device_id)
                                                          v
                                          [公式 Spotify アプリ = Connect デバイス] --> 🔊
```

- **mac の公式 Spotify アプリ**が「再生デバイス」。起動・ログインしておくと Connect デバイスとして
  見え、spoterm は Web API 経由でそのデバイスへ再生をトランスファーして音を鳴らす。
- 認証は **Authorization Code + PKCE**（Client Secret を端末に置かなくてよい＝公開向き）。
- **spotifyd（librespot ベースの非公式クライアント）はスコープ外**。spoterm 自体は librespot に依存しない。

## 開発環境（Docker）

ホスト(mac)を汚さないため、Rust の開発・ビルドは **Docker コンテナ内**で行う。
公式 Spotify アプリと spoterm は Spotify クラウド経由で連携するため、この分離が可能。

```
[ホスト mac]                          [Docker コンテナ (Linux)]
  公式 Spotify アプリ (音を出す) 🔊      rust toolchain + spoterm 開発
  ブラウザ (OAuth 同意画面)             neovim (ホストの設定を持ち込み)
        ↕ Spotify クラウド ↕                    ↕
        └──────── どちらも api.spotify.com と通信 ────────┘
```

- **公式 Spotify アプリはホスト(mac)で起動**。コンテナ内だと mac のスピーカーに音を出せないため。
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
spoterm devices           # 利用可能デバイス一覧（公式 Spotify アプリを含む）
spoterm device use <name> # 指定デバイス（公式アプリ）へ再生をトランスファー
spoterm playlist ls | play <name>
spoterm lib               # 保存済みトラック/アルバム一覧・再生
```

## フェーズ計画

### Phase 0 — 環境セットアップ
- [ ] `Dockerfile`（Rust ベースイメージ + neovim + ビルド依存）
- [ ] `docker-compose.yml`（上表のマウント・ポート・named volume）
- [ ] コンテナ起動 & `docker compose exec` で shell / nvim が使えることを確認
- [ ] ホスト側で **公式 Spotify アプリ**を起動・Premium ログイン（Connect デバイスとして可視になることを確認）
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

#### Phase 6.4 — シーク & 現在曲のお気に入り（Seek + save）✅
- [x] `←`/`→` で 5 秒シーク（`seek_track`・`chrono::Duration`）。成功時にローカル進捗を即時更新して
      即反映（強制ポーリングせず Connect 遅延の巻き戻り表示を回避）。目標算出は純粋関数 `view::seek_target`。
- [x] `s` で現在曲をライブラリに保存/解除。非 deprecated の `library_add`/`library_remove`/`library_contains`
      （`LibraryId::Track`）を使用。保存状態は state 行に `♥ 保存済み`/`♡ 未保存` を表示（`view::render_lines`）。
      **`user-library-modify` スコープを追加**（要再ログイン）。Unknown 経路の URI は `track_id_from_json` で取得。
- [x] 連続シークは「1 キー=1 API」（音量 +/- と同方針、ローカル進捗から積算）。保存状態は曲ごとに 1 回だけ
      問い合わせ（`saved_checked`）で永続失敗の連打を回避。
- [x] `fmt`/`clippy -D warnings`／`cargo test`（53 件・`seek_target`/`track_id_from_json`/saved 追加）・
      ECC rust-reviewer 反映（CRITICAL: スコープ追加、MEDIUM: 問い合わせ上限・cast 明示）
- [ ] 実端末での実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 6.4 手順・要再ログイン）— **ユーザー実機で実施予定**

#### Phase 6.5 — UI 仕上げ & 内部改善 ✅
- [x] `?` ヘルプオーバーレイ（表示専用 `Mode::Help`・どのキーでも閉じる）でキー一覧を表示、フッターを
      `? ヘルプ   q 終了` に簡略化。キー定義は純粋関数 `view::help_entries()` に一元化（フッター/ヘルプで共有）。
- [x] ワイド文字（絵文字/全角）の表示幅を `unicode-width` で厳密化。`format::display_width` を追加し、
      `truncate` を列幅ベースに書き換え（`…` 1 列ぶんを確保・全角の境界越えは手前で停止）。ratatui が
      既に間接依存する版に統一（依存ツリー増なし）。
- [x] ステータス行を `STATUS_TTL`（4 秒）で自動クリア（変化検知で計時）。種別を純粋関数 `view::status_kind`
      で分類し `⚠`=赤 / 成功=緑 / それ以外=淡色で色分け。自動更新停止の案内は `poll_failures` から
      `draw_now` が常時描画（自動クリアで消えない＝silent failure 回避）。
- [x] `authed_client` の毎ポーリング再構築は **Phase 6.2 で対応済み**（`App` がクライアントを保持し
      `auth::ensure_fresh_token` で失効時のみ更新）。本フェーズでは確認のみ。
- [x] `fmt`/`clippy -D warnings`／`cargo test`（56 件・`display_width`/`help_entries`/`status_kind` 追加）・
      ECC レビュー（rust-reviewer + silent-failure-hunter）反映
- [ ] 実端末での実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 6.5 手順）— **ユーザー実機で実施予定**

#### Phase 6.6 — カバーアート表示（Cover art）
コンプラ調査で挙がった「再生時はカバーアート＋メタデータを表示」（Spotify Developer Policy）の未充足を解消。
- [x] `ratatui-image` で Now Playing 左にカバーアートを表示。端末を問い合わせて最適プロトコル
      （Kitty/iTerm2/Sixel）を自動選択、非対応端末は halfblocks（カラー半ブロック）へフォールバック。
      **ratatui 0.29→0.30 へアップグレード**（ratatui-image 11.0.6 が 0.30 依存・自作コードは無改修で移行）。
- [x] 依存追加: `ratatui-image`（`=11.0.6` pin）/ `image`（jpeg,png）/ `reqwest`（rustls・タイムアウト5s・
      リダイレクト無効）。アート URL 選択は純粋関数 `art::pick_image_url`、取得+デコードは
      `art::fetch_decode`（`spawn_blocking`・デコード寸法上限 4096）。Unknown 経路は `album_images_from_json`。
- [x] 曲変化時のみ取得（`art_url` で 1 回化）。`r` で再取得可（行き止まりにしない）。取得失敗はステータス表示、
      アート無し（エピソード等）は「(アートなし)」プレースホルダで空状態を明示（silent failure 禁止）。
- [x] セキュリティ: `art::is_allowed_art_url` で `https`＋`*.scdn.co` に限定（SSRF 対策・単体テスト）。
- [x] `fmt`/`clippy -D warnings`／`cargo test`（64 件・`pick_image_url`/`album_images_from_json`/`is_allowed_art_url` 追加）・
      ECC レビュー（rust-reviewer + silent-failure-hunter + security-reviewer 並列＋再レビュー）反映
- [ ] 実端末での実 API 動作確認（[manual-tests.md](./manual-tests.md) の Phase 6.6 手順）— **ユーザー実機で実施予定**
- 保留（フォローアップ）: `pick_image_url` の幅0タイブレーク、描画時 `last_encoding_result()` の失敗検出（LOW）。

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
- **再生デバイスは公式 Spotify アプリに確定（spotifyd はスコープ外）**：当初は spotifyd（librespot ベースの
  非公式クライアント）も再生デバイス候補にしていたが、Spotify Developer 規約・API 規約の観点（非公式クライアント
  依存を避ける）と、mac の公式アプリで十分なことから **spotifyd は採用しない**。spoterm 本体は librespot に一切
  依存しない（依存は `rspotify`＝公式 Web API クライアントのみ）。
  - デバイス可視性の検証（2026-07-18・当時は spotifyd で確認）は Connect デバイス一般に当てはまり、公式アプリも
    起動・ログインしておけば `devices` 一覧に現れ、`device use`／TUI の `d` から `transfer_playback` で再生を移せる。
  - 詳細な設計履歴は [design/devices.md](./design/devices.md)（記述は当時の spotifyd 前提を含むが、対象デバイスを
    公式アプリに読み替える）。

## 次の一手
Phase 6.0〜6.4 は実装・自動テスト・実機確認まで完了。Phase 6.5（UI 仕上げ & 内部改善）は実装・自動テスト・
ECC レビュー（rust-reviewer + silent-failure-hunter）まで完了、残りは実機確認（[manual-tests.md](./manual-tests.md)
の Phase 6.5 手順）。これで Phase 6（TUI 化）はほぼ完了。次は Phase 7（テスト & 配布：wiremock・CI・リリース）へ。
