// A small backtracking regex for selector values (`[text=/…/i]`,
// `:text-matches(…)`): the crate is std-only. It searches (matches anywhere,
// like Playwright's text regexes).
//
// Supported: literals, `.`, `^` `$`, `[…]` / `[^…]` with ranges, `\d \w \s`
// and their negations, `\` escapes, `( … )` / `(?: … )` (no captures), `|`,
// `* + ? {m} {m,} {m,n}` and their lazy `?` forms. Case-insensitive folds
// both sides to lowercase. A step budget stops pathological backtracking
// (the match then fails rather than hangs).

/// Matching steps per `is_match`, well past anything a UI string needs.
const STEP_BUDGET: u32 = 200_000;

#[derive(Debug, Clone)]
pub struct Regex {
    alt: Alt,
    ci: bool,
}

type Alt = Vec<Vec<Node>>;

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class { items: Vec<ClassItem>, negated: bool },
    Start,
    End,
    Group(Alt),
    Repeat { node: Box<Node>, min: u32, max: Option<u32>, lazy: bool },
}

#[derive(Debug, Clone)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

impl Regex {
    pub fn new(pattern: &str, case_insensitive: bool) -> Result<Regex, String> {
        let chars: Vec<char> = if case_insensitive { fold(pattern) } else { pattern.chars().collect() };
        let mut p = Parser { s: &chars, i: 0 };
        let alt = p.alt()?;
        if p.i < chars.len() {
            return Err(format!("regex: unexpected '{}' at {}", chars[p.i], p.i));
        }
        Ok(Regex { alt, ci: case_insensitive })
    }

    pub fn is_match(&self, text: &str) -> bool {
        let t: Vec<char> = if self.ci { fold(text) } else { text.chars().collect() };
        let mut budget = STEP_BUDGET;
        (0..=t.len()).any(|start| {
            let mut m = Matcher { t: &t, budget: &mut budget };
            m.alt(&self.alt, start, &mut |_, _| true)
        })
    }
}

fn fold(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> { self.s.get(self.i).copied() }

    fn alt(&mut self) -> Result<Alt, String> {
        let mut alt = vec![self.seq()?];
        while self.peek() == Some('|') {
            self.i += 1;
            alt.push(self.seq()?);
        }
        Ok(alt)
    }

    fn seq(&mut self) -> Result<Vec<Node>, String> {
        let mut seq = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' { break; }
            let atom = self.atom()?;
            seq.push(self.quantified(atom)?);
        }
        Ok(seq)
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("regex: unexpected end")?;
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                if self.s.get(self.i) == Some(&'?') && self.s.get(self.i + 1) == Some(&':') { self.i += 2; }
                let alt = self.alt()?;
                if self.peek() != Some(')') { return Err("regex: unclosed (".into()); }
                self.i += 1;
                Node::Group(alt)
            }
            '[' => self.class()?,
            '\\' => match self.escape()? {
                Esc::Char(c) => Node::Char(c),
                Esc::Class(item) => Node::Class { items: vec![item], negated: false },
            },
            '*' | '+' | '?' | '{' => return Err(format!("regex: nothing to repeat before '{c}'")),
            ')' => return Err("regex: unmatched )".into()),
            c => Node::Char(c),
        })
    }

    fn escape(&mut self) -> Result<Esc, String> {
        let c = self.peek().ok_or("regex: trailing \\")?;
        self.i += 1;
        Ok(match c {
            'd' => Esc::Class(ClassItem::Digit(true)),
            'D' => Esc::Class(ClassItem::Digit(false)),
            'w' => Esc::Class(ClassItem::Word(true)),
            'W' => Esc::Class(ClassItem::Word(false)),
            's' => Esc::Class(ClassItem::Space(true)),
            'S' => Esc::Class(ClassItem::Space(false)),
            'n' => Esc::Char('\n'),
            't' => Esc::Char('\t'),
            c => Esc::Char(c),
        })
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = self.peek() == Some('^');
        if negated { self.i += 1; }
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("regex: unclosed [")?;
            if c == ']' && !first { self.i += 1; break; }
            first = false;
            self.i += 1;
            let lo = if c == '\\' {
                match self.escape()? {
                    Esc::Class(item) => { items.push(item); continue; }
                    Esc::Char(c) => c,
                }
            } else { c };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|&n| n != ']') {
                self.i += 1;
                let mut hi = self.peek().ok_or("regex: unclosed [")?;
                self.i += 1;
                if hi == '\\' {
                    match self.escape()? { Esc::Char(c) => hi = c, Esc::Class(_) => return Err("regex: class in range".into()) }
                }
                if hi < lo { return Err(format!("regex: bad range {lo}-{hi}")); }
                items.push(ClassItem::Range(lo, hi));
            } else {
                items.push(ClassItem::Range(lo, lo));
            }
        }
        Ok(Node::Class { items, negated })
    }

    fn quantified(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => { self.i += 1; (0, None) }
            Some('+') => { self.i += 1; (1, None) }
            Some('?') => { self.i += 1; (0, Some(1)) }
            Some('{') => {
                self.i += 1;
                let min = self.number().ok_or("regex: bad {m,n}")?;
                let max = if self.peek() == Some(',') {
                    self.i += 1;
                    if self.peek() == Some('}') { None } else { Some(self.number().ok_or("regex: bad {m,n}")?) }
                } else { Some(min) };
                if self.peek() != Some('}') { return Err("regex: unclosed {".into()); }
                self.i += 1;
                if max.is_some_and(|m| m < min) { return Err("regex: {m,n} with n < m".into()); }
                (min, max)
            }
            _ => return Ok(atom),
        };
        if matches!(atom, Node::Start | Node::End) { return Err("regex: nothing to repeat".into()); }
        let lazy = self.peek() == Some('?');
        if lazy { self.i += 1; }
        if matches!(self.peek(), Some('*' | '+' | '{')) || (self.peek() == Some('?') && lazy) {
            return Err("regex: repeated quantifier".into());
        }
        Ok(Node::Repeat { node: Box::new(atom), min, max, lazy })
    }

    fn number(&mut self) -> Option<u32> {
        let begin = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) { self.i += 1; }
        self.s[begin..self.i].iter().collect::<String>().parse().ok()
    }
}

