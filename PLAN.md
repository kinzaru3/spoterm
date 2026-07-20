# PLAN — issue #26 デザイン変更（TUI 全面刷新）

> このファイルはリリース時に削除する一時的な開発計画（git 管理可・release/v0.4.0 の作業用）。
> 確定した設計は `docs/design/` に反映し、本ファイルは役目を終えたら削除する。
> 対象 issue: https://github.com/kinzaru3/spotterm/issues/26 「デザイン変更」

## 0. issue 要件

- 添付 HTML モック 2 枚（通常モード / 検索モード, いずれも「下段 2 カラム版」）の UI にする。
- 「不足機能を実装」。

モックの構造（両モード共通）:

```
┌─ spotterm — tui ────────────────────────────────────────┐
│ ● ● ●  spotterm — tui                                    │  ← ウィンドウ風タイトルバー
├─────────────────────────────┬───────────────────────────┤
│ Now Playing                 │ Visualizer                │  ← 上段 2 カラム（左右 1:1）
│  [cover] Title / Artist     │  ▁▃▅▇▆▄ … （縦バー）      │
│         Album · Year        │                           │
│         ♥ Saved · ◱ Device  │                           │
├─────────────────────────────┴───────────────────────────┤
│ (検索モードのみ) Search:  ⌕ weird fishes▏               │  ← 検索バー（検索モード時だけ）
├──────────────────┬──────────────────────────────────────┤
│ Library ▸ focus  │ 選択項目の詳細                        │  ← 下段 2 カラム（左 1 : 右 1.45）
│  [All][Artists]  │  In Rainbows · Radiohead · 2007       │
│  [Albums][Play.] │  1  15 Step            3:57           │
│  [Tracks]        │  …                                    │
│  ARTISTS         │  ▶4 Weird Fishes/Arp.  5:18           │
│   ♪ Radiohead    │  …                                    │
│  ALBUMS ◈ …      │                                       │
├──────────────────┴──────────────────────────────────────┤
│ ▶ 1:23 ▬▬▬▬●───────── 5:18   vol ▮▮▮▯▯ 40%              │  ← 再生バー（progress + volume）
├──────────────────────────────────────────────────────────┤
│ space play/pause  n/p …  / search  [ ] tab  tab focus  q │  ← フッター（キーヒント）
└──────────────────────────────────────────────────────────┘
```

最大の変化: **現状の「Now Playing 単一ペイン ＋ オーバーレイ切替（検索/ライブラリ/デバイス/ヘルプ）」から、
常時 4 領域を同時表示する「ダッシュボード型」へ全面刷新**する。検索は下段全体をオーバーレイするのではなく、
検索バー ＋ 下段 2 カラム（結果一覧 ＋ ハイライト詳細）としてダッシュボード内に統合する。

---

## 1. 現状（起点コード）の把握

| ファイル | 役割 | 行数 |
| --- | --- | --- |
| `src/tui/mod.rs` | TUI 本体・メインループ・`draw`/`draw_now`/`draw_search`/`draw_browse`/`draw_devices`/`draw_help`・再生制御 | 1484 |
| `src/tui/view.rs` | 純粋整形関数（`render_lines`/`search_row`/`device_row`/`help_entries`/`status_kind` 等） | 490 |
| `src/tui/browse.rs` | ライブラリ閲覧（現状 **Playlists タブのみ**・`BrowseCache`） | 240 |
| `src/tui/devices.rs` | デバイス選択 | 95 |
| `src/tui/art.rs` | カバーアート取得（SSRF 対策・`*.scdn.co`/https のみ） | 129 |
| `src/np_json.rs` | `/me/player` の Unknown JSON フォールバック抽出 | — |
| `src/theme.rs` | 色・Nerd Font アイコン定数・`OK_PREFIXES` | — |
| `src/format.rs` | `format_ms`/`join_artists`/`truncate`/`display_width` | — |

現状のモード遷移: `Mode = Normal | Search | Browse | Devices | Help`。`draw` が `ModeKind` で網羅 match して
モードごとに全画面を切り替える。描画は「API 取得 → プリミティブ写像 → 純粋整形関数 → widget」の分離を徹底。

**OAuth スコープ（`src/auth.rs`）**: `user-read-playback-state` / `user-modify-playback-state` /
`playlist-read-private` / `playlist-read-collaborative` / `user-library-read` / `user-library-modify`。

---

## 2. デザイン実装可否（結論）

