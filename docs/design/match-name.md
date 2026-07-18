# 詳細設計: 名前照合ヘルパ `match_name`（共通）

## 目的

「ユーザーが打った名前 → 候補一覧の中の 1 件」を選ぶ照合ロジックを共通化する。
`device use <name>`（デバイス名）と `playlist play <name>`（プレイリスト名）で同一の
振る舞いが必要なため、純粋関数として `src/match_name.rs` に切り出して両者から使う。

## シグネチャ

```rust
pub enum NameMatch { Found(usize), None, Ambiguous(Vec<usize>) }

/// 候補名（呼び出し側と同順・1:1）から query に一致するものを選ぶ。
pub fn match_name(names: &[&str], query: &str) -> NameMatch
```

- 戻り値の `usize` は `names` のインデックス。呼び出し側は元コレクションにそのまま使える。

## 振る舞い

- 大文字小文字を無視。前後空白は trim。
- **空クエリ（trim 後が空）は `None`**（全件部分一致の暴発を防ぐ）。
- **完全一致を優先**。完全一致が 1 件→`Found`、複数→`Ambiguous`。
- 完全一致 0 件なら**部分一致**（`contains`）。0→`None` / 1→`Found` / 複数→`Ambiguous`。

## 由来

Phase 4 の `commands/device.rs::match_device` を一般化したもの。抽出に伴い、
`device use` は本ヘルパを呼ぶだけの薄い層になる（ロジック重複を排除）。

## テスト（`match_name`）

- 完全一致（大文字小文字違い含む）→ `Found`
- 部分一致 1 件 → `Found`
- 該当なし → `None`
- 部分一致複数 → `Ambiguous`
- 完全一致が部分一致より優先されること
- 空クエリ → `None`
