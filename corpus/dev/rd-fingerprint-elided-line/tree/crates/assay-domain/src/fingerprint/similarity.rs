//! Evidence similarity for the fuzzy dismissal match (03 §3).

/// The similarity a fuzzy match needs, `NUMERATOR / DENOMINATOR` = 0.8 (03 §3).
const NUMERATOR: usize = 4;
/// See [`NUMERATOR`].
const DENOMINATOR: usize = 5;

/// Whether two normalised lines are at least 0.8 similar, similarity being
/// `1 - edit_distance / longer_length` over characters.
///
/// Compared in integers, `5 * distance <= longer_length`, so the threshold is exact. Takes
/// time proportional to the product of the lengths; evidence is capped at 400 characters by the
/// schema before it gets here.
pub(super) fn similar(left: &str, right: &str) -> bool {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let longer = left.len().max(right.len());
    // 1 - d / n >= 4 / 5  <=>  5 * d <= (5 - 4) * n.
    edit_distance(&left, &right).saturating_mul(DENOMINATOR)
        <= longer.saturating_mul(DENOMINATOR - NUMERATOR)
}

/// The Levenshtein distance between `left` and `right`: the fewest single-character
/// insertions, deletions and substitutions that turn one into the other.
fn edit_distance(left: &[char], right: &[char]) -> usize {
    // `row[j]` is the distance between the left prefix processed so far and `right[..j]`.
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, &l) in left.iter().enumerate() {
        let Some((first, rest)) = row.split_first_mut() else {
            break;
        };
        // The cell diagonally up-left, and the one to the left, of the cell being computed.
        let mut diagonal = *first;
        *first = i.saturating_add(1);
        let mut previous = *first;
        for (cell, &r) in rest.iter_mut().zip(right) {
            let above = *cell;
            let substitution = diagonal.saturating_add(usize::from(l != r));
            let insertion_or_deletion = above.min(previous).saturating_add(1);
            *cell = substitution.min(insertion_or_deletion);
            diagonal = above;
            previous = *cell;
        }
    }
    row.last().copied().unwrap_or(left.len())
}

#[cfg(test)]
mod tests {
    use super::similar;

    #[test]
    fn accepts_exactly_four_fifths() {
        assert!(similar("abcdefghij", "abcdefghXY"));
    }

    #[test]
    fn rejects_below_four_fifths() {
        assert!(!similar("abcdefghij", "abcdefgXYZ"));
    }

    #[test]
    fn rejects_just_below_four_fifths() {
        let line = "a".repeat(100);
        let at = format!("{}{}", "a".repeat(80), "b".repeat(20));
        let below = format!("{}{}", "a".repeat(79), "b".repeat(21));

        assert_eq!((similar(&line, &at), similar(&line, &below)), (true, false));
    }

    #[test]
    fn measures_against_the_longer_line() {
        assert_eq!(
            (similar("abcd", "abcde"), similar("abc", "abcde")),
            (true, false)
        );
    }

    #[test]
    fn counts_insertions_deletions_and_substitutions_as_one_edit_each() {
        // kitten -> sitting is the textbook distance 3 over 7 characters: 4/7 < 4/5.
        assert_eq!(
            (similar("kitten", "sitting"), similar("kitten", "kittens")),
            (false, true)
        );
    }

    #[test]
    fn treats_two_empty_lines_as_identical() {
        assert!(similar("", ""));
    }
}
