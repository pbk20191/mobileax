// Android Studio's Logcat query language for `mobax logs`, evaluated on the
// host over `logcat -v threadtime -v uid`.
//
//   package:com.example   package:mine   process:com.example:remote
//   tag:OkHttp   message:"timed out"   level:WARN   age:5m   is:crash
//   is:stacktrace   free text
//
// Each key also comes negated (`-tag:chatty`), as a regex (`tag~:^Ok`) and
// as an exact match (`tag=:OkHttp`); the plain form matches a substring,
// ignoring case. As in Studio, terms side by side combine by key: the same
// key ORs (`tag:A tag:B`), different keys and negations AND. `&`, `|` and
// parentheses group explicitly (`&` binds tighter). `age:` limits the whole
// query to that many seconds back (s, m, h, d); `name:` only names a filter
// in Studio and matches everything here.
//
// `package:` goes by the entry's uid, so it keeps matching across restarts
// and still has the lines of a process that has died; `mine` is the app in
// front when the query starts.

use std::collections::HashMap;

use mobax_core::regex::Regex;

#[derive(Debug)]
pub(crate) enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Term(Term),
}

#[derive(Debug)]
pub(crate) enum Term {
    /// Free text: tag, message, package or process.
    Text(String),
    Field { key: Key, m: Match },
    /// Minimum priority, as its logcat letter's rank.
    Level(u8),
    Crash,
    Stacktrace,
    /// Handled as a whole (`-T`); true for every entry.
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Key { Tag, Package, Process, Message }

#[derive(Debug)]
pub(crate) enum Match {
    Contains(String),
    Exact(String),
    Re(Regex),
    /// `package:mine`: the app in front, resolved when logs start.
    Mine,
}

/// A parsed query and what it needs from the device before it runs.
#[derive(Debug)]
pub(crate) struct Query {
    pub expr: Expr,
    /// The largest `age:`, in seconds.
    pub age_secs: Option<u64>,
    pub uses_mine: bool,
}

pub(crate) fn parse(src: &str) -> Result<Query, String> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks: &toks, i: 0, age: None, mine: false };
    let expr = if toks.is_empty() { Expr::And(Vec::new()) } else { p.or()? };
    if p.i < toks.len() {
        return Err(format!("logs query: unexpected {:?}", toks[p.i]));
    }
    Ok(Query { expr, age_secs: p.age, uses_mine: p.mine })
}

// ---------- tokens ----------

#[derive(Debug, PartialEq)]
enum Tok {
    Open,
    Close,
    And,
    Or,
    /// `[-]key[~|=]:value`, or a bare word (`key` empty).
    Word { neg: bool, key: String, op: char, value: String },
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut depth = 0usize;
    while i < c.len() {
        match c[i] {
            ' ' | '\t' | '\n' => i += 1,
            '(' => { out.push(Tok::Open); depth += 1; i += 1; }
            ')' if depth > 0 => { out.push(Tok::Close); depth -= 1; i += 1; }
            '&' => { out.push(Tok::And); i += 1; }
            '|' => { out.push(Tok::Or); i += 1; }
            _ => {
                let start = i;
                let neg = c[i] == '-' && c.get(i + 1).is_some_and(|ch| ch.is_ascii_alphabetic());
                if neg { i += 1; }
                let key_start = i;
                while i < c.len() && c[i].is_ascii_alphabetic() { i += 1; }
                let (key, op) = match (c.get(i), c.get(i + 1)) {
                    (Some(':'), _) if i > key_start => { i += 1; (c[key_start..i - 1].iter().collect(), ':') }
                    (Some(&o @ ('~' | '=')), Some(':')) if i > key_start => { i += 2; (c[key_start..i - 2].iter().collect(), o) }
                    _ => { i = start; (String::new(), ':') }
                };
                let neg = neg && !key.is_empty();
                let value = word(&c, &mut i, depth)?;
                out.push(Tok::Word { neg, key, op, value });
            }
        }
    }
    Ok(out)
}

/// A value: `"quoted, with \" escapes"` or up to whitespace (or a `)` that
/// closes an open group).
fn word(c: &[char], i: &mut usize, depth: usize) -> Result<String, String> {
    let mut s = String::new();
    if c.get(*i) == Some(&'"') {
        *i += 1;
        while *i < c.len() && c[*i] != '"' {
            if c[*i] == '\\' && *i + 1 < c.len() { *i += 1; }
            s.push(c[*i]);
            *i += 1;
        }
        if *i == c.len() { return Err("logs query: unclosed quote".into()); }
        *i += 1;
        return Ok(s);
    }
    while *i < c.len() && !c[*i].is_whitespace() && !(c[*i] == ')' && depth > 0) {
        s.push(c[*i]);
        *i += 1;
    }
    Ok(s)
}

