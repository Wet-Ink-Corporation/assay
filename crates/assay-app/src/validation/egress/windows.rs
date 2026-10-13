//! The token's windows and how a text window matches one: exactly, or with wildcards standing
//! for non-ASCII alphanumerics the folds could not read back (a lookalike from any script).

use std::collections::HashSet;

use super::{WILDCARD, WINDOW};

/// How many symbols a window draws from once folded: ASCII digits and lowercase letters.
const SYMBOLS: usize = 36;

/// The token's distinct windows, indexed for exact and wildcard matching.
pub(super) struct TokenWindows {
    /// Every distinct window, in no particular order.
    windows: Vec<[u8; WINDOW]>,
    /// The same windows, for the exact match of a window without wildcards.
    exact: HashSet<[u8; WINDOW]>,
    /// For position `p` and symbol `s`, at `p * SYMBOLS + s`: the indices into `windows` of
    /// those holding `s` at `p`.
    by_symbol: Vec<Vec<usize>>,
}

impl TokenWindows {
    /// The windows of `forms`, each a string of ASCII digits and lowercase letters; any other
    /// byte ends no window but cannot match, as no text symbol equals it.
    pub(super) fn new(forms: &[Vec<u8>]) -> Self {
        let exact: HashSet<[u8; WINDOW]> = forms
            .iter()
            .flat_map(|form| form.array_windows::<WINDOW>().copied())
            .collect();
        let windows: Vec<[u8; WINDOW]> = exact.iter().copied().collect();
        let mut by_symbol = vec![Vec::new(); WINDOW.saturating_mul(SYMBOLS)];
        for (at, window) in windows.iter().enumerate() {
            for (position, &byte) in window.iter().enumerate() {
                if let Some(list) = symbol(byte).and_then(|s| by_symbol.get_mut(slot(position, s)))
                {
                    list.push(at);
                }
            }
        }
        Self {
            windows,
            exact,
            by_symbol,
        }
    }

    /// Whether some window of `view` matches a token window: exactly when it holds no wildcard;
    /// otherwise when it holds at least `min_known` other symbols and each equals the token
    /// window's symbol at its position.
    ///
    /// Cost: per text window, one hash lookup without wildcards; with them, a check of 12
    /// symbols against each token window sharing its first known symbol at that position, at
    /// most every token window (bounded by the token's byte cap), so O(n × windows × 12).
    pub(super) fn matched_in(&self, view: &[u8], min_known: usize) -> bool {
        view.array_windows::<WINDOW>()
            .any(|window| self.matches(window, min_known))
    }

    fn matches(&self, window: &[u8; WINDOW], min_known: usize) -> bool {
        let known = window.iter().filter(|&&byte| byte != WILDCARD).count();
        if known == WINDOW {
            return self.exact.contains(window);
        }
        if known < min_known {
            return false;
        }
        let Some(candidates) = window
            .iter()
            .enumerate()
            .find_map(|(position, &byte)| Some(slot(position, symbol(byte)?)))
            .and_then(|at| self.by_symbol.get(at))
        else {
            return false;
        };
        candidates
            .iter()
            .filter_map(|&at| self.windows.get(at))
            .any(|token| {
                window
                    .iter()
                    .zip(token)
                    .all(|(&text, &expected)| text == WILDCARD || text == expected)
            })
    }
}

/// The index of an ASCII digit or lowercase letter among the [`SYMBOLS`].
fn symbol(byte: u8) -> Option<usize> {
    match byte {
        b'0'..=b'9' => Some(usize::from(byte.wrapping_sub(b'0'))),
        b'a'..=b'z' => Some(usize::from(byte.wrapping_sub(b'a')).saturating_add(10)),
        _ => None,
    }
}

/// Where symbol `s` at `position` is listed in `by_symbol`.
const fn slot(position: usize, s: usize) -> usize {
    position.saturating_mul(SYMBOLS).saturating_add(s)
}

#[cfg(test)]
mod tests {
    use super::TokenWindows;

    fn windows() -> TokenWindows {
        TokenWindows::new(&[b"skantoat01zq3lw9".to_vec()])
    }

    #[test]
    fn matches_a_window_exactly_or_with_enough_known_symbols() {
        let (exact, six_known, five_known) = (
            b"skantoat01zq".as_slice(),
            b"s*a*t*a*0*z*".as_slice(),
            b"s*a*t*a*0***".as_slice(),
        );

        assert_eq!(
            [exact, six_known, five_known].map(|view| windows().matched_in(view, 6)),
            [true, true, false]
        );
    }

    #[test]
    fn refuses_a_wildcard_window_whose_known_symbols_disagree() {
        assert!(
            !windows().matched_in(b"s*a*t*a*0*y*", 6),
            "y is not the token's z"
        );
    }
}
