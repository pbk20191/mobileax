// CSS-like selector engine for the daemon's JSON dump tree.
//
// Supported syntax (all client-side — no daemon round-trip per match):
//
//   Tag                  match by class name (simple-name or fully-qualified)
//   *                    any node
//   [attr=value]         exact attribute match
//   [attr~=value]        substring
//   [attr^=value]        prefix
//   [attr$=value]        suffix
//   [attr]               attribute present (non-empty)
//   [attr="v" i]         any of the above, case-insensitive (CSS Selectors 4)
//   [attr=/re/i]         regex search (flags: i); see crate::regex
//                        Values may be quoted ("…" or '…', with \" \' \\ escapes);
//                        an unquoted value runs to the `]`. Unknown attribute
//                        names are an error, not a silent miss.
//   #name                resource-id, short (`#email`) or full (`#com.app:id/email`);
//                        `[id=email]` also matches the short form
//   :flag                node has the given a11y flag (clickable, enabled, editable, heading, …)
//   [role=…] [state=…]   Compose / androidx role and state descriptions (device language)
//   :has-text("foo")     substring text match (Playwright sugar for [text~=foo])
//   :text-is("foo")      exact text match    (Playwright sugar for [text=foo])
//   :text-matches("re", "i")  regex text match (Playwright's :text-matches)
//   :has(SEL)            has a descendant matching SEL; of nested matches only
//                        the innermost is kept (the row, not every ancestor)
//   :in(SEL)             descendant of any node matching SEL
//   :below(SEL)          top edge ≥ anchor's bottom edge   } a lone selector's
//   :right-of(SEL)       left edge ≥ anchor's right edge   } matches come
//   :near(SEL, PX)       centre-to-centre distance ≤ PX    } nearest first
//   Tag[a=v][b=v]:flag   AND-combined
//
// Multiple comma-separated selectors run as OR (any matches). The
// pseudo-class vocabulary intentionally mirrors Playwright's locator API
// (`getByText`, `near()`, `below()`) so muscle memory transfers from web
// test code.
//
// The matcher walks the JSON tree produced by `dump` / `dump_active` (the
// nested {cls, pkg, rid, text, desc, bounds, flags, children} objects).

use crate::json::Value;

#[derive(Debug, Clone)]
pub struct Selector {
    pub class: ClassMatch,
    pub attrs: Vec<AttrPred>,
    pub flags: Vec<char>,
    /// Relational pseudo-classes that need access to the surrounding tree
    /// (`:in(SEL)`, `:near(SEL, PX)`, `:below(SEL)`, `:right-of(SEL)`).
    /// Evaluated by `MatchCtx::matches` rather than `Selector::matches`.
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone)]
pub enum Relation {
    In(Vec<Selector>),                       // descendant of any matching ancestor
    Has(Vec<Selector>),                      // ancestor of any matching descendant
    Near(Vec<Selector>, i64),                // centre distance ≤ PX
    Below(Vec<Selector>),                    // top edge ≥ anchor's bottom
    RightOf(Vec<Selector>),                  // left edge ≥ anchor's right
}

#[derive(Debug, Clone)]
pub enum ClassMatch {
    Any,
    Exact(String),       // full match, e.g. "android.widget.EditText"
    Simple(String),      // last segment only, e.g. "EditText"
}

#[derive(Debug, Clone)]
pub struct AttrPred {
    pub key: String,     // normalised to the JSON field name ("rid", "text", …)
    pub op: AttrOp,
    pub val: String,
    /// The `i` flag: compare case-insensitively.
    pub ci: bool,
    /// For [`AttrOp::Regex`].
    pub re: Option<crate::regex::Regex>,
}

