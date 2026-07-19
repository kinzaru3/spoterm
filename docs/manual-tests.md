# 手動テスト手順（実 API・実機）

自動テスト（`cargo test`）では純粋関数のみを検証している。再生を伴う挙動は実 API・実機でしか
確認できないため、その手順をここに残す。**実行すると実際に音が鳴る**点に注意。

## 前提

- ホスト（mac）で **spotifyd が起動**していること（`brew services list` で `spotifyd` が started）。
- `spotifyd` の音声出力先は **mac のシステム既定の出力デバイス**（portaudio backend）。
  AirPods 等で聞きたい場合は、再生前に mac の出力を AirPods にしておく
  （spoterm/Web API 側では出力先は制御しない。あくまで再生デバイス=spotifyd を指定するだけ）。
- 一度 `spoterm login` 済みで `token.json` が有効なこと。
- コンテナ内で実行する。ホストからは:
  ```bash
  export PATH="$HOME/.rd/bin:$PATH"
  docker compose exec -T dev bash -lc 'cd /workspace && cargo run -q -- <args>'
  ```

## Phase 3 — 読み取り系（副作用なし・安全）

```bash
cargo run -q -- devices   # spotifyd を含むデバイス一覧が出る
cargo run -q -- status    # 再生状況（未再生なら「再生中の曲はありません」）
cargo run -q -- search daft punk   # Tracks/Albums/Artists が URI 付きで出る
```

期待:
- `devices` に `MacBook-spotifyd` が表示される（discovery 方式でも Web API に見える。実地確認済み）。

## Phase 4 — 再生コントロール（音が鳴る）

推奨手順（音量を控えめにしてから再生）:

```bash
cargo run -q -- device use MacBook-spotifyd   # spotifyd をアクティブ化（転送）
cargo run -q -- vol 25                          # 先に音量を下げる
cargo run -q -- play instant crush daft punk    # 検索して再生
cargo run -q -- status                          # ▶ 再生中 / 曲・アーティスト・進捗・デバイスが出る
cargo run -q -- next                            # 次の曲へ
cargo run -q -- prev                            # 前の曲へ
cargo run -q -- toggle                          # 再生⇔一時停止のトグル
cargo run -q -- pause                           # 停止
cargo run -q -- play                            # （無引数）再開
```

期待:
- `device use` 後、`devices` で `MacBook-spotifyd` が `● … (active)` になる。
- `play <query>` で該当曲が再生され、`status` に `▶ 再生中` と曲情報が出る。
- `vol` の値が `status` / `devices` の `vol` に反映される。

## Phase 5 — プレイリスト & ライブラリ

`lib` は読み取り専用（音は鳴らない）。`playlist play` は再生を開始する（音が鳴る）。

```bash
cargo run -q -- playlist ls                 # プレイリスト一覧（曲数・URI）。50件超なら総数注記
cargo run -q -- lib                         # 保存済みトラック/アルバム一覧（各先頭20件）
cargo run -q -- device use MacBook-spotifyd # 再生の前にデバイスをアクティブ化
cargo run -q -- vol 25                       # 音量を下げてから
cargo run -q -- playlist play <名前の一部>   # 名前照合して再生（部分一致可）
cargo run -q -- status                       # ▶ 再生中 とプレイリストの曲情報が出る
```

期待:
- `playlist ls`：`  1. <名前>  —  <n>曲    spotify:playlist:...` 形式。0 件なら「プレイリストがありません」。
- `playlist play`：
  - 一意に一致 → `▶ 再生: <名前>`。アクティブデバイスが無いと失敗し `device use` を促すヒントが出る。
  - 部分一致が複数 → 候補名を列挙（`Ambiguous`）。
  - 該当なし → 案内文（先頭 50 件しか見ていない場合はその旨も付く）。
- `lib`：`🎵 保存済みトラック` / `💿 保存済みアルバム` の 2 セクション。両方 0 件なら一括メッセージ。
  取得上限（各 20 件）を超える場合は見出しに `（先頭 20 件 / 全 M 件）` が付く。

## Phase 6 — 対話型 TUI（Now Playing ダッシュボード）

TUI は **raw mode / 代替スクリーンを使う対話アプリ**なので、`docker compose exec -T`（TTY 無し）
では起動できない（`enable_raw_mode` が `os error 6` で失敗する）。ホストの実ターミナルで、
TTY を割り当てて（`-T` を付けずに）実行する:

```bash
export PATH="$HOME/.rd/bin:$PATH"
docker compose exec dev bash -lc 'cd /workspace && cargo run -q -- tui'
```

事前に `device use MacBook-spotifyd` などでアクティブデバイスを用意しておくと操作を確認しやすい。