| モック要素 | 実装可否 | 補足 |
| --- | --- | --- |
| ウィンドウ風タイトルバー（● ● ●） | ✅ 可 | 装飾。ratatui で枠＋色付き記号。端末では簡略化余地あり |
| 上段 2 カラム（Now Playing ＋ Visualizer） | ✅ 可（Visualizer 除く） | `Layout` 水平分割。カバーアートは既存 `art.rs` を流用 |
| **Visualizer（音声スペクトラム）** | ⚠️ **実データは不可** | 後述 §3。公式 Web API のみ・音声ストリーム非保持のためリアルタイム波形は取得不能 |
| 検索バー（検索モード時） | ✅ 可 | 既存検索入力を流用。ダッシュボード内に配置 |
| 下段左: Library タブ（All/Artists/Albums/Playlists/Tracks） | ⚠️ 一部要スコープ | Playlists/Albums/Tracks は既存スコープで可。**Artists（フォロー中）は `user-follow-read` 追加＋再ログインが必要** |
| 下段左: 検索結果のカテゴリ分類（TOP/SONGS/ARTISTS/ALBUMS） | ✅ 可 | search API を `Track,Artist,Album`（＋Playlist）で取得しカテゴリ表示。現状は Track のみ |
| 下段右: 選択項目の詳細ペイン（アルバムのトラックリスト等） | ✅ 可 | album tracks / playlist items / artist top-tracks 等の取得 API を追加 |
| 再生バー（progress ●スライダ＋vol ▮▮▮▯▯） | ✅ 可 | 既存 `progress_ratio`/`format_ms` を横バー整形に拡張。vol も同様 |
| フッター（キーヒント） | ✅ 可 | 既存 `help_entries()` を単一定義元として流用（表記ずれ防止の不変条件を維持） |

**総括**: Visualizer を除き、モックのレイアウト・機能はすべて公式 Web API と ratatui で実装可能。
ただしオーバーレイ主体 → ダッシュボード主体への**アーキテクチャ変更を伴う大規模改修**になる。

---

## 3. 要方針決定（PM → ユーザー確認事項）

### (A) Visualizer をどう扱うか　★最重要

spotterm は**公式 Web API のみ**を使い、音声そのものを再生・保持しない（librespot/spotifyd 非依存）。
したがって**リアルタイムの音声スペクトラムは原理的に取得できない**。選択肢:

1. **疑似アニメーション（装飾）** … 再生中フラグ・進捗・乱数でバーを揺らす“見た目だけ”のビジュアライザー。
   実データではない旨は割り切る。実装容易・追加スコープ不要。（**推奨**）
2. **Visualizer 枠を別情報に置換** … 例: キュー/次の曲、アルバムアート大サイズ 等（歌詞 API は非対応）。
3. **Audio Analysis で擬似ビート同期** … Spotify の Audio Analysis / Audio Features は 2024-11 に新規アプリで
   deprecated。将来 403 リスクが高く非推奨。

> **決定**: **(1) 疑似アニメーション（装飾）を採用**。実データではない旨をコード comment / README に明記する。

### (B) Artists タブ（フォロー中アーティスト）

モック下段左に「Artists」タブがある。フォロー中アーティスト取得（`current_user_followed_artists`）には
`user-follow-read` スコープが必要で、現状スコープに含まれない＝**追加＋再ログインが必要**。選択肢:

1. `user-follow-read` を追加し「フォロー中アーティスト」を表示（要再ログイン告知）。（**推奨**）
2. スコープ追加を避け、「Artists」= ライブラリ（保存曲/アルバム）に登場するアーティストから導出。
3. Artists タブを当面省略（All/Albums/Playlists/Tracks のみ）。

> **決定**: **(1) `user-follow-read` を追加**しフォロー中アーティストを表示する。
> スコープ追加により**次回起動時に再ログイン（`spotterm login`）が必要**になる旨をユーザーへ告知する
> （README / 起動時ステータスで案内）。

### (C) 検索モードとデバイス選択の扱い

- 検索はダッシュボード内統合（検索バー＋下段 2 カラム）にする。既存 `Mode::Search` は残しつつ描画を刷新。
- **デバイス選択（`d`）はモックに枠が無い**。オーバーレイ（モーダル）として残す方針を提案（Now Playing の
  Device 表示からの導線）。ヘルプ（`?`）も同様にモーダル維持。

> **決定**: **デバイス選択・ヘルプはモーダル（オーバーレイ）維持**。ダッシュボード本体は Normal（通常）と
> Search（検索）の 2 状態のみを常時表示とし、`d`/`?` は従来どおりモーダルで開く。

---

## 4. 実装計画（フェーズ分解）

> 各フェーズは feature ブランチ上で「調査 → 設計(docs) → 実装(TDD) → レビュー(rust-reviewer+silent-failure-hunter) →
> 3 点ゲート(fmt/clippy/test)」を回す。CRITICAL/HIGH ゼロを確認してから次へ。

### Phase 1: レイアウト骨格（ダッシュボード化）
- `draw` を刷新し、常時表示の領域構成（タイトルバー / 上段 2 カラム / (検索バー) / 下段 2 カラム / 再生バー / フッター）を
  `Layout` で構築する純粋な「領域分割関数」を切り出す（幅・高さからの分割は純粋関数化してテスト）。
- 既存 `draw_now` の Now Playing 描画を上段左パネルへ、再生バー・フッターを下段へ再配置。
- 狭い端末幅・高さでの degrade（カラム落とし／最小行数）を定義しテスト。
- Visualizer 枠はプレースホルダ（§3(A) 決定まで）。