impl AttrPred {
    fn new(key: &str, op: AttrOp, val: String) -> AttrPred {
        AttrPred { key: key.into(), op, val, ci: false, re: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrOp { Eq, Has, Substr, Prefix, Suffix, Regex }

impl Selector {
    pub fn parse(input: &str) -> Result<Vec<Selector>, String> {
        let mut out = Vec::new();
        for part in split_top_level(input, ',') {
            let trimmed = part.trim();
            if trimmed.is_empty() { continue; }
            out.push(parse_one(trimmed)?);
        }
        if out.is_empty() { return Err("empty selector".into()); }
        Ok(out)
    }

    pub fn matches(&self, node: &Value) -> bool {
        // class
        let cls = get_str(node, "cls").unwrap_or("");
        match &self.class {
            ClassMatch::Any => {}
            ClassMatch::Exact(s) => if cls != s { return false; },
            ClassMatch::Simple(s) => {
                let simple = cls.rsplit('.').next().unwrap_or(cls);
                if simple != s { return false; }
            }
        }
        // attributes
        for p in &self.attrs {
            if !p.holds(get_str(node, &p.key).unwrap_or("")) { return false; }
        }
        // flags (single-letter codes from Traverse.java's encoded string)
        if !self.flags.is_empty() {
            let f = get_str(node, "flags").unwrap_or("");
            for c in &self.flags {
                if !f.contains(*c) { return false; }
            }
        }
        true
    }
}

impl AttrPred {
    fn holds(&self, v: &str) -> bool {
        if let Some(re) = &self.re { return re.is_match(v); }
        if self.op == AttrOp::Has { return !v.is_empty(); }
        let (v, want) = if self.ci { (v.to_lowercase(), self.val.to_lowercase()) } else { (v.to_string(), self.val.clone()) };
        match self.op {
            AttrOp::Eq if self.key == "rid" => rid_eq(&v, &want),
            AttrOp::Eq     => v == want,
            AttrOp::Substr => v.contains(&want),
            AttrOp::Prefix => v.starts_with(&want),
            AttrOp::Suffix => v.ends_with(&want),
            AttrOp::Has | AttrOp::Regex => unreachable!(),
        }
    }
}

fn parse_one(s: &str) -> Result<Selector, String> {
    let mut i = 0;
    let bytes = s.as_bytes();
    // 1. Optional tag prefix: ident (.ident)* or '*'
    let (class, advance) = parse_class(s, i)?;
    i += advance;
    let mut attrs = Vec::new();
    let mut flags = Vec::new();
    let mut relations: Vec<Relation> = Vec::new();

    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '[' {
            let (pred, adv) = parse_attr(&s[i..])?;
            attrs.push(pred);
            i += adv;
        } else if c == '#' {
            let len = s[i + 1..].find(|c: char| !is_id_char(c)).unwrap_or(s.len() - i - 1);
            if len == 0 {
                return Err(format!("expected a resource-id after '#' at position {i}"));
            }
            attrs.push(AttrPred::new("rid", AttrOp::Eq, s[i + 1..i + 1 + len].into()));
            i += 1 + len;
        } else if c == ':' {
            let (psd, adv) = parse_pseudo(&s[i..])?;
            match psd {
                Pseudo::Flag(c) => flags.push(c),
                Pseudo::Rel(r)  => relations.push(r),
                Pseudo::Attr(p) => attrs.push(p),
            }
            i += adv;
        } else if c.is_whitespace() {
            i += 1;
        } else {
            return Err(format!("unexpected '{c}' at position {i} in selector"));
        }
    }
    Ok(Selector { class, attrs, flags, relations })
}

fn parse_class(s: &str, start: usize) -> Result<(ClassMatch, usize), String> {
    let bytes = s.as_bytes();
    let mut i = start;
    if i >= bytes.len() { return Ok((ClassMatch::Any, 0)); }
    if matches!(bytes[i], b'[' | b':' | b'#') { return Ok((ClassMatch::Any, 0)); }
    if bytes[i] == b'*' { return Ok((ClassMatch::Any, 1)); }
    // ident chars: [A-Za-z0-9_.$]
    let begin = i;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'$' { i += 1; }
        else { break; }
    }
    if i == begin {
        return Err(format!("expected class name or '*' at position {start}"));
    }
    let name = &s[begin..i];
    let cls = if name.contains('.') {
        ClassMatch::Exact(name.to_string())
    } else {
        ClassMatch::Simple(name.to_string())
    };
    Ok((cls, i - start))
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '$')
}

/// A resource-id given in full (`com.app:id/email`) or by its local part (`email`).
fn rid_eq(rid: &str, want: &str) -> bool {
    rid == want || (!want.contains('/') && rid.rsplit_once('/').is_some_and(|(_, local)| local == want))
}

