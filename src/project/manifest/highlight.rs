/*! `[highlight]`: syntax rules for the code blocks on the package's page on
the Ember site.

```toml
[[highlight.rules]]
languages = ["alx"]
type = "keyword"
before = '\{'
words = ["#if", ":else", "/if", "#each", "/each"]
```

a rule gives a token type of the Alloy semantic token legend, the site
grammars that it extends, and a list of words or a pattern. `before` is a
pattern that must come just before the match, and the site does not colour
it. the site adds the rules to its own grammar. it never runs code of the
package.

the patterns run in each visitor's browser, on a backtracking engine. so
`check` takes a small subset of regex that JavaScript (highlight.js, no `u`
flag) and Rust read the same way:

- printable ASCII only. a pattern matches ASCII text only: there is no `.`,
  no negated class, and no `\W` or `\D`. JavaScript reads those per UTF-16
  unit and Rust per character.
- `\d`, `\w` and `\b` are ASCII, as JavaScript reads them without the `u`
  flag. there is no `\s`: the two engines give it other characters.
- groups are `(?: )`. no capture, no lookaround, no flag, no backreference,
  and no `^` or `$`, which highlight.js reads at each line.
- each repeat has an upper bound: `?`, `{n}` or `{n,m}`, never `*`, `+` or
  `{n,}`. a repeated group holds no repeat and no `|`. the count of ways
  that one rule can try at one place is 256 at most, so the work for each
  character of a code block has a fixed limit.

the registry makes the same checks, in `src/highlight.rs` of
ember-luau/backend. change the two together. the site makes them again and
skips a rule that fails, so a bad rule cannot break a page. */

use toml::Value;

/** the token types of the Alloy semantic token legend, which are the
LSP's standard types. an ingot sends one of these with each token. */
pub const TYPES: [&str; 23] = [
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "event",
    "function",
    "method",
    "macro",
    "keyword",
    "modifier",
    "comment",
    "string",
    "number",
    "regexp",
    "operator",
    "decorator",
];

/** the site grammars that a rule can extend. `luau` also covers ```lua
fences and `aly` covers ```alloy fences. */
pub const LANGUAGES: [&str; 5] = ["luau", "aly", "alx", "loom", "luwu"];

/// the most rules in one package.
const MAX_RULES: usize = 32;
/// the longest `match` or `before`, in characters.
const MAX_PATTERN: usize = 256;
/// the most words in one rule.
const MAX_WORDS: usize = 64;
/// the longest word, in characters.
const MAX_WORD: usize = 64;
/// the most characters of all patterns and words together. the table goes
/// into each index entry, so this keeps the index small.
const MAX_TOTAL: usize = 4096;
/// the most ways that one rule can try at one place of the text.
const MAX_WAYS: u64 = 256;

const RULE_KEYS: [&str; 5] = ["languages", "type", "match", "words", "before"];

/// checks a `[highlight]` table. the error names the rule and the problem.
pub fn check(highlight: &Value) -> Result<(), String> {
    let table = highlight
        .as_table()
        .ok_or("[highlight] is not a table; write [[highlight.rules]]")?;
    if let Some(key) = table.keys().find(|key| *key != "rules") {
        return Err(format!(
            "[highlight] has the key `{key}`; it takes `rules` only"
        ));
    }
    let rules = table
        .get("rules")
        .and_then(Value::as_array)
        .filter(|rules| !rules.is_empty())
        .ok_or("[highlight] has no rules; add a [[highlight.rules]] table")?;
    if rules.len() > MAX_RULES {
        return Err(format!(
            "[highlight] has {} rules; the most is {MAX_RULES}",
            rules.len()
        ));
    }
    let mut total = 0;
    for (index, rule) in rules.iter().enumerate() {
        check_rule(rule, &mut total)
            .map_err(|why| format!("[highlight] rule {}: {why}", index + 1))?;
    }
    if total > MAX_TOTAL {
        return Err(format!(
            "[highlight] has {total} characters of patterns and words; the most is {MAX_TOTAL}"
        ));
    }
    Ok(())
}

