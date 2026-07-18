# 詳細設計: 再生コントロール（`play`/`pause`/`next`/`prev`/`toggle`/`vol`）

## 目的

再生状態を操作するコマンド群（Phase 4）。Spotify Connect のアクティブデバイスに対して指示を送る。
Premium 必須。副作用ありのため、成功時は何を行ったかを明示する。

## 呼び出し元 / 依存

- `src/main.rs` の各 `Command`（`Play`/`Pause`/`Next`/`Prev`/`Toggle`/`Vol`）→ `commands::playback::*`。
- `auth::authed_client` を使用。
- `play <query>` は `search`（`SearchType::Track`, limit=1）→ `start_uris_playback`。

## デバイス指定の方針（KISS）

全 API は `device_id: Option<&str>` を取り、`None` は**アクティブデバイス**を対象にする。
本フェーズは `None` を渡してアクティブデバイスを操作する。spotifyd を鳴らす場合は先に
`spoterm device use MacBook-spotifyd`（[device-use.md](./device-use.md)）でアクティブ化する運用。
アクティブデバイスが無いと Web API は 404 を返すため、その場合は
「アクティブなデバイスがありません。`spoterm device use <name>` で選択してください」を促す。

## 使用 API と挙動

| コマンド | API | 挙動 / 出力 |
| --- | --- | --- |
| `pause` | `pause_playback(None)` | `⏸ 一時停止しました` |
| `next` | `next_track(None)` | `⏭ 次の曲へ` |
| `prev` | `previous_track(None)` | `⏮ 前の曲へ` |
| `vol <0-100>` | `volume(level, None)` | `🔊 音量を <level>% にしました`（clap で 0-100 検証済み） |
| `toggle` | `current_playback` → 分岐 | 再生中なら `pause_playback`、停止中なら `resume_playback`。デバイス無しは上記の促し |
| `play`（無引数） | `resume_playback(None, None)` | `▶ 再生を再開しました` |
| `play <query>` | `search`(track,1) → `start_uris_playback([id], None, None, None)` | `▶ 再生: <track> — <artists>`。0 件は明示メッセージ |

## エラーハンドリング

- API 失敗は `.context()` を付けて伝播。アクティブデバイス必須のコマンドは
  「デバイス未選択かも」というヒントを文言に含める。
- `play <query>` でトラック 0 件、`toggle` でセッション無しは、エラーではなくメッセージ表示で正常終了。

## 純粋関数（テスト対象）

このフェーズはほぼ薄い API 呼び出しで、純粋ロジックは少ない。テスト可能な単位:
- `render_volume(level: u8) -> String` 等の短い整形は必要に応じて切り出す。
- 主要な検証対象はデバイス名照合（[device-use.md](./device-use.md) の `match_device`）に集約する。
- 実挙動は手動の実 API 検証と Phase 7 の `wiremock` に委ねる。