fn parse_attr(s: &str) -> Result<(AttrPred, usize), String> {
    // s starts with '['. Scan by char so quoted values may hold `]`, `=`, …
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let at = |k: usize| chars.get(k).map(|&(_, c)| c);
    let byte = |k: usize| chars.get(k).map_or(s.len(), |&(b, _)| b);
    let mut k = 1;
    while at(k).is_some_and(char::is_whitespace) { k += 1; }
    let key_start = k;
    while at(k).is_some_and(|c| !matches!(c, '=' | '~' | '^' | '$' | ']') && !c.is_whitespace()) { k += 1; }
    let key = normalise_key(&s[byte(key_start)..byte(k)])?;
    while at(k).is_some_and(char::is_whitespace) { k += 1; }
    let op = match (at(k), at(k + 1)) {
        (Some(']'), _) => return Ok((AttrPred::new(&key, AttrOp::Has, String::new()), byte(k + 1))),
        (Some('='), _) => { k += 1; AttrOp::Eq }
        (Some('~'), Some('=')) => { k += 2; AttrOp::Substr }
        (Some('^'), Some('=')) => { k += 2; AttrOp::Prefix }
        (Some('$'), Some('=')) => { k += 2; AttrOp::Suffix }
        (None, _) => return Err("unterminated [attribute]".into()),
        (Some(c), _) => return Err(format!("expected =, ~=, ^=, $= or ] in [{key}…], got '{c}'")),
    };
    while at(k).is_some_and(char::is_whitespace) { k += 1; }
    let mut pred = AttrPred::new(&key, op, String::new());
    match at(k) {
        Some(q @ ('"' | '\'')) => {
            let (val, end) = read_quoted(s, byte(k), q)?;
            pred.val = val;
            k = chars.iter().position(|&(b, _)| b >= end).unwrap_or(chars.len());
            while at(k).is_some_and(char::is_whitespace) { k += 1; }
            if at(k) == Some('i') { pred.ci = true; k += 1; }
        }
        Some('/') => {
            if op != AttrOp::Eq { return Err(format!("a /regex/ goes with = in [{key}…]")); }
            let (pat, end) = read_quoted(s, byte(k), '/')?;
            k = chars.iter().position(|&(b, _)| b >= end).unwrap_or(chars.len());
            let mut ci = false;
            while let Some(f) = at(k).filter(char::is_ascii_alphabetic) {
                if f != 'i' { return Err(format!("unknown regex flag '{f}' (only i)")); }
                ci = true;
                k += 1;
            }
            pred.op = AttrOp::Regex;
            pred.re = Some(crate::regex::Regex::new(&pat, ci).map_err(|e| format!("in [{key}=/{pat}/]: {e}"))?);
            pred.val = pat;
        }
        _ => {
            // Unquoted: everything up to the `]`, trimmed (spaces allowed).
            let begin = k;
            while at(k).is_some_and(|c| c != ']') { k += 1; }
            pred.val = s[byte(begin)..byte(k)].trim().to_string();
        }
    }
    while at(k).is_some_and(char::is_whitespace) { k += 1; }
    if at(k) != Some(']') { return Err(format!("expected ] to close [{key}…]")); }
    Ok((pred, byte(k + 1)))
}

/// `s[start]` is the quote `q`: the unescaped text up to the closing `q`,
/// and the byte index just past it. `\q` and `\\` are escapes; for a /regex/
/// every other `\x` is kept as is (the regex reads it).
fn read_quoted(s: &str, start: usize, q: char) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut it = s[start + q.len_utf8()..].char_indices();
    while let Some((i, c)) = it.next() {
        if c == q { return Ok((out, start + q.len_utf8() + i + c.len_utf8())); }
        if c == '\\' {
            match it.next() {
                Some((_, e)) if e == q || (e == '\\' && q != '/') => out.push(e),
                Some((_, e)) => { out.push('\\'); out.push(e); }
                None => break,
            }
        } else {
            out.push(c);
        }
    }
    Err(format!("unterminated {q}…{q}"))
}

/// The attributes a dump node has, and the CSS-ish names for them.
const KNOWN_KEYS: &[&str] = &["text", "desc", "hint", "id", "resource-id", "class", "package", "role", "state", "content-desc"];

/// CSS-like attribute names → JSON field names used by the daemon.
fn normalise_key(k: &str) -> Result<String, String> {
    Ok(match k {
        "class" | "cls"            => "cls",
        "package" | "package-name" | "pkg" => "pkg",
        "id" | "resource-id" | "rid" => "rid",
        "content-desc" | "desc"    => "desc",
        "hint"                     => "hint",
        "text"                     => "text",
        "role"                     => "role",
        "state"                    => "state",
        other => return Err(format!("unknown attribute [{other}] — use one of: {}", KNOWN_KEYS.join(", "))),
    }.to_string())
}

enum Pseudo {
    Flag(char),
    Rel(Relation),
    /// `:has-text("foo")` / `:text-is("foo")` — Playwright-style sugar that
    /// compiles to an `[text~=foo]` / `[text=foo]` attribute predicate so
    /// the matcher path stays unchanged.
    Attr(AttrPred),
}

/// Split `input` on `delim` only at paren-depth 0 / bracket-depth 0 / not
/// inside quotes. Keeps `,` available as the OR separator for top-level
/// selectors while letting `:near(EditText[a="x,y"], 60)` keep its commas.
fn split_top_level(input: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut in_dq = false;
    let mut in_sq = false;
    let mut escaped = false;
    for c in input.chars() {
        if in_dq || in_sq {
            buf.push(c);
            if escaped { escaped = false; }
            else if c == '\\' { escaped = true; }
            else if (in_dq && c == '"') || (in_sq && c == '\'') { in_dq = false; in_sq = false; }
            continue;
        }
        match c {
            '"'  => { in_dq = true; buf.push(c); }
            '\'' => { in_sq = true; buf.push(c); }
            '('  => { paren += 1; buf.push(c); }
            ')'  => { paren -= 1; buf.push(c); }
            '['  => { bracket += 1; buf.push(c); }
            ']'  => { bracket -= 1; buf.push(c); }
            ch if ch == delim && paren == 0 && bracket == 0 => {
                out.push(std::mem::take(&mut buf));
            }
            ch => buf.push(ch),
        }
    }
    out.push(buf);
    out
}