enum Esc {
    Char(char),
    Class(ClassItem),
}

struct Matcher<'a, 'b> {
    t: &'a [char],
    budget: &'b mut u32,
}

/// What to do with the position after the part just matched.
type Cont<'c, 'a, 'b> = dyn FnMut(&mut Matcher<'a, 'b>, usize) -> bool + 'c;

impl<'a, 'b> Matcher<'a, 'b> {
    fn step(&mut self) -> bool {
        if *self.budget == 0 { return false; }
        *self.budget -= 1;
        true
    }

    fn alt(&mut self, alt: &Alt, pos: usize, k: &mut Cont<'_, 'a, 'b>) -> bool {
        for seq in alt {
            if self.seq(seq, 0, pos, k) { return true; }
        }
        false
    }

    fn seq(&mut self, seq: &[Node], i: usize, pos: usize, k: &mut Cont<'_, 'a, 'b>) -> bool {
        if !self.step() { return false; }
        match seq.get(i) {
            None => k(self, pos),
            Some(Node::Repeat { node, min, max, lazy }) => {
                self.repeat(node, *min, *max, *lazy, 0, pos, &mut |m: &mut Matcher<'a, 'b>, p| m.seq(seq, i + 1, p, k))
            }
            Some(node) => self.node(node, pos, &mut |m: &mut Matcher<'a, 'b>, p| m.seq(seq, i + 1, p, k)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(&mut self, node: &Node, min: u32, max: Option<u32>, lazy: bool, count: u32, pos: usize, k: &mut Cont<'_, 'a, 'b>) -> bool {
        if !self.step() { return false; }
        let can_stop = count >= min;
        let can_more = max.is_none_or(|m| count < m);
        if lazy && can_stop && k(self, pos) { return true; }
        // A repetition that consumed nothing can't make progress: don't loop on it.
        if can_more && self.node(node, pos, &mut |m: &mut Matcher<'a, 'b>, p| p != pos && m.repeat(node, min, max, lazy, count + 1, p, k)) {
            return true;
        }
        !lazy && can_stop && k(self, pos)
    }

    fn node(&mut self, node: &Node, pos: usize, k: &mut Cont<'_, 'a, 'b>) -> bool {
        match node {
            Node::Start => pos == 0 && k(self, pos),
            Node::End => pos == self.t.len() && k(self, pos),
            Node::Any => pos < self.t.len() && self.t[pos] != '\n' && k(self, pos + 1),
            Node::Char(c) => self.t.get(pos) == Some(c) && k(self, pos + 1),
            Node::Class { items, negated } => {
                let Some(&c) = self.t.get(pos) else { return false };
                (items.iter().any(|it| class_has(it, c)) != *negated) && k(self, pos + 1)
            }
            Node::Group(alt) => self.alt(alt, pos, k),
            Node::Repeat { node, min, max, lazy } => self.repeat(node, *min, *max, *lazy, 0, pos, k),
        }
    }
}

fn class_has(item: &ClassItem, c: char) -> bool {
    match *item {
        ClassItem::Range(lo, hi) => lo <= c && c <= hi,
        ClassItem::Digit(want) => c.is_ascii_digit() == want,
        ClassItem::Word(want) => (c.is_alphanumeric() || c == '_') == want,
        ClassItem::Space(want) => c.is_whitespace() == want,
    }
}

#[cfg(test)]
mod tests {
    use super::Regex;

    fn m(pat: &str, s: &str) -> bool { Regex::new(pat, false).unwrap().is_match(s) }
    fn mi(pat: &str, s: &str) -> bool { Regex::new(pat, true).unwrap().is_match(s) }

    #[test]
    fn literals_dot_and_anchors() {
        assert!(m("abc", "xxabcxx"));
        assert!(!m("abd", "abc"));
        assert!(m("a.c", "abc") && !m("a.c", "ac"));
        assert!(m("^ab", "abc") && !m("^bc", "abc"));
        assert!(m("bc$", "abc") && !m("ab$", "abc"));
        assert!(m("^$", "") && !m("^$", "a"));
    }

    #[test]
    fn quantifiers() {
        assert!(m("^ab*c$", "ac") && m("^ab*c$", "abbbc"));
        assert!(m("^ab+c$", "abc") && !m("^ab+c$", "ac"));
        assert!(m("^ab?c$", "ac") && m("^ab?c$", "abc") && !m("^ab?c$", "abbc"));
        assert!(m(r"^\d{2,3}$", "12") && m(r"^\d{2,3}$", "123") && !m(r"^\d{2,3}$", "1234"));
        assert!(m(r"^\d{2}$", "12") && !m(r"^\d{2}$", "1"));
        assert!(m(r"^\d{2,}$", "12345") && !m(r"^\d{2,}$", "1"));
        assert!(m("^a.*?b$", "axxb"));
    }

    #[test]
    fn classes_and_escapes() {
        assert!(m(r"^[a-c]+$", "abcab") && !m(r"^[a-c]+$", "abd"));
        assert!(m(r"^[^0-9]+$", "abc") && !m(r"^[^0-9]+$", "a1"));
        assert!(m(r"^\w+\s\d$", "item_x 7"));
        assert!(m(r"^\S+$", "a-b") && !m(r"^\S+$", "a b"));
        assert!(m(r"^a\.b$", "a.b") && !m(r"^a\.b$", "axb"));
        assert!(m(r"^[\d.]+$", "1.5"));
        assert!(m(r"^[가-힣]+$", "설정") && !m(r"^[가-힣]+$", "abc"));
    }

    #[test]
    fn groups_and_alternation() {
        assert!(m("^(ab)+$", "abab") && !m("^(ab)+$", "aba"));
        assert!(m("^(log|sign) ?in$", "login") && m("^(log|sign) ?in$", "sign in"));
        assert!(!m("^(log|sign)in$", "logsignin"));
        assert!(m("cat|dog", "hotdog"));
        assert!(m("^(?:a|b)c$", "bc"));
    }

    #[test]
    fn case_insensitive() {
        assert!(mi("sign in", "SIGN IN") && !m("sign in", "SIGN IN"));
        assert!(mi("^[a-z]+$", "ABC"));
        assert!(mi("Ä", "ä"));
    }

    #[test]
    fn errors_and_pathological_input() {
        for bad in ["(ab", "ab)", "[ab", "a{2", "*a", "a**", "a{3,2}", r"\"] {
            assert!(Regex::new(bad, false).is_err(), "{bad}");
        }
        // Exponential backtracking is cut off, not hung on.
        assert!(!m("^(a+)+$", &("a".repeat(40) + "b")));
    }
}