fn check_rule(rule: &Value, total: &mut usize) -> Result<(), String> {
    let rule = rule.as_table().ok_or("is not a table")?;
    if let Some(key) = rule.keys().find(|key| !RULE_KEYS.contains(&key.as_str())) {
        return Err(format!(
            "has the key `{key}`; a rule takes {}",
            RULE_KEYS.join(", ")
        ));
    }
    let kind = rule
        .get("type")
        .and_then(Value::as_str)
        .ok_or("has no `type` text")?;
    if !TYPES.contains(&kind) {
        return Err(format!(
            "has the type `{kind}`, which is not a token type; use one of {}",
            TYPES.join(", ")
        ));
    }
    let languages = rule
        .get("languages")
        .and_then(Value::as_array)
        .filter(|languages| !languages.is_empty())
        .ok_or("has no `languages` list")?;
    for language in languages {
        if !language
            .as_str()
            .is_some_and(|name| LANGUAGES.contains(&name))
        {
            return Err(format!(
                "has the language {language}, which is not a site grammar; use {}",
                LANGUAGES.join(", ")
            ));
        }
    }
    let before = match rule.get("before") {
        Some(before) => {
            let before = before.as_str().ok_or("has a `before` that is not text")?;
            *total += before.len();
            pattern(before)
                .map_err(|why| format!("`before` {why}"))?
                .ways
        }
        None => 1,
    };
    let ways = match (rule.get("match"), rule.get("words")) {
        (Some(_), Some(_)) => return Err("has `match` and `words`; give one of the two".into()),
        (None, None) => return Err("has no `match` and no `words`; give one of the two".into()),
        (Some(text), None) => {
            let text = text.as_str().ok_or("has a `match` that is not text")?;
            *total += text.len();
            let read = pattern(text).map_err(|why| format!("`match` {why}"))?;
            if read.min == 0 {
                return Err(
                    "`match` can match empty text; the site needs at least one character".into(),
                );
            }
            read.ways
        }
        (None, Some(words)) => check_words(words, total)?,
    };
    if before.saturating_mul(ways) > MAX_WAYS {
        return Err(format!(
            "`before` and the match can try more than {MAX_WAYS} ways at one place; use fewer repeats or words"
        ));
    }
    Ok(())
}

/// checks a `words` list and gives the count of words.
fn check_words(words: &Value, total: &mut usize) -> Result<u64, String> {
    let words = words
        .as_array()
        .filter(|words| !words.is_empty())
        .ok_or("has a `words` that is not a list of words")?;
    if words.len() > MAX_WORDS {
        return Err(format!(
            "has {} words; the most is {MAX_WORDS}",
            words.len()
        ));
    }
    for word in words {
        let word = word.as_str().ok_or("has a word that is not text")?;
        *total += word.len();
        if word.is_empty() || word.len() > MAX_WORD {
            return Err(format!(
                "has the word {word:?}; a word has 1 to {MAX_WORD} characters"
            ));
        }
        if !word.bytes().all(|byte| (b' '..=b'~').contains(&byte)) || word.trim() != word {
            return Err(format!(
                "has the word {word:?}; a word takes printable ASCII, with no space at its ends"
            ));
        }
    }
    Ok(words.len() as u64)
}

/// what a pattern can do at one place of the text.
#[derive(Clone, Copy)]
struct Read {
    /// the fewest characters that it matches.
    min: usize,
    /// the most ways that a backtracking engine can try.
    ways: u64,
    /// it holds a repeat or a `|`, so a repeat of it can backtrack badly.
    branches: bool,
}

impl Read {
    const CHAR: Read = Read {
        min: 1,
        ways: 1,
        branches: false,
    };
}

/// reads a pattern of the subset that the module comment gives.
fn pattern(text: &str) -> Result<Read, String> {
    if text.is_empty() {
        return Err("is empty".into());
    }
    if text.len() > MAX_PATTERN {
        return Err(format!(
            "has {} characters; the most is {MAX_PATTERN}",
            text.len()
        ));
    }
    if let Some(c) = text.chars().find(|c| !(' '..='~').contains(c)) {
        return Err(format!(
            "has the character {c:?}; a pattern takes printable ASCII only"
        ));
    }
    let mut parser = Parser {
        text: text.as_bytes(),
        at: 0,
    };
    let read = parser.alternation()?;
    if parser.at < text.len() {
        return Err(format!(
            "has a `)` with no `(` at character {}",
            parser.at + 1
        ));
    }
    if read.ways > MAX_WAYS {
        return Err(format!(
            "can try more than {MAX_WAYS} ways at one place; use fewer or smaller repeats"
        ));
    }
    Ok(read)
}