fn parse_pseudo(s: &str) -> Result<(Pseudo, usize), String> {
    let bytes = s.as_bytes();
    assert_eq!(bytes[0], b':');
    let mut i = 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() || c == b'-' { i += 1; } else { break; }
    }
    let name = &s[1..i];
    // Relational pseudo-classes take a `(...)` argument. `:has-text` and
    // `:text-is` also take an argument but compile to attribute predicates,
    // not relations — they're Playwright-flavoured sugar over `[text~=…]`
    // and `[text=…]` so users coming from web automation can keep their
    // muscle memory.
    if bytes.get(i) == Some(&b'(') {
        let close = find_matching_paren(s, i)
            .ok_or_else(|| format!("unterminated :{name}( argument"))?;
        let inner = &s[i + 1..close];
        match name {
            "has-text" => {
                let val = unquote(inner)?;
                if val.is_empty() {
                    return Err(":has-text needs (\"TEXT\")".into());
                }
                return Ok((Pseudo::Attr(AttrPred::new("text", AttrOp::Substr, val)), close + 1));
            }
            "text-is" => {
                let val = unquote(inner)?;
                if val.is_empty() {
                    return Err(":text-is needs (\"TEXT\")".into());
                }
                return Ok((Pseudo::Attr(AttrPred::new("text", AttrOp::Eq, val)), close + 1));
            }
            "text-matches" => {
                // :text-matches("re") or :text-matches("re", "i")
                let parts = split_top_level(inner, ',');
                let pat = unquote(parts.first().map_or("", |p| p.as_str()))?;
                let flags = parts.get(1).map(|f| unquote(f)).transpose()?.unwrap_or_default();
                if pat.is_empty() || parts.len() > 2 {
                    return Err(":text-matches needs (\"REGEX\") or (\"REGEX\", \"i\")".into());
                }
                if let Some(f) = flags.chars().find(|&f| f != 'i') {
                    return Err(format!(":text-matches: unknown flag '{f}' (only i)"));
                }
                let re = crate::regex::Regex::new(&pat, flags.contains('i')).map_err(|e| format!(":text-matches: {e}"))?;
                let mut p = AttrPred::new("text", AttrOp::Regex, pat);
                p.re = Some(re);
                return Ok((Pseudo::Attr(p), close + 1));
            }
            _ => {}
        }
        let rel = match name {
            "in"       => Relation::In(Selector::parse(inner)?),
            "has"      => Relation::Has(Selector::parse(inner)?),
            "below"    => Relation::Below(Selector::parse(inner)?),
            "right-of" => Relation::RightOf(Selector::parse(inner)?),
            "near" => {
                // `:near(SEL, PX)` — split on the last *top-level* comma so
                // SEL can itself contain commas (OR groups, attribute
                // values) without ambiguity.
                let parts = split_top_level(inner, ',');
                if parts.len() < 2 {
                    return Err(":near needs (SELECTOR, PX)".into());
                }
                let px_src = parts.last().unwrap().trim();
                let px: i64 = px_src.parse()
                    .map_err(|_| format!(":near distance must be an int, got '{px_src}'"))?;
                let sel_src = parts[..parts.len() - 1].join(",");
                Relation::Near(Selector::parse(&sel_src)?, px)
            }
            other => return Err(format!("unknown relational pseudo-class :{other}(…)")),
        };
        return Ok((Pseudo::Rel(rel), close + 1));
    }
    let flag = match name {
        "clickable" => 'c',
        "long-clickable" => 'L',
        "scrollable" => 's',
        "checkable" => 'k',
        "checked" => 'K',
        "focusable" => 'f',
        "focused" => 'F',
        "enabled" => 'e',
        "selected" => 'S',
        "password" => 'p',
        "visible" => 'v',
        "editable" => 'E',
        "heading" => 'H',
        other => return Err(format!("unknown pseudo-class :{other}")),
    };
    Ok((Pseudo::Flag(flag), i))
}