// ---------- grammar ----------

struct Parser<'a> {
    toks: &'a [Tok],
    i: usize,
    age: Option<u64>,
    mine: bool,
}

impl Parser<'_> {
    fn or(&mut self) -> Result<Expr, String> {
        let mut alts = vec![self.and()?];
        while self.toks.get(self.i) == Some(&Tok::Or) {
            self.i += 1;
            alts.push(self.and()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Expr::Or(alts) })
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut all = vec![self.seq()?];
        while self.toks.get(self.i) == Some(&Tok::And) {
            self.i += 1;
            all.push(self.seq()?);
        }
        Ok(if all.len() == 1 { all.pop().unwrap() } else { Expr::And(all) })
    }

    /// Terms side by side: the same key ORs, everything else ANDs.
    /// Consecutive bare words are one phrase, as typed in Studio.
    fn seq(&mut self) -> Result<Expr, String> {
        let mut by_key: Vec<(String, Vec<Expr>)> = Vec::new();
        let mut rest: Vec<Expr> = Vec::new();
        let mut phrase: Vec<String> = Vec::new();
        let flush = |phrase: &mut Vec<String>, rest: &mut Vec<Expr>| {
            if !phrase.is_empty() {
                rest.push(Expr::Term(Term::Text(phrase.join(" "))));
                phrase.clear();
            }
        };
        loop {
            match self.toks.get(self.i) {
                Some(Tok::Open) => {
                    flush(&mut phrase, &mut rest);
                    self.i += 1;
                    let e = self.or()?;
                    if self.toks.get(self.i) != Some(&Tok::Close) {
                        return Err("logs query: missing ')'".into());
                    }
                    self.i += 1;
                    rest.push(e);
                }
                Some(Tok::Word { neg, key, op, value }) => {
                    self.i += 1;
                    if key.is_empty() {
                        phrase.push(value.clone());
                        continue;
                    }
                    flush(&mut phrase, &mut rest);
                    let t = Expr::Term(self.term(key, *op, value)?);
                    if *neg {
                        rest.push(Expr::Not(Box::new(t)));
                    } else if let Some((_, v)) = by_key.iter_mut().find(|(k, _)| k == key) {
                        v.push(t);
                    } else {
                        by_key.push((key.clone(), vec![t]));
                    }
                }
                _ => break,
            }
        }
        flush(&mut phrase, &mut rest);
        let mut all: Vec<Expr> = by_key.into_iter()
            .map(|(_, mut v)| if v.len() == 1 { v.pop().unwrap() } else { Expr::Or(v) })
            .collect();
        all.extend(rest);
        match all.len() {
            0 => Err(format!("logs query: expected a term at {:?}", self.toks.get(self.i))),
            1 => Ok(all.pop().unwrap()),
            _ => Ok(Expr::And(all)),
        }
    }

    fn term(&mut self, key: &str, op: char, value: &str) -> Result<Term, String> {
        let field = |k: Key, p: &mut Self| -> Result<Term, String> {
            let m = match op {
                '~' => Match::Re(Regex::new(value, true).map_err(|e| format!("{key}~:{value}: {e}"))?),
                '=' => Match::Exact(value.to_string()),
                _ if k == Key::Package && value == "mine" => { p.mine = true; Match::Mine }
                _ => Match::Contains(value.to_lowercase()),
            };
            Ok(Term::Field { key: k, m })
        };
        match key {
            "tag" => field(Key::Tag, self),
            "package" => field(Key::Package, self),
            "process" => field(Key::Process, self),
            "message" => field(Key::Message, self),
            "level" => level_rank(value).map(Term::Level)
                .ok_or_else(|| format!("level:{value}: VERBOSE DEBUG INFO WARN ERROR ASSERT")),
            "is" => match value {
                "crash" => Ok(Term::Crash),
                "stacktrace" => Ok(Term::Stacktrace),
                _ => Err(format!("is:{value}: crash or stacktrace")),
            },
            "age" => {
                let secs = parse_age(value).ok_or_else(|| format!("age:{value}: e.g. 30s, 5m, 2h, 1d"))?;
                self.age = Some(self.age.map_or(secs, |a| a.max(secs)));
                Ok(Term::Always)
            }
            "name" => Ok(Term::Always),
            _ => Err(format!("logs query: unknown key {key}: (package tag process message level age is)")),
        }
    }
}

fn level_rank(v: &str) -> Option<u8> {
    Some(match v.to_ascii_uppercase().as_str() {
        "V" | "VERBOSE" => 0,
        "D" | "DEBUG" => 1,
        "I" | "INFO" => 2,
        "W" | "WARN" | "WARNING" => 3,
        "E" | "ERROR" => 4,
        "F" | "A" | "FATAL" | "ASSERT" => 5,
        _ => return None,
    })
}

