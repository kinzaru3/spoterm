# 詳細設計: `spoterm device use <name>`

## 目的

指定名のデバイス（例 `MacBook-spotifyd`）へ再生をトランスファーする。Phase 3 で spotifyd が
Web API のデバイス一覧に見えることを確認済み。本コマンドで spotifyd をアクティブ化し、
以降の再生コントロール（[playback.md](./playback.md)）の対象にする。

## 呼び出し元 / 依存

- `src/main.rs` の `Command::Device { action: DeviceAction::Use { name } }` → `commands::device::run`。
- `auth::authed_client` を使用。
- `device()` で一覧取得 → 名前照合 → `transfer_playback(&id, Some(true))`。

## 使用 API

- `device()` → `Vec<Device>`
- `transfer_playback(device_id: &str, play: Option<bool>)`：`Some(true)` で転送先の再生を継続/開始。

## 名前照合（純粋関数・テスト対象）

`match_device(names: &[String], query: &str) -> DeviceMatch`

- 大文字小文字を無視。**完全一致を優先**し、無ければ**部分一致**。
- 戻り値:
  - `Found(usize)` … 一意に決定（完全一致1件、または部分一致1件）
  - `None` … 該当なし
  - `Ambiguous(Vec<usize>)` … 複数一致（完全一致複数、または部分一致複数）

```
enum DeviceMatch { Found(usize), None, Ambiguous(Vec<usize>) }
```

## 出力仕様

- `Found`: 対象デバイスの `id`（`Option<String>`）を取り出し `transfer_playback` →
  `▶ '<name>' へ再生を移しました`。`id` が無い（`None`）場合はエラー文言。
- `None`: `'<query>' に一致するデバイスがありません。spoterm devices で一覧を確認してください`
- `Ambiguous`: 候補名を列挙して、より具体的な指定を促す。

## テスト（`match_device`）

- 完全一致（大文字小文字違い含む）→ `Found`
- 部分一致 1 件 → `Found`
- 該当なし → `None`
- 部分一致複数 → `Ambiguous`
- 完全一致が部分一致より優先されること（例 `"Living Room"` と `"Living Room TV"` があり query=`"living room"` は完全一致側を選ぶ）