/// `s[start]` is `(`: the index of its matching `)`, skipping quoted text
/// (so `:has-text(")")` and `:text-is("a(b")` work).
fn find_matching_paren(s: &str, start: usize) -> Option<usize> {
    debug_assert_eq!(s.as_bytes()[start], b'(');
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s[start..].char_indices() {
        if let Some(q) = quote {
            if escaped { escaped = false; } else if c == '\\' { escaped = true; } else if c == q { quote = None; }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => { depth -= 1; if depth == 0 { return Some(start + i); } }
            _ => {}
        }
    }
    None
}

/// A pseudo-class argument: quoted (with escapes) or bare.
fn unquote(s: &str) -> Result<String, String> {
    let s = s.trim();
    match s.chars().next() {
        Some(q @ ('"' | '\'')) => {
            let (val, end) = read_quoted(s, 0, q)?;
            if !s[end..].trim().is_empty() { return Err(format!("unexpected text after {q}…{q}: '{}'", &s[end..])); }
            Ok(val)
        }
        _ => Ok(s.to_string()),
    }
}

// ---------- tree walking ----------

pub fn get_str<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == key {
                if let Value::Str(s) = v { return Some(s); }
            }
        }
    }
    None
}

pub fn children(node: &Value) -> Option<&Vec<Value>> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == "children" {
                if let Value::Arr(a) = v { return Some(a); }
            }
        }
    }
    None
}

pub fn bounds(node: &Value) -> Option<(i64, i64, i64, i64)> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == "bounds" {
                if let Value::Arr(a) = v {
                    if a.len() == 4 {
                        let n = |i| if let Value::Num(n) = a[i] { Some(n) } else { None };
                        return Some((n(0)?, n(1)?, n(2)?, n(3)?));
                    }
                }
            }
        }
    }
    None
}

/// Re-usable context for selector evaluation — caches the per-window roots
/// so relational pseudo-classes (:in / :near / :below / :right-of) can do
/// quick ancestor/anchor lookups without re-walking the envelope each time.
pub struct MatchCtx<'a> {
    pub roots: Vec<&'a Value>,
    /// Parent map keyed by node pointer-identity (raw `*const Value`),
    /// computed lazily on first relational evaluation.
    parent: std::cell::OnceCell<std::collections::HashMap<usize, &'a Value>>,
    /// Anchors of each relational argument, found once per context instead
    /// of once per candidate node (keyed by the argument's address).
    anchors: std::cell::RefCell<std::collections::HashMap<usize, Vec<&'a Value>>>,
}

impl<'a> MatchCtx<'a> {
    pub fn new(dump: &'a Value) -> Self {
        Self::from_roots(crate::json::all_roots(dump))
    }

    /// Match over the given window roots only, in that order.
    pub fn from_roots(roots: Vec<&'a Value>) -> Self {
        Self { roots, parent: std::cell::OnceCell::new(), anchors: Default::default() }
    }

