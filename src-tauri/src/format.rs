//! Deterministic transcript cleanup and classification.
//!
//! - `looks_non_english` — is this romanized Bangla / another language?
//! - `looks_like_code`   — is this a shell command / code / path?
//! - `spoken_symbols`    — turn "dash dash force" into "--force", etc. (code only)
//! - `tidy`              — whitespace + (for prose) casing / fillers / end mark.

// ===========================================================================
// Classification
// ===========================================================================

/// Common English function words. English prose hits many of these; romanized
/// Bangla ("ami valo achi kemon acho") hits almost none.
const ENGLISH_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "is", "are", "was", "were", "be", "been",
    "to", "of", "in", "on", "at", "for", "with", "as", "by", "that", "this", "it",
    "i", "you", "he", "she", "we", "they", "have", "has", "had", "do", "does", "did",
    "not", "no", "yes", "can", "will", "would", "should", "could", "my", "your",
    "so", "if", "then", "there", "here", "what", "when", "where", "how", "why",
    "am", "me", "us", "our", "his", "her", "their", "from", "up", "out", "about",
];

/// Very common romanized-Bangla tokens. Their presence is a strong positive
/// signal — English prose almost never contains them.
const BANGLISH_MARKERS: &[&str] = &[
    "ami", "ami", "tumi", "apni", "tui", "amar", "tomar", "apnar", "ache", "achi",
    "acho", "achen", "chilo", "chilam", "korbo", "korba", "korte", "kora", "korchi",
    "hoy", "hobe", "hoye", "hoyeche", "na", "nai", "ki", "ki", "keno", "kemon",
    "kothay", "kobe", "kolke", "kalke", "ajke", "ekhon", "akhon", "porshu", "gele",
    "jabo", "jabe", "jai", "asi", "eshe", "bhai", "bhalo", "valo", "kharap", "onek",
    "khub", "aro", "abar", "ekta", "duita", "jeta", "eita", "oita", "eirokom",
    "mone", "hocche", "hoise", "dekho", "dekhi", "bolo", "bola", "bolba", "bollo",
    "ar", "ba", "kintu", "tobe", "jodi", "tahole", "karon", "jonno", "diye", "theke",
    "er", "ta", "te", "ke", "o", "je", "ei", "oi", "boss",
];

/// Heuristic: does this transcript look like it is NOT plain English?
/// True on any non-Latin letter; OR when it has several romanized-Bangla marker
/// words AND a low English-stopword ratio.
pub fn looks_non_english(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t.chars().any(|c| c.is_alphabetic() && !c.is_ascii_alphabetic()) {
        return true;
    }
    let words = word_tokens(t);
    if words.len() < 3 {
        return false; // too short to judge — treat as English
    }

    let stop_hits = words
        .iter()
        .filter(|w| ENGLISH_STOPWORDS.contains(&w.as_str()))
        .count();
    let marker_hits = words
        .iter()
        .filter(|w| BANGLISH_MARKERS.contains(&w.as_str()))
        .count();

    let stop_ratio = stop_hits as f32 / words.len() as f32;
    let marker_ratio = marker_hits as f32 / words.len() as f32;

    // Confident banglish: multiple markers OR a high marker density, plus not
    // clearly English (few stopwords).
    (marker_hits >= 2 || marker_ratio >= 0.2) && stop_ratio < 0.25
}

/// Command names that, appearing as the first word, strongly imply a shell line.
const COMMAND_HEADS: &[&str] = &[
    "git", "sudo", "cd", "ls", "cat", "grep", "rg", "find", "mkdir", "rm", "cp",
    "mv", "touch", "chmod", "chown", "echo", "export", "source", "curl", "wget",
    "ssh", "scp", "rsync", "tar", "unzip", "docker", "kubectl", "helm", "npm",
    "npx", "pnpm", "yarn", "node", "python", "python3", "pip", "pip3", "cargo",
    "rustc", "go", "make", "cmake", "gcc", "clang", "java", "javac", "mvn",
    "gradle", "systemctl", "journalctl", "apt", "apt-get", "dnf", "yum", "pacman",
    "brew", "kill", "ps", "top", "htop", "df", "du", "tail", "head", "less",
    "vim", "nvim", "nano", "code", "awk", "sed", "xargs", "sort", "uniq", "wc",
    "ping", "netstat", "ss", "ip", "ifconfig", "dig", "nslookup", "man", "which",
];