/// what came before a `-` in a class.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Before {
    Nothing,
    Char(u8),
    Escape,
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn alternation(&mut self) -> Result<Read, String> {
        let mut read = self.sequence()?;
        while self.peek() == Some(b'|') {
            self.at += 1;
            let next = self.sequence()?;
            read = Read {
                min: read.min.min(next.min),
                ways: read.ways.saturating_add(next.ways),
                branches: true,
            };
        }
        Ok(read)
    }

    fn sequence(&mut self) -> Result<Read, String> {
        let mut read = Read {
            min: 0,
            ways: 1,
            branches: false,
        };
        while let Some(byte) = self.peek() {
            if byte == b'|' || byte == b')' {
                break;
            }
            let atom = self.atom()?;
            let atom = self.repeat(atom)?;
            read.min += atom.min;
            read.ways = read.ways.saturating_mul(atom.ways);
            read.branches |= atom.branches;
        }
        Ok(read)
    }

    fn atom(&mut self) -> Result<Read, String> {
        let byte = self.text[self.at];
        self.at += 1;
        match byte {
            b'(' => {
                if !self.text[self.at..].starts_with(b"?:") {
                    return Err(group_error(&self.text[self.at..]));
                }
                self.at += 2;
                let inner = self.alternation()?;
                if self.peek() != Some(b')') {
                    return Err("has a `(` with no `)`".into());
                }
                self.at += 1;
                Ok(inner)
            }
            b'[' => self.class(),
            b'\\' => {
                let escaped = self.peek().ok_or("ends with a lone `\\`")?;
                self.at += 1;
                match escaped {
                    b'b' | b'B' => Ok(Read {
                        min: 0,
                        ..Read::CHAR
                    }),
                    _ => escape(escaped, false).map(|_| Read::CHAR),
                }
            }
            b'.' => Err(
                "has `.`, which JavaScript and Rust read differently; use a class such as [A-Za-z]"
                    .into(),
            ),
            b'^' | b'$' => Err(format!(
                "has the anchor `{}`, which highlight.js reads at each line; leave it out",
                byte as char
            )),
            b'*' | b'+' | b'?' => Err(format!(
                "has a `{}` with nothing before it to repeat",
                byte as char
            )),
            b'{' | b'}' | b']' => Err(format!("has a lone `{0}`; write it as \\{0}", byte as char)),
            _ => Ok(Read::CHAR),
        }
    }

    /// reads a class after its `[`. a class gives one character.
    fn class(&mut self) -> Result<Read, String> {
        match self.peek() {
            Some(b'^') => {
                return Err("has a negated class, which can match text other than ASCII; list the characters".into());
            }
            Some(b']') => return Err("has an empty class `[]`".into()),
            _ => {}
        }
        // what came before a `-`: a character starts a range, a class
        // escape is a range that Rust refuses, nothing makes `-` a mark.
        let mut previous = Before::Nothing;
        loop {
            let byte = self.peek().ok_or("has a `[` with no `]`")?;
            self.at += 1;
            match byte {
                b']' => return Ok(Read::CHAR),
                b'[' => {
                    return Err(
                        "has a `[` in a class, which Rust reads as a nested class; write it as \\["
                            .into(),
                    );
                }
                b'&' | b'~' | b'-' if self.peek() == Some(byte) => {
                    return Err(format!(
                        "has `{0}{0}` in a class, which Rust reads as a set operation",
                        byte as char
                    ));
                }
                b'-' if previous != Before::Nothing && self.peek() != Some(b']') => {
                    let Before::Char(low) = previous else {
                        return Err("has a `-` after a class escape, which Rust refuses as a range; write \\-".into());
                    };
                    let high = match self.peek().ok_or("has a `[` with no `]`")? {
                        b'[' => {
                            return Err("has a `[` in a class, which Rust reads as a nested class; write it as \\[".into());
                        }
                        b'\\' => {
                            self.at += 1;
                            let escaped = self.peek().ok_or("ends with a lone `\\`")?;
                            self.at += 1;
                            escape(escaped, true)?
                                .ok_or("has a range that ends in a class escape")?
                        }
                        high => {
                            self.at += 1;
                            high
                        }
                    };
                    if high < low {
                        return Err(format!(
                            "has the range {}-{}, which runs backward",
                            low as char, high as char
                        ));
                    }
                    previous = Before::Nothing;
                }
                b'\\' => {
                    let escaped = self.peek().ok_or("ends with a lone `\\`")?;
                    self.at += 1;
                    previous = match escape(escaped, true)? {
                        Some(char) => Before::Char(char),
                        None => Before::Escape,
                    };
                }
                _ => previous = Before::Char(byte),
            }
        }
    }

    /// reads the repeat after an atom, if there is one.
    fn repeat(&mut self, atom: Read) -> Result<Read, String> {
        let (low, high) = match self.peek() {
            Some(b'?') => {
                self.at += 1;
                (0, 1)
            }
            Some(b'*' | b'+') => {
                return Err(format!(
                    "has `{}`, a repeat with no upper bound; write {{0,32}} or {{1,32}}",
                    self.text[self.at] as char
                ));
            }
            Some(b'{') => {
                self.at += 1;
                self.counts()?
            }
            _ => return Ok(atom),
        };
        if matches!(self.peek(), Some(b'?' | b'*' | b'+' | b'{')) {
            return Err("has a repeat of a repeat; write one repeat".into());
        }
        if atom.min == 0 {
            return Err("has a repeat of something that matches no character".into());
        }
        if atom.branches {
            return Err(
                "has a repeat of a group that holds a repeat or a `|`, which can backtrack badly"
                    .into(),
            );
        }
        Ok(Read {
            min: atom.min * low,
            ways: high - low as u64 + 1,
            branches: true,
        })
    }

    /// reads `n}` or `n,m}` after a `{`, and gives the two bounds.
    fn counts(&mut self) -> Result<(usize, u64), String> {
        let low = self.number();
        let high = if self.peek() == Some(b',') {
            self.at += 1;
            self.number()
        } else {
            low
        };
        if self.peek() != Some(b'}') || low.is_none() {
            return Err("has a `{` that is no repeat; write a lone { as \\{".into());
        }
        self.at += 1;
        let (Some(low), Some(high)) = (low, high) else {
            return Err("has a repeat with no upper bound; write {n,m}".into());
        };
        if high < low || high == 0 {
            return Err(format!(
                "has the repeat {{{low},{high}}}, which matches nothing"
            ));
        }
        Ok((low as usize, high))
    }

    /// reads up to three digits.
    fn number(&mut self) -> Option<u64> {
        let start = self.at;
        while self.at - start < 3 && self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.at += 1;
        }
        std::str::from_utf8(&self.text[start..self.at])
            .ok()?
            .parse()
            .ok()
    }
}