### Phase 2: フォーカス管理とパネル間移動
- `tab` でパネル間フォーカス移動、`[` `]` でライブラリ/結果タブ切替。
- `Focus` enum（Library / Detail / …）を導入し網羅 match。選択ハイライトは既存の GREEN 選択スタイル流用。

### Phase 3: ライブラリのタブ拡張（browse.rs）
- `BrowseTab` に `All / Artists / Albums / Playlists / Tracks` を追加、`BrowseCache` をタブ別に拡張。
- ローダー追加: 保存アルバム（`current_user_saved_albums`）/ 保存トラック（`current_user_saved_tracks`）/
  （§3(B) 次第で）フォロー中アーティスト。All はカテゴリ見出し付き結合表示。
- silent failure 不変条件維持（0 件・取得失敗は必ずメッセージ）。

### Phase 4: 詳細ペイン（右カラム）
- 選択項目に応じた詳細取得: アルバム → トラックリスト（`album_tracks`）/ プレイリスト → items（`playlist_items`）/
  アーティスト → トップトラック（`artist_top_tracks`）/ トラック → 所属アルバム文脈。
- 現在再生中トラックのハイライト（`▶`）を詳細内で表現。
- 純粋整形関数（`view::detail_rows` 等）に切り出してテスト。

### Phase 5: 検索の 2 カラム化＋カテゴリ分類
- 検索を `SearchType::Track,Artist,Album`（＋Playlist）に拡張。結果を TOP/SONGS/ARTISTS/ALBUMS へ分類。
- 左＝結果一覧（タブ付き）、右＝ハイライト詳細（選択結果の文脈）。
- **不変条件維持**: 「検索して再生はヒット全件をキュー化」（`queue_from_uris` / `Offset::Uri`）。
  カテゴリ分類後も再生対象のキュー化を崩さない。

### Phase 6: 再生バー／音量バーのグラフィカル化
- `progress_ratio` から `▬▬●───` 型スライダを組む純粋整形関数を追加（幅依存・テスト）。
- 音量 `▮▮▮▯▯ 40%` の整形関数を追加。状態行アイコンとステータス分類の同期（`theme` 定数）維持。

### Phase 7: Visualizer（§3(A) 決定に従う）
- 疑似アニメーション採用時: `TICK`（200ms）で更新するバー高計算を純粋関数化（進捗/乱数シード → 高さ配列）しテスト。
  再生停止時は静止。実データでない旨をコード comment/README に明記。

### Phase 8: docs 反映・仕上げ
- `docs/design/tui.md` / `overlays.md`（→ 役割変化に応じ再編）/ 新規パネル設計を最新コードに厳密一致で更新。
- `docs/manual-tests.md` に実機確認手順（各タブ・詳細・検索 2 カラム・再生バー・Visualizer）を追加。
- キー表（`help_entries`）とフッター/ヘルプの単一定義元不変条件を維持。
- `PLAN.md` は削除（確定設計は `docs/design/` へ）。

---

## 5. 変更ファイル見込み

- `src/tui/mod.rs`（大）: `draw` 全面刷新、`Mode`/`Focus` 再設計、詳細取得・キー処理追加。
- `src/tui/view.rs`（大）: 領域分割・詳細行・再生バー・音量バー・（Visualizer）整形の純粋関数を追加。
- `src/tui/browse.rs`（中）: タブ拡張・ローダー追加。
- 新規 `src/tui/detail.rs`（中）想定: 詳細取得・整形の分離。
- `src/tui/search.rs` 切り出し（任意）: 検索の状態・分類ロジックを mod.rs から分離（mod.rs 肥大対策）。
- `src/auth.rs`（小・§3(B) 次第）: `user-follow-read` 追加。
- `src/theme.rs`（小）: 追加アイコン（タブ記号 ♪◈≡♫ 等）を定数化（Nerd Font 前提）。

## 6. 守るべき不変条件（リグレッション防止・CLAUDE.md より）
- silent failure 禁止 / トークンリフレッシュ自前制御（`auto_reauth` 無効）維持。
- ratatui 系 pin（`ratatui-image = "=11.0.6"`）を緩めない。
- 検索再生はヒット全件キュー化 / カバーアート SSRF 対策（https ＋ `*.scdn.co`）維持。
- 状態行アイコンとステータス分類の同期（`theme` 定数 / `OK_PREFIXES`）維持。
- 公式 Web API のみ / SDK 非同梱。整形は純粋関数に分離してテスト。

## 7. リスク
- **端末サイズ**: 常時 4 領域＋2 カラムは狭端末で破綻しやすい。degrade 設計とテストを Phase 1 で先に固める。
- **API 呼び出し増**: タブ/詳細で取得が増える。`BrowseCache` 同様にキャッシュし、poll 間隔・レート配慮。
- **mod.rs 肥大**: 既に 1484 行。ファイル分割（detail.rs/search.rs）で 800 行以内を志向。
- **スコープ追加**: 再ログイン必須。ユーザー告知（§3(B)）。