    /// Build (or fetch) the parent-pointer map needed by `:in()`.
    fn parents(&self) -> &std::collections::HashMap<usize, &'a Value> {
        self.parent.get_or_init(|| {
            let mut map = std::collections::HashMap::new();
            for root in &self.roots {
                build_parents(root, &mut map);
            }
            map
        })
    }

    /// Does `node` satisfy `sel`, including relational pseudo-classes?
    pub fn matches(&self, sel: &Selector, node: &'a Value) -> bool {
        if !sel.matches(node) { return false; }
        for rel in &sel.relations {
            if !self.relation_holds(rel, node) { return false; }
        }
        true
    }

    fn relation_holds(&self, rel: &Relation, node: &'a Value) -> bool {
        match rel {
            Relation::In(sels) => {
                let parents = self.parents();
                let mut cursor = node;
                while let Some(&p) = parents.get(&(cursor as *const Value as usize)) {
                    for s in sels { if self.matches(s, p) { return true; } }
                    cursor = p;
                }
                false
            }
            Relation::Has(sels) => {
                let mut stack: Vec<&'a Value> = children(node).map(|k| k.iter().collect()).unwrap_or_default();
                while let Some(d) = stack.pop() {
                    if sels.iter().any(|s| self.matches(s, d)) { return true; }
                    if let Some(k) = children(d) { stack.extend(k.iter()); }
                }
                false
            }
            Relation::Near(sels, px)    => self.has_anchor_within(sels, node, *px),
            Relation::Below(sels)       => self.has_anchor_directional(sels, node, Dir::Below),
            Relation::RightOf(sels)     => self.has_anchor_directional(sels, node, Dir::RightOf),
        }
    }

    /// The nodes `sels` matches, cached for this context.
    fn anchors_of(&self, sels: &[Selector]) -> Vec<&'a Value> {
        let key = sels.as_ptr() as usize;
        if let Some(v) = self.anchors.borrow().get(&key) { return v.clone(); }
        let v = find_all_with(self, sels);
        self.anchors.borrow_mut().insert(key, v.clone());
        v
    }

    /// How far `node` is from its nearest anchor under its first spatial
    /// relation; `None` when it has none.
    fn spatial_distance(&self, sel: &Selector, node: &'a Value) -> Option<i64> {
        let n = bounds(node)?;
        let (sels, dir) = sel.relations.iter().find_map(|r| match r {
            Relation::Below(s) => Some((s, Some(Dir::Below))),
            Relation::RightOf(s) => Some((s, Some(Dir::RightOf))),
            Relation::Near(s, _) => Some((s, None)),
            _ => None,
        })?;
        let gap = |lo: i64, hi: i64, alo: i64, ahi: i64| if hi < alo { alo - hi } else if ahi < lo { lo - ahi } else { 0 };
        self.anchors_of(sels).iter().filter_map(|a| bounds(a)).filter_map(|b| match dir {
            Some(Dir::Below) if n.1 >= b.3 => Some((n.1 - b.3) + gap(n.0, n.2, b.0, b.2)),
            Some(Dir::RightOf) if n.0 >= b.2 => Some((n.0 - b.2) + gap(n.1, n.3, b.1, b.3)),
            None => {
                let (dx, dy) = ((n.0 + n.2) / 2 - (b.0 + b.2) / 2, (n.1 + n.3) / 2 - (b.1 + b.3) / 2);
                Some(((dx * dx + dy * dy) as f64).sqrt() as i64)
            }
            _ => None,
        }).min()
    }

    fn has_anchor_within(&self, sels: &[Selector], node: &'a Value, px: i64) -> bool {
        let n = match bounds(node) { Some(b) => b, None => return false };
        let cx = (n.0 + n.2) / 2;
        let cy = (n.1 + n.3) / 2;
        let anchors = self.anchors_of(sels);
        for a in anchors {
            if let Some(b) = bounds(a) {
                let acx = (b.0 + b.2) / 2;
                let acy = (b.1 + b.3) / 2;
                let dx = cx - acx;
                let dy = cy - acy;
                if dx * dx + dy * dy <= px * px { return true; }
            }
        }
        false
    }

    fn has_anchor_directional(&self, sels: &[Selector], node: &'a Value, dir: Dir) -> bool {
        let n = match bounds(node) { Some(b) => b, None => return false };
        let anchors = self.anchors_of(sels);
        for a in anchors {
            if let Some(b) = bounds(a) {
                let ok = match dir {
                    Dir::Below   => n.1 >= b.3,   // node top ≥ anchor bottom
                    Dir::RightOf => n.0 >= b.2,   // node left ≥ anchor right
                };
                if ok { return true; }
            }
        }
        false
    }

}

#[derive(Copy, Clone)]
enum Dir { Below, RightOf }

fn build_parents<'a>(
    node: &'a Value,
    map: &mut std::collections::HashMap<usize, &'a Value>,
) {
    if let Some(kids) = children(node) {
        for c in kids {
            map.insert(c as *const Value as usize, node);
            build_parents(c, map);
        }
    }
}

/// Every node any of `selectors` matches, in tree order, with two
/// refinements: of nested matches of a `:has(…)` selector only the innermost
/// is kept, and a lone selector with `:below` / `:right-of` / `:near` returns
/// its matches nearest-first.
pub fn find_all_with<'a>(ctx: &MatchCtx<'a>, selectors: &[Selector]) -> Vec<&'a Value> {
    let mut out: Vec<(&'a Value, usize)> = Vec::new();
    for &root in &ctx.roots {
        let mut stack: Vec<&'a Value> = vec![root];
        while let Some(n) = stack.pop() {
            if let Some(i) = selectors.iter().position(|s| ctx.matches(s, n)) { out.push((n, i)); }
            if let Some(kids) = children(n) {
                for c in kids.iter().rev() { stack.push(c); }
            }
        }
    }
    let has = |i: usize| selectors[i].relations.iter().any(|r| matches!(r, Relation::Has(_)));
    if out.iter().any(|&(_, i)| has(i)) {
        // A match whose subtree holds another match of the same selector is an
        // outer container of it: drop it.
        let ids: std::collections::HashSet<(usize, usize)> = out.iter().map(|&(n, i)| (n as *const Value as usize, i)).collect();
        out.retain(|&(n, i)| !has(i) || !holds_match(n, i, &ids));
    }
    let mut nodes: Vec<&'a Value> = out.into_iter().map(|(n, _)| n).collect();
    if let [sel] = selectors {
        if sel.relations.iter().any(|r| matches!(r, Relation::Below(_) | Relation::RightOf(_) | Relation::Near(..))) {
            nodes.sort_by_key(|n| ctx.spatial_distance(sel, n).unwrap_or(i64::MAX));
        }
    }
    nodes
}