操作:
- `space` 再生/一時停止トグル、`n` 次の曲、`p` 前の曲
- `+` / `-` 音量 ±5
- `r` 手動更新、`q` / `Esc` / `Ctrl-C` 終了

期待:
- 起動直後に Now Playing（曲名・アーティスト・アルバム・進捗ゲージ・デバイス・音量）が出る。
- 進捗ゲージは 2 秒ごとの再取得の合間もローカル補間で滑らかに進む（再生中のみ）。
- 無再生時は「再生中の曲はありません（p で再開 / spoterm play で開始）」を表示。
- キー操作の結果はステータス行に出る（例: `⏭ 次の曲へ`）。操作は即時に再取得され画面へ反映される。
- API エラー（アクティブデバイス無し等）はステータス行に `⚠ …` で出て、TUI は落ちない。
- 終了後、ターミナルは元の画面・カーソルに正しく戻る。

### Phase 6.1 — 検索して再生（Search overlay）

TUI 起動後（前項と同じく TTY 必須）、Now Playing 画面で操作する。

- `/` を押す → 検索入力モード（`検索: ▌`）。
- クエリを入力（例 `daft punk`）して `Enter` → トラック候補が最大 10 件出る。
- `↑`/`↓` で選択、`Enter` で再生（アクティブデバイスが必要）。
- `Esc`（結果画面）→ クエリ修正へ戻る。`Esc`（入力画面）→ Now Playing へ戻る。
- `Ctrl-C` はどの画面でも終了。

期待:
- 再生を選ぶと Now Playing に戻り、選んだ曲が再生される（`▶ 再生を開始しました`）。
- ヒット 0 → 「"…" に一致するトラックはありませんでした」。空クエリで `Enter` は無反応。
- アクティブデバイス未選択で再生すると、ステータス行に `⚠ 操作に失敗: …` が出る（先に `device use` するか Now Playing で用意）。
- 検索中も裏で Now Playing のポーリングは継続（戻ると最新表示）。

### Phase 6.2 — ライブラリ / プレイリスト閲覧（Browse view）

TUI 起動後（TTY 必須）、Now Playing 画面で操作する。

- `2` を押す → ライブラリ閲覧オーバーレイ。上部にタブ `[プレイリスト] 保存トラック 保存アルバム`。
- `←`/`→` でタブ切替（切替のたびに一覧を取得）、`↑`/`↓` で選択、`Enter` で再生。
- `Esc` で Now Playing に戻る。`Ctrl-C` はどの画面でも終了。

期待:
- プレイリスト（先頭50件）・保存トラック/アルバム（各先頭20件）が `title — subtitle` で並ぶ。
- `Enter` 再生：トラックは単体再生、プレイリスト/アルバムはコンテキスト再生 → Now Playing に戻り再生開始。
- 空タブは「… は空です」、取得失敗・再生失敗（アクティブデバイス無し等）は補足行に `…に失敗しました: …` を表示。
- 閲覧中も裏で Now Playing のポーリングは継続（戻ると最新表示）。

## 既知の挙動・注意

- **`status` の曲情報**: Spotify の `/me/player` はトラックに `external_ids` を含めず、rspotify 0.16.1 の
  `FullTrack` 解析が失敗して `PlayableItem::Unknown`（生 JSON）に落ちる。spoterm は生 JSON から
  曲名・アーティスト・アルバム・尺を取り出してフォールバック表示する（`commands/status.rs::track_from_json`）。
- **`toggle` の連続実行**: Spotify Connect の状態伝播には遅延がある。`toggle` を短時間に連続実行すると、
  直後の `current_playback` が古い `is_playing` を返し、意図と逆（例: 2 回連続で「一時停止」）になることがある。
  通常の対話利用（数秒以上あけて実行）では問題ない。
- **アクティブデバイス未選択**: 再生系コマンドは `device_id=None`（アクティブデバイス対象）で送る。
  アクティブデバイスが無いと Web API が 404 を返すため、先に `device use <name>` で選択する。
- **spotifyd の音声出力先は起動時に固定される（重要）**: `spotifyd.conf` は `backend = "portaudio"` で
  `device` 未指定のため、**spotifyd 起動時点の mac 既定出力デバイス**を掴んだまま常駐する。あとから
  AirPods 等に出力先を切り替えても spotifyd は追従しない（Spotify 側では再生中に見えるのに音が
  出ない、という症状になる）。
  → **mac の出力先を切り替えたら spotifyd を再起動する**:
  ```bash
  brew services restart spotifyd   # AirPods を既定にした状態で実行
  ```
  実測でも、AirPods を既定出力にしてから spotifyd を再起動したところ AirPods から再生できた。
