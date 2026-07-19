//! Shared name-matching helper. `device use <name>` (device name) and `playlist play <name>`
//! (playlist name) use the same matching logic, so it lives here as a pure function.

/// Result of a name match (returned as an index into `names`).
#[derive(Debug, PartialEq)]
pub enum NameMatch {
    Found(usize),
    None,
    Ambiguous(Vec<usize>),
}

/// Match `query` against the candidate `names` (same order / 1:1 with the caller's list).
/// Case-insensitive; an exact match takes precedence over a partial match.
pub fn match_name(names: &[&str], query: &str) -> NameMatch {
    let q = query.trim().to_lowercase();
    // An empty query would partially match everything, so treat it explicitly as "no match".
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
        let n = ["MacBook Pro", "iPhone"];
        assert_eq!(match_name(&n, "macbook pro"), NameMatch::Found(0));
    }

    #[test]
    fn partial_match_single() {
        let n = ["MacBook Pro", "iPhone"];
        assert_eq!(match_name(&n, "pro"), NameMatch::Found(0));
    }

    #[test]
    fn no_match() {
        let n = ["MacBook Pro", "iPhone"];
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
        // "Living Room" matches index 0 exactly and index 1 partially. Exact wins.
        let n = ["Living Room", "Living Room TV"];
        assert_eq!(match_name(&n, "living room"), NameMatch::Found(0));
    }

    #[test]
    fn empty_query_matches_nothing() {
        let n = ["MacBook Pro", "iPhone"];
        assert_eq!(match_name(&n, "   "), NameMatch::None);
    }
}