fn holds_match(node: &Value, sel: usize, ids: &std::collections::HashSet<(usize, usize)>) -> bool {
    let mut stack: Vec<&Value> = children(node).map(|k| k.iter().collect()).unwrap_or_default();
    while let Some(d) = stack.pop() {
        if ids.contains(&(d as *const Value as usize, sel)) { return true; }
        if let Some(k) = children(d) { stack.extend(k.iter()); }
    }
    false
}

/// What a match must be, beyond the selector (`--visible` / `--clickable`
/// / `--enabled`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Require {
    pub visible: bool,
    pub clickable: bool,
    pub enabled: bool,
}

/// Apply [`Require`] filters in-place on a match list. Cheap and lossless:
/// callers can chain it after `find_all`.
pub fn apply_filters(matches: &mut Vec<&Value>, f: Require) {
    if !f.visible && !f.clickable && !f.enabled { return; }
    matches.retain(|n| {
        let flags = get_str(n, "flags").unwrap_or("");
        if f.clickable && !flags.contains('c') { return false; }
        if f.enabled   && !flags.contains('e') { return false; }
        if f.visible {
            // `:visible` here means "has bounds and at least one of the
            // a11y visibility flags". The daemon emits 'v' for nodes that
            // pass AccessibilityNodeInfo.isVisibleToUser().
            if !flags.contains('v') { return false; }
            match bounds(n) {
                Some((x1, y1, x2, y2)) if x2 > x1 && y2 > y1 => {}
                _ => return false,
            }
        }
        true
    });
}

