//! 名前照合の共通ヘルパ。`device use <name>`（デバイス名）と `playlist play <name>`
//! （プレイリスト名）で同一の照合ロジックを使うため、純粋関数として切り出す。
//! 詳細設計: docs/design/match-name.md

/// 名前照合の結果（`names` のインデックスで返す）。
#[derive(Debug, PartialEq)]
pub enum NameMatch {
    Found(usize),
    None,
    Ambiguous(Vec<usize>),
}

/// 候補名（呼び出し側と同順・1:1）から query に一致するものを照合する。
/// 大文字小文字を無視し、完全一致を部分一致より優先する。
pub fn match_name(names: &[&str], query: &str) -> NameMatch {
    let q = query.trim().to_lowercase();
    // 空クエリは全件に部分一致してしまうため、明示的に「該当なし」とする。
    if q.is_empty() {
        return NameMatch::None;
    }

    let exact: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase() == q)
        .map(|(i, _)| i)
        .collect();
    match exact.len() {
        1 => return NameMatch::Found(exact[0]),
        n if n > 1 => return NameMatch::Ambiguous(exact),
        _ => {}
    }

    let partial: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect();
    match partial.len() {
        0 => NameMatch::None,
        1 => NameMatch::Found(partial[0]),
        _ => NameMatch::Ambiguous(partial),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_case_insensitive() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_name(&n, "macbook-spotifyd"), NameMatch::Found(0));
    }

    #[test]
    fn partial_match_single() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_name(&n, "spotifyd"), NameMatch::Found(0));
    }

    #[test]
    fn no_match() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_name(&n, "speaker"), NameMatch::None);
    }

    #[test]
    fn partial_match_ambiguous() {
        let n = ["Living Room TV", "Living Room Speaker"];
        assert_eq!(
            match_name(&n, "living room"),
            NameMatch::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn exact_wins_over_partial() {
        // "Living Room" は 0 番と完全一致し、1 番とは部分一致。完全一致を優先する。
        let n = ["Living Room", "Living Room TV"];
        assert_eq!(match_name(&n, "living room"), NameMatch::Found(0));
    }

    #[test]
    fn empty_query_matches_nothing() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_name(&n, "   "), NameMatch::None);
    }
}
