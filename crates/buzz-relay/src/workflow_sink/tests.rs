use super::*;

fn m(name: &str, pubkey: &str) -> (String, String) {
    (name.to_string(), pubkey.to_string())
}

// A 64-char hex pubkey built from a single repeated nibble, for readable tests.
fn pk(nibble: char) -> String {
    std::iter::repeat_n(nibble, 64).collect()
}

#[test]
fn resolves_exact_member_name() {
    let members = vec![m("Robby", &pk('a'))];
    assert_eq!(
        resolve_mention_pubkeys("heads up @Robby — please take a look", &members),
        vec![pk('a')]
    );
}

#[test]
fn matches_case_insensitively() {
    let members = vec![m("Robby", &pk('a'))];
    assert_eq!(
        resolve_mention_pubkeys("ping @robby", &members),
        vec![pk('a')]
    );
}

#[test]
fn ignores_non_member_and_bare_at() {
    let members = vec![m("Robby", &pk('a'))];
    assert!(resolve_mention_pubkeys("hey @Stranger and @", &members).is_empty());
}

#[test]
fn greedy_longest_binds_full_name_not_prefix() {
    // Both "Will" and "Will Pfleger" are members. `@Will Pfleger` must bind
    // Pfleger's key only; a bare `@Will` binds Will.
    let members = vec![m("Will", &pk('1')), m("Will Pfleger", &pk('2'))];
    assert_eq!(
        resolve_mention_pubkeys("cc @Will Pfleger on this", &members),
        vec![pk('2')]
    );
    assert_eq!(
        resolve_mention_pubkeys("cc @Will on this", &members),
        vec![pk('1')]
    );
}

#[test]
fn at_mid_token_does_not_match() {
    // `@` must sit at a left boundary (start / whitespace / `(`). An email-ish
    // or mid-token `@` (`alice@Robby`) must not wake Robby.
    let members = vec![m("Robby", &pk('a'))];
    assert!(resolve_mention_pubkeys("alice@Robby", &members).is_empty());
}

#[test]
fn prefix_member_does_not_match_inside_longer_word() {
    // "Sam" is a member; `@Sami` (no "Sami" member) must not wake Sam.
    let members = vec![m("Sam", &pk('3'))];
    assert!(resolve_mention_pubkeys("hi @Sami", &members).is_empty());
}

#[test]
fn name_with_spaces_and_punctuation() {
    let members = vec![m("Lep (Subagent)", &pk('4'))];
    assert_eq!(
        resolve_mention_pubkeys("@Lep (Subagent) take it", &members),
        vec![pk('4')]
    );
}

#[test]
fn em_dash_terminates_name() {
    // Generated prose often writes `@Name—text` with no space.
    let members = vec![m("Robby", &pk('a'))];
    assert_eq!(
        resolve_mention_pubkeys("@Robby—please look", &members),
        vec![pk('a')]
    );
}

#[test]
fn non_ascii_member_name() {
    let members = vec![m("Zoë", &pk('5'))];
    assert_eq!(
        resolve_mention_pubkeys("welcome @Zoë!", &members),
        vec![pk('5')]
    );
}

#[test]
fn lowercase_expansion_does_not_shift_later_mentions() {
    // Regression (Wren's redteam counterexample): `İ` (U+0130) lowercases to
    // TWO code points (`i` + U+0307). A design that pre-lowercases the whole
    // text and indexes it in parallel with the original chars desyncs after
    // the expansion, dropping every later valid mention. `@İ @Robby` must
    // resolve BOTH members, in order.
    let members = vec![m("İ", &pk('c')), m("Robby", &pk('a'))];
    assert_eq!(
        resolve_mention_pubkeys("@İ @Robby", &members),
        vec![pk('c'), pk('a')]
    );
}

#[test]
fn sharp_s_matches_case_insensitively() {
    // `ẞ` (U+1E9E capital sharp s) lowercases to `ß` (U+00DF) — a single
    // char, NOT `ss` (that's uppercase/full-case-fold behavior, not
    // `char::to_lowercase`). Covers non-ASCII case-insensitive matching, and
    // that a later mention still resolves after it.
    let members = vec![m("ẞ", &pk('d')), m("Max", &pk('b'))];
    assert_eq!(
        resolve_mention_pubkeys("@ẞ and @Max", &members),
        vec![pk('d'), pk('b')]
    );
}

// Adversarial rows from Quinn's re-review (the two `ẞ→ss`-premised ones were
// dropped as vacuous — `ẞ` lowercases to `ß`, one char, so it never inverts
// original-vs-folded length; only `İ` does).

#[test]
fn combining_mark_in_name_matches() {
    // A name carrying a combining mark (`é` as `e` + U+0301) matches the same
    // sequence in text (1:1 folding) and terminates cleanly.
    let members = vec![m("Jos\u{0065}\u{0301}", &pk('4'))]; // "José" decomposed
    assert_eq!(
        resolve_mention_pubkeys("hi @Jos\u{0065}\u{0301}!", &members),
        vec![pk('4')]
    );
}

#[test]
fn expanding_name_at_trailing_boundary() {
    // Expansion at the very end: `@İ` with nothing after must match, and
    // `@İx` (x extends the name, no `İx` member) must NOT match `İ`.
    let members = vec![m("İ", &pk('5'))];
    assert_eq!(resolve_mention_pubkeys("@İ", &members), vec![pk('5')]);
    assert!(resolve_mention_pubkeys("@İx", &members).is_empty());
}

#[test]
fn back_to_back_at_is_one_mention() {
    // `@İ@Robby`: the second `@` is preceded by a name char (`İ`), so it is
    // NOT at a left boundary — same rule as `alice@Robby`. Back-to-back
    // `@a@b` is intentionally one mention; a separator is required to wake
    // both. The expanding first name (`İ` → 2 folded chars) also proves the
    // span accounting stays in original coordinates.
    let members = vec![m("İ", &pk('5')), m("Robby", &pk('a'))];
    assert_eq!(resolve_mention_pubkeys("@İ@Robby", &members), vec![pk('5')]);
    // ASCII control: same shape, same outcome — it's the boundary rule, not
    // a Unicode span-accounting bug.
    let ascii = vec![m("Sam", &pk('6')), m("Robby", &pk('a'))];
    assert_eq!(resolve_mention_pubkeys("@Sam@Robby", &ascii), vec![pk('6')]);
    // With a separator, both wake.
    assert_eq!(
        resolve_mention_pubkeys("@İ @Robby", &members),
        vec![pk('5'), pk('a')]
    );
}

#[test]
fn ambiguous_name_wakes_no_one() {
    // Six "Fizz" agents (real team case) with distinct pubkeys → tag none.
    let members = vec![
        m("Fizz", &pk('6')),
        m("Fizz", &pk('7')),
        m("Fizz", &pk('8')),
    ];
    assert!(resolve_mention_pubkeys("@Fizz status?", &members).is_empty());
}

#[test]
fn duplicate_name_same_pubkey_is_not_ambiguous() {
    // Same identity listed twice (e.g. two channels) is not a conflict.
    let members = vec![m("Fizz", &pk('6')), m("Fizz", &pk('6'))];
    assert_eq!(resolve_mention_pubkeys("@Fizz go", &members), vec![pk('6')]);
}

#[test]
fn dedupes_repeated_mentions_in_first_appearance_order() {
    let members = vec![m("Robby", &pk('a')), m("Max", &pk('b'))];
    assert_eq!(
        resolve_mention_pubkeys("@Max then @Robby then @Max again", &members),
        vec![pk('b'), pk('a')]
    );
}