/** reads the character after a `\`. gives the character that the escape
matches, or None for `\d` and `\w`. refuses an escape that JavaScript and
Rust read differently or that one of the two refuses. */
fn escape(byte: u8, in_class: bool) -> Result<Option<u8>, String> {
    match byte {
        b'd' | b'w' => Ok(None),
        b'n' => Ok(Some(b'\n')),
        b't' => Ok(Some(b'\t')),
        b'r' => Ok(Some(b'\r')),
        b'b' | b'B' if in_class => Err(format!(
            "has `\\{}` in a class, which JavaScript and Rust read differently",
            byte as char
        )),
        // Rust reads \< and \> as word boundaries, JavaScript as < and >.
        b'<' | b'>' => Err(format!(
            "has `\\{}`, which JavaScript and Rust read differently; write {} alone",
            byte as char, byte as char
        )),
        _ if byte.is_ascii_punctuation() => Ok(Some(byte)),
        b's' | b'S' => Err(format!(
            "has `\\{}`, which JavaScript and Rust read differently; write a class such as [ \\t]",
            byte as char
        )),
        _ => Err(format!(
            "has the escape `\\{}`, which the site does not take; use \\d, \\w, \\b, \\n, \\t, \\r or an escaped mark",
            byte as char
        )),
    }
}

