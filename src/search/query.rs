//! Query-side identifier folding.
//!
//! wdpkr indexes LLM-written natural-language summaries, but code searches are
//! often typed as identifiers — `getUserAccount`, `MAX_RETRY_COUNT`,
//! `parse_json_body`. An embedder tokenizes those into subwords that line up
//! poorly with the prose they are being compared against, so the query and the
//! index are effectively written in two different registers.
//!
//! [`split_identifiers`] rewrites the query into the index's register by
//! splitting identifier-shaped words on case and separator boundaries. Like case
//! folding, it exists for *comparison*: context-free, locale-independent, and
//! ASCII-first. It does not stem, expand abbreviations, or consult a dictionary.
//!
//! **This is an experiment and is off by default.** One-sided normalization can
//! hurt as easily as help, so it ships behind `--fold-identifiers` pending eval
//! numbers (wdpkr-hax). No config key yet — the flag is enough to A/B it, and
//! permanent config surface should wait until the numbers justify shipping.

/// Split identifier-shaped words in `query` into space-separated lowercase words.
///
/// Boundaries: `_`/`-`/`.`/`/`/`:` separators, lower→upper transitions
/// (`getUser` → `get user`), and acronym→word transitions
/// (`HTTPSConnection` → `https connection`). Digits attach to the word they
/// follow (`utf8Decode` → `utf8 decode`).
///
/// Words with no internal boundary are lowercased and passed through, so an
/// already-natural query is only case-folded. Non-ASCII characters are left
/// alone rather than guessed at.
pub fn split_identifiers(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + query.len() / 4);
    for word in query.split_whitespace() {
        for piece in split_word(word) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&piece);
        }
    }
    out
}

/// Split one whitespace-delimited word into its constituent lowercase pieces.
fn split_word(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut pieces = Vec::new();
    let mut current = String::new();

    for i in 0..chars.len() {
        let ch = chars[i];

        // Separators end the current piece and are dropped. Punctuation that is
        // not an identifier separator (quotes, `?`) is kept as-is so ordinary
        // prose survives untouched.
        if matches!(ch, '_' | '-' | '.' | '/' | ':') {
            push_piece(&mut pieces, &mut current);
            continue;
        }

        if i > 0 && is_boundary(&chars, i) {
            push_piece(&mut pieces, &mut current);
        }
        current.push(ch.to_ascii_lowercase());
    }
    push_piece(&mut pieces, &mut current);
    pieces
}

/// Is there a word boundary immediately before `chars[i]`?
fn is_boundary(chars: &[char], i: usize) -> bool {
    let prev = chars[i - 1];
    let ch = chars[i];

    if !ch.is_ascii_uppercase() {
        return false;
    }
    // getUser → get | User. Anything that is not itself an ASCII capital counts
    // as the end of a word, so non-ASCII letters (`caféLoader`) split correctly
    // instead of silently swallowing the boundary.
    if !prev.is_ascii_uppercase() {
        return true;
    }
    // HTTPSConnection → HTTPS | Connection: the last capital of a run belongs to
    // the word that follows it, not the acronym before it.
    prev.is_ascii_uppercase()
        && chars
            .get(i + 1)
            .is_some_and(|next| next.is_ascii_lowercase())
}

fn push_piece(pieces: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        pieces.push(std::mem::take(current));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_camel_case() {
        assert_eq!(split_identifiers("getUserAccount"), "get user account");
    }

    #[test]
    fn splits_pascal_case() {
        assert_eq!(split_identifiers("SearchRun"), "search run");
    }

    #[test]
    fn splits_snake_case() {
        assert_eq!(split_identifiers("parse_json_body"), "parse json body");
    }

    #[test]
    fn splits_screaming_snake_case() {
        assert_eq!(split_identifiers("MAX_RETRY_COUNT"), "max retry count");
    }

    #[test]
    fn splits_kebab_case() {
        assert_eq!(split_identifiers("rate-limit-policy"), "rate limit policy");
    }

    /// The trailing capital of an acronym run starts the next word.
    #[test]
    fn splits_acronym_boundary() {
        assert_eq!(split_identifiers("HTTPSConnection"), "https connection");
        assert_eq!(split_identifiers("parseJSONBody"), "parse json body");
    }

    #[test]
    fn digits_attach_to_preceding_word() {
        assert_eq!(split_identifiers("utf8Decode"), "utf8 decode");
        assert_eq!(split_identifiers("sha256Sum"), "sha256 sum");
    }

    #[test]
    fn splits_paths_and_qualified_names() {
        assert_eq!(
            split_identifiers("src/finance/commission.rs"),
            "src finance commission rs"
        );
        assert_eq!(
            split_identifiers("store::VectorStore"),
            "store vector store"
        );
    }

    /// A plain-prose query must come back as itself, only case-folded — the
    /// rewrite must never mangle ordinary natural language.
    #[test]
    fn natural_language_only_case_folds() {
        assert_eq!(
            split_identifiers("how is rate limiting implemented"),
            "how is rate limiting implemented"
        );
        assert_eq!(
            split_identifiers("Where does commission logic live?"),
            "where does commission logic live?"
        );
    }

    #[test]
    fn non_ascii_passes_through() {
        assert_eq!(split_identifiers("caféLoader"), "café loader");
    }

    #[test]
    fn empty_and_whitespace_are_safe() {
        assert_eq!(split_identifiers(""), "");
        assert_eq!(split_identifiers("   \n\t "), "");
    }

    #[test]
    fn collapses_repeated_separators() {
        assert_eq!(split_identifiers("foo__bar"), "foo bar");
        assert_eq!(split_identifiers("a  b"), "a b");
    }
}