fn parse_age(v: &str) -> Option<u64> {
    let (num, mult) = match v.chars().last()? {
        's' => (&v[..v.len() - 1], 1),
        'm' => (&v[..v.len() - 1], 60),
        'h' => (&v[..v.len() - 1], 3600),
        'd' => (&v[..v.len() - 1], 86400),
        _ => return None,
    };
    num.parse::<u64>().ok().filter(|&n| n > 0).map(|n| n * mult)
}

// ---------- entries ----------

/// One `threadtime` + `uid` line.
pub(crate) struct Entry<'a> {
    pub uid: &'a str,
    pub pid: u32,
    pub level: char,
    pub tag: &'a str,
    pub message: &'a str,
    /// The line as plain `threadtime` (the uid column cut out).
    pub plain: String,
}

pub(crate) fn parse_entry(line: &str) -> Option<Entry<'_>> {
    // MM-DD HH:MM:SS.mmm UID PID TID L TAG: message
    let mut fields = Fields { s: line, i: 0 };
    fields.next()?;
    let time_end = { fields.next()?; fields.i };
    let uid = fields.next()?;
    let uid_end = fields.i;
    let pid = fields.next()?.parse().ok()?;
    fields.next()?;
    let level = fields.next()?.chars().next()?;
    let rest = &line[fields.i..].trim_start();
    let (tag, message) = rest.split_once(": ").unwrap_or((rest, ""));
    Some(Entry {
        uid,
        pid,
        level,
        tag: tag.trim_end(),
        message,
        plain: format!("{}{}", &line[..time_end], &line[uid_end..]),
    })
}

struct Fields<'a> { s: &'a str, i: usize }

impl<'a> Fields<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let rest = &self.s[self.i..];
        let start = self.i + (rest.len() - rest.trim_start().len());
        let len = self.s[start..].find(' ').unwrap_or(self.s.len() - start);
        if len == 0 { return None; }
        self.i = start + len;
        Some(&self.s[start..start + len])
    }
}

// ---------- matching ----------

/// What a query needs to know about the device's apps.
#[derive(Default)]
pub(crate) struct Device {
    /// App id (uid % 100000) → its packages, from `pm list packages -U`.
    pub packages: HashMap<u32, Vec<String>>,
    /// pid → process name, from `ps` when logs start.
    pub processes: HashMap<u32, String>,
    /// `package:mine`.
    pub mine: Option<String>,
}

impl Device {
    /// `package:a uid:10123` lines (shared uids list several packages).
    pub fn read_packages(&mut self, pm: &str) {
        for l in pm.lines() {
            let mut pkg = None;
            let mut uid = None;
            for tok in l.split_whitespace() {
                if let Some(p) = tok.strip_prefix("package:") { pkg = Some(p); }
                if let Some(u) = tok.strip_prefix("uid:") { uid = u.split(',').next().and_then(|u| u.parse::<u32>().ok()); }
            }
            if let (Some(p), Some(u)) = (pkg, uid) {
                self.packages.entry(u % 100_000).or_default().push(p.to_string());
            }
        }
    }

    /// `ps -A -o PID,NAME` output.
    pub fn read_processes(&mut self, ps: &str) {
        for l in ps.lines().skip(1) {
            if let Some((pid, name)) = l.trim().split_once(char::is_whitespace) {
                if let Ok(pid) = pid.parse() {
                    self.processes.insert(pid, name.trim().to_string());
                }
            }
        }
    }

    fn packages_of(&self, uid: &str) -> &[String] {
        uid.parse::<u32>().ok()
            .and_then(|u| self.packages.get(&(u % 100_000)))
            .map_or(&[], Vec::as_slice)
    }

    /// A process started after logs began is named after its app.
    fn process_of<'a>(&'a self, e: &Entry) -> Option<&'a str> {
        self.processes.get(&e.pid).map(String::as_str)
            .or_else(|| self.packages_of(e.uid).first().map(String::as_str))
    }
}

impl Query {
    pub fn matches(&self, e: &Entry, d: &Device) -> bool {
        eval(&self.expr, e, d)
    }
}

fn eval(x: &Expr, e: &Entry, d: &Device) -> bool {
    match x {
        Expr::And(v) => v.iter().all(|x| eval(x, e, d)),
        Expr::Or(v) => v.iter().any(|x| eval(x, e, d)),
        Expr::Not(x) => !eval(x, e, d),
        Expr::Term(t) => term(t, e, d),
    }
}