/// the error for a `(` that does not start `(?:`.
fn group_error(after: &[u8]) -> String {
    if after.starts_with(b"?=")
        || after.starts_with(b"?!")
        || after.starts_with(b"?<=")
        || after.starts_with(b"?<!")
    {
        "has a lookaround, which Rust does not read; use `before`".into()
    } else if after.starts_with(b"?") {
        "has a named group or a flag; write a group as (?: )".into()
    } else {
        "has a capture group; write a group as (?: )".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Value {
        Value::Table(toml::from_str(text).unwrap())
    }

    fn rule(body: &str) -> Value {
        table(&format!(
            "[[rules]]\nlanguages = [\"alx\"]\ntype = \"keyword\"\n{body}\n"
        ))
    }

    /** the rules of svelte-aly pass, and so do the usual shapes: a word
    list, a sigil and a name, a pattern with a boundary on each side. */
    #[test]
    fn good_rules_pass() {
        for body in [
            "before = '\\{'\nwords = [\"#if\", \":else\", \"/if\", \"#each\", \"/each\"]",
            "match = '\\$[A-Za-z_][A-Za-z0-9_]{0,63}'",
            "match = '\\b(?:fn|let|mut)\\b'",
            "match = ':else[ \\t]{1,8}if\\b|:else\\b'",
            "match = '[#:/@](?:if|each)'",
            "match = '[-+]'",
            "match = '[a-]'",
            "match = '\\]\\}\\{\\.\\*'",
            "before = '\\b'\nmatch = 'x'",
            "words = [\"and also\"]",
        ] {
            assert_eq!(check(&rule(body)), Ok(()), "{body}");
        }
    }

    /** each pattern that JavaScript and Rust read differently, or that
    can backtrack badly, is refused with its reason. */
    #[test]
    fn bad_patterns_are_refused() {
        for (pattern, why) in [
            ("(if)", "has a capture group"),
            ("(?=if)x", "has a lookaround"),
            ("(?<=\\{)x", "has a lookaround"),
            ("(?i)x", "has a named group or a flag"),
            ("(?<name>x)", "has a named group or a flag"),
            ("a.b", "has `.`"),
            ("^x", "has the anchor `^`"),
            ("x$", "has the anchor `$`"),
            ("a*", "has `*`, a repeat with no upper bound"),
            ("a+", "has `+`, a repeat with no upper bound"),
            ("a{2,}", "has a repeat with no upper bound"),
            ("a{,2}", "has a `{` that is no repeat"),
            ("a{x}", "has a `{` that is no repeat"),
            ("a{3,1}", "which matches nothing"),
            ("{", "has a lone `{`"),
            ("}", "has a lone `}`"),
            ("]", "has a lone `]`"),
            ("?a", "has a `?` with nothing before it"),
            ("a??", "has a repeat of a repeat"),
            ("a{1,2}?", "has a repeat of a repeat"),
            ("(?:a{1,3}){1,3}", "can backtrack badly"),
            ("(?:a|b){1,2}", "can backtrack badly"),
            (
                "x\\b?",
                "has a repeat of something that matches no character",
            ),
            (
                "x(?:\\b){2}",
                "has a repeat of something that matches no character",
            ),
            ("[^}]", "has a negated class"),
            ("[]a]", "has an empty class"),
            ("[[a]", "has a `[` in a class"),
            ("[a&&b]", "set operation"),
            ("[a--b]", "set operation"),
            ("[z-a]", "runs backward"),
            ("[A-[]", "has a `[` in a class"),
            ("[\\d-z]", "has a `-` after a class escape"),
            ("[a-\\d]", "has a range that ends in a class escape"),
            ("[\\b]", "in a class"),
            ("\\s", "has `\\s`"),
            ("\\W", "has the escape `\\W`"),
            ("\\1", "has the escape `\\1`"),
            ("\\p{L}", "has the escape `\\p`"),
            ("\\<", "has `\\<`"),
            ("a\\", "ends with a lone"),
            ("(?:a", "has a `(` with no `)`"),
            ("a)", "has a `)` with no `(`"),
            ("[a", "has a `[` with no `]`"),
            ("é", "printable ASCII only"),
            ("a?", "can match empty text"),
            ("\\b", "can match empty text"),
            ("[a-z]{0,20}[a-z]{0,20}", "can try more than 256 ways"),
        ] {
            let error = check(&rule(&format!("match = '{pattern}'"))).unwrap_err();
            assert!(
                error.starts_with("[highlight] rule 1: `match` "),
                "{pattern}: {error}"
            );
            assert!(error.contains(why), "{pattern}: {error}");
        }
        assert_eq!(check(&rule("match = '[a-z-0\\d_]'")), Ok(()));
        let long = format!("match = '{}'", "a".repeat(257));
        assert!(
            check(&rule(&long))
                .unwrap_err()
                .contains("has 257 characters")
        );
    }

    /// the table, the rule keys, the type, the languages and the words.
    #[test]
    fn bad_tables_are_refused() {
        let cases = [
            (table("rule = []"), "[highlight] has the key `rule`"),
            (table("rules = []"), "[highlight] has no rules"),
            (
                rule("match = 'x'\ncolor = \"red\""),
                "rule 1: has the key `color`",
            ),
            (
                table("[[rules]]\nlanguages = [\"alx\"]\nmatch = 'x'"),
                "rule 1: has no `type` text",
            ),
            (
                table("[[rules]]\nlanguages = [\"alx\"]\ntype = \"keywords\"\nmatch = 'x'"),
                "rule 1: has the type `keywords`, which is not a token type",
            ),
            (
                table("[[rules]]\ntype = \"keyword\"\nmatch = 'x'"),
                "rule 1: has no `languages` list",
            ),
            (
                table("[[rules]]\nlanguages = [\"svelte\"]\ntype = \"keyword\"\nmatch = 'x'"),
                "rule 1: has the language \"svelte\", which is not a site grammar",
            ),
            (
                rule("match = 'x'\nwords = [\"x\"]"),
                "rule 1: has `match` and `words`",
            ),
            (rule(""), "rule 1: has no `match` and no `words`"),
            (
                rule("words = []"),
                "rule 1: has a `words` that is not a list",
            ),
            (rule("words = [\"\"]"), "rule 1: has the word \"\""),
            (rule("words = [\" if\"]"), "no space at its ends"),
            (rule("words = [\"süß\"]"), "printable ASCII"),
            (
                rule("before = '[a-z]{0,9}'\nmatch = '[a-z]{1,30}'"),
                "rule 1: `before` and the match can try more than 256 ways",
            ),
        ];
        for (value, why) in cases {
            let error = check(&value).unwrap_err();
            assert!(error.contains(why), "{error}");
        }
        let many = (0..65).map(|n| format!("\"w{n}\"")).collect::<Vec<_>>();
        let error = check(&rule(&format!("words = [{}]", many.join(", ")))).unwrap_err();
        assert!(error.contains("has 65 words; the most is 64"), "{error}");
    }

    /// the caps on the count of rules and on the size of all of them.
    #[test]
    fn the_caps_hold() {
        let one = "[[rules]]\nlanguages = [\"aly\"]\ntype = \"keyword\"\nmatch = 'x'\n";
        assert_eq!(check(&table(&one.repeat(32))), Ok(()));
        assert_eq!(
            check(&table(&one.repeat(33))).unwrap_err(),
            "[highlight] has 33 rules; the most is 32"
        );
        let word = format!("\"{}\"", "w".repeat(64));
        let words = vec![word; 64].join(", ");
        let big =
            format!("[[rules]]\nlanguages = [\"aly\"]\ntype = \"keyword\"\nwords = [{words}]\n")
                .repeat(2);
        assert_eq!(
            check(&table(&big)).unwrap_err(),
            "[highlight] has 8192 characters of patterns and words; the most is 4096"
        );
    }
}