/// A daemon-grammar selector that names `node`: the full resource id when
/// it has one (always unique), else class plus the node's own text.
pub fn node_to_daemon_selector(node: &Value) -> String {
    let rid = get_str(node, "rid").unwrap_or("");
    if !rid.is_empty() {
        return format!("id={rid}");
    }
    let cls_full = get_str(node, "cls").unwrap_or("EditText");
    let simple = cls_full.rsplit('.').next().unwrap_or(cls_full);
    let txt = get_str(node, "text").unwrap_or("");
    if !txt.is_empty() {
        return format!("class={simple} text={txt:?}");
    }
    format!("class={simple}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_id_matches_short_and_full_resource_ids() {
        let node = crate::json::parse(r#"{"cls":"android.widget.EditText","rid":"com.app:id/email"}"#).unwrap();
        for sel in ["#email", "EditText#email", "#com.app:id/email", "[id=email]", "[resource-id=com.app:id/email]"] {
            assert!(Selector::parse(sel).unwrap()[0].matches(&node), "{sel}");
        }
        for sel in ["#mail", "Button#email", "[id=other:id/email]"] {
            assert!(!Selector::parse(sel).unwrap()[0].matches(&node), "{sel}");
        }
        assert!(Selector::parse("#").is_err());
    }

    #[test]
    fn parses_has_text_to_substring_attr() {
        let sels = Selector::parse("Button:has-text(\"Sign in\")").unwrap();
        assert_eq!(sels.len(), 1);
        let s = &sels[0];
        match &s.class {
            ClassMatch::Simple(n) => assert_eq!(n, "Button"),
            other => panic!("expected Simple(\"Button\"), got {other:?}"),
        }
        assert_eq!(s.attrs.len(), 1);
        assert_eq!(s.attrs[0].key, "text");
        assert_eq!(s.attrs[0].op, AttrOp::Substr);
        assert_eq!(s.attrs[0].val, "Sign in");
    }

    #[test]
    fn parses_text_is_to_eq_attr() {
        let sels = Selector::parse("Button:text-is(\"OK\")").unwrap();
        let s = &sels[0];
        assert_eq!(s.attrs.len(), 1);
        assert_eq!(s.attrs[0].op, AttrOp::Eq);
        assert_eq!(s.attrs[0].val, "OK");
    }

    #[test]
    fn rejects_empty_has_text_argument() {
        let err = Selector::parse("Button:has-text(\"\")").unwrap_err();
        assert!(err.contains("has-text"), "got {err}");
    }

    fn node(json: &str) -> Value { crate::json::parse(json).unwrap() }

    fn texts<'a>(root: &'a Value, sel: &str) -> Vec<&'a str> {
        let ctx = MatchCtx::from_roots(vec![root]);
        find_all_with(&ctx, &Selector::parse(sel).unwrap()).into_iter().map(|n| get_str(n, "text").unwrap_or("")).collect()
    }

    fn one(sel: &str, json: &str) -> bool { Selector::parse(sel).unwrap()[0].matches(&node(json)) }

    #[test]
    fn quoted_values_may_hold_brackets_and_operator_characters() {
        assert!(one(r#"[text="a]b"]"#, r#"{"text":"a]b"}"#));
        assert!(one(r#"[text="1^=2"]"#, r#"{"text":"1^=2"}"#));
        assert!(!one(r#"[text="1^=2"]"#, r#"{"text":"1^=2x"}"#), "= is exact, not prefix");
        assert!(one(r#"[desc='x~=y']"#, r#"{"desc":"x~=y"}"#));
        assert!(one(r#"[text="say \"hi\""]"#, r#"{"text":"say \"hi\""}"#));
        assert!(one("[text=Hello World]", r#"{"text":"Hello World"}"#), "unquoted values with spaces still work");
    }

    #[test]
    fn pseudo_arguments_may_hold_parentheses() {
        assert!(one(r#":has-text("설정 (고급)")"#, r#"{"text":"설정 (고급) 메뉴"}"#));
        assert!(one(r#":has-text(")")"#, r#"{"text":"a)"}"#));
        assert!(one(r#":text-is("(")"#, r#"{"text":"("}"#));
    }

    #[test]
    fn an_unknown_attribute_is_an_error_not_a_silent_miss() {
        let e = Selector::parse("[txt=OK]").unwrap_err();
        assert!(e.contains("unknown attribute") && e.contains("text"), "{e}");
        for ok in ["[text=a]", "[desc=a]", "[hint=a]", "[id=a]", "[resource-id=a]", "[class=a]", "[package=a]", "[role=a]", "[state=a]"] {
            assert!(Selector::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn the_i_flag_matches_case_insensitively() {
        assert!(one(r#"[text="sign in" i]"#, r#"{"text":"Sign In"}"#));
        assert!(one(r#"[text~="SIGN" i]"#, r#"{"text":"please sign in"}"#));
        assert!(!one(r#"[text="sign in"]"#, r#"{"text":"Sign In"}"#));
    }

    #[test]
    fn regex_values_and_text_matches() {
        assert!(one(r"[text=/sign\s*in/i]", r#"{"text":"Please SIGN  IN"}"#));
        assert!(one(r"[text=/^OK$/]", r#"{"text":"OK"}"#));
        assert!(!one(r"[text=/^OK$/]", r#"{"text":"OK!"}"#));
        assert!(one(r#":text-matches("log.?in", "i")"#, r#"{"text":"LOG-IN"}"#));
        assert!(one(r#"[desc=/\d{2,3} items?/]"#, r#"{"desc":"12 items"}"#));
        assert!(Selector::parse(r"[text=/(unclosed/]").is_err());
        assert!(Selector::parse(r"[text~=/x/]").is_err(), "a regex goes with =");
    }

    #[test]
    fn has_keeps_the_innermost_container() {
        let root = node(r#"{"cls":"android.widget.LinearLayout","text":"outer","children":[
            {"cls":"android.widget.LinearLayout","text":"row","children":[{"cls":"android.widget.TextView","text":"Wi-Fi"}]},
            {"cls":"android.widget.LinearLayout","text":"other","children":[{"cls":"android.widget.TextView","text":"Bluetooth"}]}]}"#);
        assert_eq!(texts(&root, "LinearLayout:has(TextView[text=Wi-Fi])"), ["row"]);
        assert!(texts(&root, "LinearLayout:has(TextView[text=NFC])").is_empty());
    }

    #[test]
    fn below_and_right_of_put_the_nearest_first() {
        // Tree order is far → near; the result must be near → far.
        let root = node(r#"{"cls":"android.widget.FrameLayout","bounds":[0,0,1000,2000],"children":[
            {"cls":"android.widget.EditText","text":"far","bounds":[0,800,1000,900]},
            {"cls":"android.widget.EditText","text":"near","bounds":[0,220,1000,300]},
            {"cls":"android.widget.TextView","text":"Email","bounds":[0,100,300,200]},
            {"cls":"android.widget.Button","text":"right-far","bounds":[900,100,1000,200]},
            {"cls":"android.widget.Button","text":"right-near","bounds":[350,100,500,200]}]}"#);
        assert_eq!(texts(&root, "EditText:below(TextView[text=Email])"), ["near", "far"]);
        assert_eq!(texts(&root, "Button:right-of(TextView[text=Email])"), ["right-near", "right-far"]);
        assert_eq!(texts(&root, "Button:near(TextView[text=Email], 2000)"), ["right-near", "right-far"]);
    }

    #[test]
    fn keeps_relational_pseudo_classes_intact() {
        // Ensure adding :has-text / :text-is didn't break the relational path.
        let sels = Selector::parse("EditText:below(TextView[text=Email])").unwrap();
        let s = &sels[0];
        assert_eq!(s.relations.len(), 1);
        match &s.relations[0] {
            Relation::Below(_) => {}
            other => panic!("expected Below(..), got {other:?}"),
        }
    }
}