/// Heuristic: does this look like a shell command / code rather than prose?
pub fn looks_like_code(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }

    // First token is a known command name.
    if let Some(first) = t.split_whitespace().next() {
        let head = first.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        if COMMAND_HEADS.contains(&head.to_lowercase().as_str()) {
            return true;
        }
    }

    // Strong structural signals.
    let signals = [
        t.contains("--"),        // long flags
        t.contains("./") || t.contains("../"),
        t.contains("~/"),
        t.contains(" | "),       // pipe
        t.contains("&&") || t.contains("||"),
        t.contains("$(") || t.contains("${"),
        t.contains("=\"") || t.contains("=$"),
        t.matches('/').count() >= 2 && !t.contains(' ') && t.len() > 4, // a path
        t.starts_with('$') || t.starts_with('#') || t.starts_with('`'),
        t.contains(".sh") || t.contains(".rs") || t.contains(".py")
            || t.contains(".js") || t.contains(".ts") || t.contains(".json")
            || t.contains(".toml") || t.contains(".yaml") || t.contains(".yml"),
    ];
    signals.iter().filter(|x| **x).count() >= 1
        && !t.ends_with('.') // a sentence ending in a period is probably prose
        || signals.iter().filter(|x| **x).count() >= 2
}

// ===========================================================================
// Spoken symbols  (applied only to code-like transcripts)
// ===========================================================================

/// Multi-word phrases first (longest match wins), then single words.
/// Case-insensitive, whole-token matching on a space-split stream.
const SYMBOL_PHRASES: &[(&str, &str)] = &[
    ("dash dash", "--"),
    ("double dash", "--"),
    ("dot slash", "./"),
    ("dot dot slash", "../"),
    ("tilde slash", "~/"),
    ("ampersand ampersand", "&&"),
    ("pipe pipe", "||"),
    ("greater than", ">"),
    ("less than", "<"),
    ("open paren", "("),
    ("close paren", ")"),
    ("open parenthesis", "("),
    ("close parenthesis", ")"),
    ("open brace", "{"),
    ("close brace", "}"),
    ("open bracket", "["),
    ("close bracket", "]"),
    ("open angle", "<"),
    ("close angle", ">"),
    ("single quote", "'"),
    ("double quote", "\""),
    ("new line", "\n"),
    ("newline", "\n"),
    ("new paragraph", "\n\n"),
];

const SYMBOL_WORDS: &[(&str, &str)] = &[
    ("dash", "-"),
    ("hyphen", "-"),
    ("minus", "-"),
    ("underscore", "_"),
    ("dot", "."),
    ("period", "."),
    ("slash", "/"),
    ("backslash", "\\"),
    ("pipe", "|"),
    ("tilde", "~"),
    ("equals", "="),
    ("equal", "="),
    ("plus", "+"),
    ("star", "*"),
    ("asterisk", "*"),
    ("ampersand", "&"),
    ("dollar", "$"),
    ("hash", "#"),
    ("hashtag", "#"),
    ("pound", "#"),
    ("percent", "%"),
    ("caret", "^"),
    ("at", "@"),
    ("bang", "!"),
    ("colon", ":"),
    ("semicolon", ";"),
    ("comma", ","),
    ("backtick", "`"),
    ("quote", "\""),
    ("apostrophe", "'"),
    ("question mark", "?"),
    ("tab", "\t"),
];