fn term(t: &Term, e: &Entry, d: &Device) -> bool {
    match t {
        Term::Always => true,
        Term::Level(min) => level_rank(&e.level.to_string()).is_some_and(|r| r >= *min),
        Term::Crash => (e.tag == "AndroidRuntime" && e.level == 'E')
            || (e.level == 'F' && matches!(e.tag, "DEBUG" | "libc" | "crash_dump")),
        Term::Stacktrace => {
            let m = e.message.trim_start();
            m.starts_with("at ") || m.starts_with("Caused by: ") || m.starts_with("... ")
        }
        Term::Text(s) => {
            let s = s.to_lowercase();
            [e.tag, e.message].iter().any(|v| v.to_lowercase().contains(&s))
                || d.packages_of(e.uid).iter().any(|p| p.to_lowercase().contains(&s))
                || d.process_of(e).is_some_and(|p| p.to_lowercase().contains(&s))
        }
        Term::Field { key, m } => match key {
            Key::Tag => field(m, e.tag, d),
            Key::Message => field(m, e.message, d),
            Key::Process => d.process_of(e).is_some_and(|p| field(m, p, d)),
            Key::Package => d.packages_of(e.uid).iter().any(|p| field(m, p, d)),
        },
    }
}

fn field(m: &Match, v: &str, d: &Device) -> bool {
    match m {
        Match::Contains(s) => v.to_lowercase().contains(s.as_str()),
        Match::Exact(s) => v == s,
        Match::Re(r) => r.is_match(v),
        Match::Mine => d.mine.as_deref() == Some(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> Device {
        let mut d = Device::default();
        d.read_packages("package:com.example.chat uid:10676\npackage:com.android.settings uid:1000\n");
        d.read_processes("  PID NAME\n13082 com.example.chat\n 900 system_server\n");
        d.mine = Some("com.example.chat".into());
        d
    }

    const APP: &str = "11-05 09:43:45.677 10676 13082 13082 I VRI[ChannelActivity]@eb8f35c: call setFrameRateCategory";
    const CRASH: &str = "11-05 09:43:46.000 10676 14000 14000 E AndroidRuntime: FATAL EXCEPTION: main";
    const SYS: &str = "11-05 09:43:47.000  1000   900  1200 W ActivityManager: Slow operation";

    fn hit(q: &str, line: &str) -> bool {
        parse(q).unwrap().matches(&parse_entry(line).unwrap(), &dev())
    }

    #[test]
    fn entries_drop_the_uid_column() {
        let e = parse_entry(APP).unwrap();
        assert_eq!((e.uid, e.pid, e.level, e.tag), ("10676", 13082, 'I', "VRI[ChannelActivity]@eb8f35c"));
        assert_eq!(e.plain, "11-05 09:43:45.677 13082 13082 I VRI[ChannelActivity]@eb8f35c: call setFrameRateCategory");
        let s = parse_entry("11-05 09:53:43.343 shell  2933  2933 I adbd    : requested").unwrap();
        assert_eq!((s.tag, s.message, s.plain.as_str()), ("adbd", "requested", "11-05 09:53:43.343  2933  2933 I adbd    : requested"));
    }

    #[test]
    fn package_goes_by_uid_across_restarts() {
        assert!(hit("package:com.example.chat", APP));
        // pid 14000 started after `ps`: still the app's, by uid.
        assert!(hit("package:example", CRASH));
        assert!(hit("package:mine", CRASH));
        assert!(!hit("package:mine", SYS));
        assert!(hit("package=:com.android.settings", SYS));
        assert!(!hit("package=:com.android", SYS));
        assert!(hit("process:system_server", SYS));
        assert!(hit("process:example", CRASH));
    }

    #[test]
    fn keys_combine_like_studio() {
        assert!(hit("tag:ActivityManager tag:AndroidRuntime", SYS));
        assert!(!hit("tag:ActivityManager level:ERROR", SYS));
        assert!(hit("level:WARN", SYS) && !hit("level:W", APP));
        assert!(!hit("package:example -tag:VRI", APP));
        assert!(hit("tag~:^vri\\[ message:\"call set\"", APP));
        assert!(hit("(tag:Nope | message:slow) & level:W", SYS));
        assert!(!hit("tag:Nope | message:slow & level:E", SYS));
        assert!(hit("is:crash", CRASH) && !hit("is:crash", SYS));
        assert!(hit("frameratecategory", APP) && hit("call setFrame", APP));
        assert!(hit("name:mine age:5m package:mine", APP));
    }

    #[test]
    fn age_and_errors() {
        assert_eq!(parse("age:30s age:5m").unwrap().age_secs, Some(300));
        assert!(parse("age:1d").unwrap().age_secs == Some(86400));
        assert!(parse("level:LOUD").is_err());
        assert!(parse("colour:red").is_err());
        assert!(parse("message:\"open").is_err());
        assert!(parse("(tag:a").is_err());
        assert!(parse("tag~:(").is_err());
    }
}