/// Replace spoken symbol words with literals. For code transcripts only —
/// "a dash of salt" must never become "a - of salt".
pub fn spoken_symbols(s: &str) -> String {
    // Work on a lowercased copy for matching, but emit original casing for
    // non-symbol tokens.
    let tokens: Vec<&str> = s.split(' ').filter(|t| !t.is_empty()).collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        // try 3-, then 2-word phrases
        let mut matched = false;
        for span in (1..=3).rev() {
            if i + span <= tokens.len() {
                let phrase = tokens[i..i + span]
                    .iter()
                    .map(|t| strip_punct(t).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                if let Some((_, sym)) = SYMBOL_PHRASES.iter().find(|(p, _)| *p == phrase) {
                    out.push((*sym).to_string());
                    i += span;
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            continue;
        }

        let bare = strip_punct(tokens[i]).to_lowercase();
        if let Some((_, sym)) = SYMBOL_WORDS.iter().find(|(w, _)| *w == bare) {
            out.push((*sym).to_string());
        } else {
            out.push(tokens[i].to_string());
        }
        i += 1;
    }

    let joined = out.join(" ");
    glue_symbols(&joined)
}

fn strip_punct(t: &str) -> &str {
    t.trim_matches(|c: char| !c.is_alphanumeric())
}

/// Re-attach spelled-out symbols to the tokens they belong with.
///
/// Rules, applied on a space-split token stream:
/// - `- -` (adjacent single dashes) -> `--`
/// - a token that is exactly `-`, `--`, `_`, `.`, `/`, `\`, `~`, `=`, `@`, `:`,
///   `$`, `#`, `+`, `*`, `%`, `^`, backtick -> glue to the FOLLOWING token
///   (`-- force` -> `--force`, `/ etc / hosts` -> `/etc/hosts`, `$ home` -> `$home`)
/// - `&`, `&&`, `|`, `||`, `>`, `<` -> keep spaces around them (shell operators)
fn glue_symbols(s: &str) -> String {
    let raw: Vec<&str> = s.split(' ').filter(|t| !t.is_empty()).collect();

    // pass 1: merge adjacent single dashes into "--"
    let mut toks: Vec<String> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == "-" && i + 1 < raw.len() && raw[i + 1] == "-" {
            toks.push("--".to_string());
            i += 2;
        } else {
            toks.push(raw[i].to_string());
            i += 1;
        }
    }

    // pass 2: glue symbols onto the token that follows.
    //  - flags (`-`, `--`) glue exactly ONE following token: "-- force" -> "--force"
    //  - path separators (`/`, `.`, `..`, `~`, `\`) chain: "/ etc / hosts" -> "/etc/hosts"
    //  - sigils (`$`, `#`, backtick, `@`, `%`, `^`) glue one following token
    //  - operators (`&&`, `||`, `|`, `>`, `<`, `=`, `+`, `*`, `:`) keep their spaces
    const FLAG: &[&str] = &["-", "--"];
    const PATH_SEP: &[&str] = &["/", ".", "..", "~", "\\"];
    const SIGIL: &[&str] = &["$", "#", "`", "@", "%", "^", "_"];

    let mut out: Vec<String> = Vec::with_capacity(toks.len());
    let mut j = 0;
    while j < toks.len() {
        let t = toks[j].as_str();

        if FLAG.contains(&t) && j + 1 < toks.len() {
            out.push(format!("{t}{}", toks[j + 1]));
            j += 2;
        } else if SIGIL.contains(&t) && j + 1 < toks.len() {
            out.push(format!("{t}{}", toks[j + 1]));
            j += 2;
        } else if PATH_SEP.contains(&t) && j + 1 < toks.len() {
            let mut glued = String::from(t);
            glued.push_str(&toks[j + 1]);
            let mut k = j + 2;
            while k + 1 < toks.len() && PATH_SEP.contains(&toks[k].as_str()) {
                glued.push_str(&toks[k]);
                glued.push_str(&toks[k + 1]);
                k += 2;
            }
            // trailing lone separator ("src /" -> "src/")
            if k < toks.len() && PATH_SEP.contains(&toks[k].as_str()) {
                glued.push_str(&toks[k]);
                k += 1;
            }
            out.push(glued);
            j = k;
        } else if (t.ends_with('/') || t.ends_with('~')) && j + 1 < toks.len() {
            out.push(format!("{t}{}", toks[j + 1]));
            j += 2;
        } else {
            out.push(toks[j].clone());
            j += 1;
        }
    }

    out.join(" ")
}

// ===========================================================================
// Tidy
// ===========================================================================

/// `language`: "en"/"auto"/"" and European langs get full prose treatment;
/// anything else (e.g. "bn") gets whitespace-only.
/// `is_code`: true suppresses casing / filler-strip / terminal period and runs
/// `spoken_symbols` instead.
pub fn tidy(raw: &str, language: &str, is_code: bool) -> String {
    let mut s = collapse_ws(raw);
    if s.is_empty() {
        return s;
    }

    if is_code {
        s = spoken_symbols(&s);
        return s.trim().to_string();
    }

    let latin = matches!(
        language,
        "en" | "auto" | "" | "de" | "fr" | "es" | "it" | "pt" | "nl" | "sv" | "da" | "no"
    );

    if latin {
        s = spoken_punctuation(&s);
        s = strip_leading_fillers(&s);
        s = collapse_ws(&s);
        s = fix_space_before_punct(&s);
        s = capitalize_sentences(&s);
        s = ensure_terminal_period(&s);
    } else {
        s = collapse_ws(&s);
        s = fix_space_before_punct(&s);
    }
    s.trim().to_string()
}

fn word_tokens(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn collapse_ws(s: &str) -> String {
    // preserve intentional newlines from "new line"/"new paragraph"
    s.split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn spoken_punctuation(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for tok in s.split(' ') {
        let lower = tok.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        let repl: Option<&str> = match lower.as_str() {
            "comma" => Some(","),
            "period" | "fullstop" => Some("."),
            "questionmark" => Some("?"),
            _ => None,
        };
        if let Some(p) = repl {
            if let Some(prev) = out.last_mut() {
                prev.push_str(p);
            } else {
                out.push(p.to_string());
            }
            continue;
        }
        out.push(tok.to_string());
    }
    let joined = out.join(" ");
    joined
        .replace(" full stop", ".")
        .replace(" question mark", "?")
        .replace(" exclamation mark", "!")
        .replace(" exclamation point", "!")
        .replace(" new line", "\n")
        .replace(" new paragraph", "\n\n")
        .replace(" open quote ", " \"")
        .replace(" close quote", "\"")
}

fn strip_leading_fillers(s: &str) -> String {
    let fillers = ["um", "uh", "er", "erm", "ah", "like", "so", "well"];
    let mut rest = s.trim_start();
    loop {
        let lower = rest.to_lowercase();
        if let Some(stripped) = lower.strip_prefix("you know") {
            let after = &rest[rest.len() - stripped.len()..];
            let after = after.trim_start_matches([',', ' ']);
            if after.len() != rest.len() {
                rest = after;
                continue;
            }
        }
        let mut matched = false;
        for f in fillers {
            if lower.starts_with(f) {
                let after = &rest[f.len()..];
                if after
                    .chars()
                    .next()
                    .map(|c| c == ' ' || c == ',')
                    .unwrap_or(false)
                {
                    rest = after.trim_start_matches([',', ' ']);
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            break;
        }
    }
    rest.to_string()
}

fn fix_space_before_punct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ' ' {
            if let Some(&next) = chars.peek() {
                if matches!(
                    next,
                    ',' | '.' | '!' | '?' | ';' | ':' | '\u{0964}' | '\u{0965}'
                ) {
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

fn capitalize_sentences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_start = true;
    for c in s.chars() {
        if at_start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            at_start = false;
        } else {
            out.push(c);
            if matches!(c, '.' | '!' | '?' | '\n') {
                at_start = true;
            } else if !c.is_whitespace() {
                at_start = false;
            }
        }
    }
    out
}

fn ensure_terminal_period(s: &str) -> String {
    let t = s.trim_end();
    if t.is_empty() {
        return t.to_string();
    }
    match t.chars().last() {
        Some('.') | Some('!') | Some('?') | Some(':') | Some(';') | Some(',') => t.to_string(),
        _ => format!("{t}."),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- prose tidy ----
    #[test]
    fn prose_capitalizes_and_periods() {
        assert_eq!(tidy("hello there", "en", false), "Hello there.");
        assert_eq!(
            tidy("this is one. and two", "en", false),
            "This is one. And two."
        );
    }
    #[test]
    fn prose_strips_leading_fillers() {
        assert_eq!(tidy("um so the plan is ready", "en", false), "The plan is ready.");
        assert_eq!(tidy("uh, hello", "en", false), "Hello.");
        assert_eq!(tidy("you know, it works", "en", false), "It works.");
    }
    #[test]
    fn prose_spoken_punctuation() {
        assert_eq!(tidy("wait comma then go", "en", false), "Wait, then go.");
        assert_eq!(tidy("is it done question mark", "en", false), "Is it done?");
    }
    #[test]
    fn prose_already_clean_is_stable() {
        assert_eq!(
            tidy("The quick brown fox.", "en", false),
            "The quick brown fox."
        );
    }
    #[test]
    fn prose_keeps_a_literal_dash_word() {
        // NOT code -> "dash" stays a word
        assert_eq!(
            tidy("add a dash of salt", "en", false),
            "Add a dash of salt."
        );
    }

    // ---- bangla ----
    #[test]
    fn bangla_only_tidies_whitespace() {
        assert_eq!(tidy("  আমি   ভালো আছি ।  ", "bn", false), "আমি ভালো আছি।");
    }

    // ---- language detection ----
    #[test]
    fn detects_banglish_vs_english() {
        assert!(looks_non_english("ami valo achi tumi kemon acho bhai"));
        assert!(looks_non_english("আমি ভালো আছি"));
        assert!(!looks_non_english("i am doing well how are you today"));
        assert!(!looks_non_english(
            "the meeting is at three in the afternoon"
        ));
        assert!(!looks_non_english("kemon acho")); // too short -> english
    }

    // ---- code detection ----
    #[test]
    fn detects_commands() {
        assert!(looks_like_code("git checkout --force main"));
        assert!(looks_like_code("sudo apt-get install libssl-dev"));
        assert!(looks_like_code("cd ../src && cargo build"));
        assert!(looks_like_code("cat /etc/hosts"));
        assert!(looks_like_code("npm run tauri build"));
        assert!(looks_like_code("./scripts/deploy.sh"));
        assert!(looks_like_code("export PATH=$HOME/bin:$PATH"));
    }
    #[test]
    fn does_not_flag_prose_as_code() {
        assert!(!looks_like_code("let me go to the store and buy milk"));
        assert!(!looks_like_code("the code review is scheduled for tomorrow."));
        assert!(!looks_like_code("i think we should merge this branch"));
        assert!(!looks_like_code("add a dash of salt to the recipe."));
    }

    // ---- spoken symbols (code path) ----
    #[test]
    fn symbols_build_flags_and_paths() {
        assert_eq!(
            spoken_symbols("git checkout dash dash force main"),
            "git checkout --force main"
        );
        assert_eq!(spoken_symbols("cd dot dot slash src"), "cd ../src");
        assert_eq!(
            spoken_symbols("cat slash etc slash hosts"),
            "cat /etc/hosts"
        );
        assert_eq!(spoken_symbols("echo dollar home"), "echo $home");
        // shell operators keep their spacing
        assert_eq!(
            spoken_symbols("npm test ampersand ampersand npm build"),
            "npm test && npm build"
        );
        assert_eq!(
            spoken_symbols("git status dash dash short"),
            "git status --short"
        );
        assert_eq!(spoken_symbols("ls dash la"), "ls -la");
        assert_eq!(
            spoken_symbols("cargo run dash dash release"),
            "cargo run --release"
        );
    }
    #[test]
    fn tidy_code_mode_uses_symbols_no_period() {
        assert_eq!(
            tidy("git status dash dash short", "en", true),
            "git status --short"
        );
        // no capitalization, no trailing period
        assert_eq!(tidy("ls dash la", "en", true), "ls -la");
    }

    // ---- edge cases ----
    #[test]
    fn empty_and_whitespace() {
        assert_eq!(tidy("", "en", false), "");
        assert_eq!(tidy("   ", "en", false), "");
        assert_eq!(tidy("\n\n", "en", false), "");
    }
    #[test]
    fn single_word() {
        assert_eq!(tidy("okay", "en", false), "Okay.");
    }
    #[test]
    fn preserves_new_paragraph_marker() {
        let out = tidy("first line new paragraph second line", "en", false);
        assert!(out.contains("\n\n"), "got: {out:?}");
    }
}
