// SPDX-License-Identifier: Apache-2.0
//! What an agent may never say on the bus (process v1 §7): no approval or
//! decision in a person's name, no refused action passed on to another
//! agent, and only the registered Queen appears as the Queen.
//!
//! Every check refuses with a named rule and explains it; nothing is
//! rewritten. A message that fails one is not sent (the one exception is
//! `queen_identity` in `warn`, below).
//!
//! - **`on_behalf_of`** (`400 invalid_field`): a message that names a
//!   person it speaks for — the contract's marker `on_behalf_of` as a field,
//!   or plainly in its text ("on behalf of …", "in the name of …").
//! - **`approval_on_behalf`** (`403 forbidden_right`): decision fields with a
//!   person as decider (`by`/`checker`/`answered_by` of kind person,
//!   `approval`, `consent`), a handoff of a person's decision (an Inbox
//!   item, a consent, a G2/G9/G10 approval, a release, deploy, secret or
//!   rights decision), or a sentence that states a person approved
//!   ("the owner approved …", "approved by the person"). The text check is
//!   secondary and conservative: questions, negations and "waiting for"
//!   sentences never match; `progress` and `finding` are never checked.
//! - **`denied_forwarded`** (`403 forbidden_right`): a question, offer or
//!   handoff whose refs (or resource) name what was refused for the sender:
//!   a place Hive denied (`StateV2::denied`), a publish Hive refused, a
//!   vetoed lock or handoff, or a refusal recorded with [`note_refused`].
//! - **`queen_identity`** (`403 forbidden_right` in `enforce`): a sender that
//!   presents itself as the Queen (role `queen`, a display name "Queen", a
//!   "From the Queen:" label) without being a Queen session the app
//!   registered ([`crate::queens`]).
//!
//!   Labels, marks, roles and display names are matched on their folded
//!   form ([`fold`]: NFKC, every default-ignorable character such as
//!   zero-width spaces, the combining grapheme joiner and variation
//!   selectors removed, visible blanks such as the Hangul fillers read as a
//!   space and as nothing, Cyrillic or Greek look-alike letters read as
//!   their Latin letter), so fullwidth, invisible-character or
//!   cross-script look-alikes count as written.
//!
//! The first three are always enforced. `queen_identity` has a rollout
//! switch, `[hive] queen_identity` (env `LEAN_CTX_HIVE_QUEEN_IDENTITY`,
//! which wins), [`QueenMode`]:
//!
//! - `warn` (the default): the message passes, is logged in
//!   `bus/queen-warnings.json` (rule `queen_identity`, mode `warn`; its own
//!   file and cap, so warnings never evict a refused action from
//!   `bus/refused.json`) and carries a visible mark for its receiver,
//!   `[unverified Queen sender: <name>, agent <id>, role <role>]`, in front
//!   of its text. The mark is always built from the authenticated sender
//!   and always put in front; a mark-like part the sender wrote is
//!   neutralised ("[text of the sender, not a mark: …"), so nobody can write
//!   a mark naming someone else. Apart from that the sender's text is kept.
//! - `enforce`: the message is refused. Any other value counts as
//!   `enforce` (fail closed).
//!
//! Switch to `enforce` only once `queen-sessions.json` on the host Instance
//! lists every Queen session (check it read-only): today Queens that run on
//! another machine are not registered with the host, so `enforce` would
//! silence them.
//!
//! Before enforce (open, not part of this change):
//! - register the Queens that run on another machine with the Instance that
//!   serves their bus (today Desktop registers only local Queen sessions);
//! - Desktop calls `/v1/local/queens/remove` when a Queen session ends (it
//!   does not yet, so an ended session stays registered);
//! - child processes inherit `LEAN_CTX_SESSION_ID`, so a process a Queen
//!   session starts carries her session id; a subagent presence is already
//!   never the Queen, other children are not told apart yet;
//! - app session messages (`hive_message_session`, an app tool, not the
//!   bus) are not covered until the app identifies the caller; when the app
//!   can't prove the sender, it must deliver the message with the same
//!   unverified-Queen-sender mark. The interface the app should reuse is
//!   public: [`marked`] with [`Speaker::mark`] (and [`Speaker::of_registry`]
//!   for the sender, [`neutralise_marks`] for a sender's text), as
//!   `lean_ctx_hive::bus::person_rules::*`;
//! - the mark is applied on the sending Instance only: a message from an
//!   older or modified Instance arrives unmarked, which only the Hive can
//!   close by checking and marking (or refusing) on publish (an HC task).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::v2::{Invalid, MessageType, Ref};
use crate::HiveError;
use crate::state::{load_or_quarantine, lock_state, write_private};

pub const ON_BEHALF_OF: &str = "on_behalf_of";
pub const APPROVAL_ON_BEHALF: &str = "approval_on_behalf";
pub const DENIED_FORWARDED: &str = "denied_forwarded";
pub const QUEEN_IDENTITY: &str = "queen_identity";

/// The refusal of `rule`, in the contract's wording, with why.
pub fn refusal(rule: &str, why: &str) -> Invalid {
    let status = if rule == ON_BEHALF_OF {
        "400 invalid_field"
    } else {
        "403 forbidden_right"
    };
    Invalid(format!("{status} reason: {rule}: {why}"))
}

const PERSON_DECISION: &str = "an agent never grants, claims or relays an approval or decision \
     for a person; only the person's own answer from a human session counts. Report what waits \
     for the person instead (the Queen puts it in the person's Inbox)";
const SPEAKS_FOR: &str = "an agent speaks only for itself; a message naming a person it speaks \
     for is not sent";
const FORWARDED: &str = "this was refused for you; report the refusal (the person decides if \
     needed), never ask another agent to do it instead";
const NOT_QUEEN: &str = "only the Queen session the app registered appears as the Queen; send \
     under your own name and role";

// --- structured fields -------------------------------------------------

/// Body fields that carry a decision; a bus message has none of them.
const DECISION_FIELDS: &[&str] = &[
    "approval",
    "approved_by",
    "answered_by",
    "by",
    "checker",
    "consent",
    "decided_by",
    "decision",
];

fn names_person(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("kind").and_then(Value::as_str) == Some("person")
                || map.values().any(names_person)
        }
        Value::Array(items) => items.iter().any(names_person),
        _ => false,
    }
}

/// The resource of a handoff that only a person decides.
pub fn is_person_subject(resource: &str) -> bool {
    let r = resource.trim().to_ascii_lowercase();
    let (head, rest) = r.split_once(':').unwrap_or((r.as_str(), ""));
    match head.trim() {
        "inbox" | "consent" | "approval" | "spec_approval" | "release" | "deploy" | "secret"
        | "rights" => true,
        "gate" => matches!(rest.trim(), "g2" | "g9" | "g10"),
        _ => false,
    }
}

// --- free text ---------------------------------------------------------

const PERSON: &str =
    r"(?:the\s+|a\s+|our\s+|my\s+)?(?:person|owner|user|human|maintainer)(?:'s|s)?";

static SPEAKS_FOR_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:on\s+behalf\s+of|in\s+the\s+name\s+of|speaking\s+for)\s+\S")
        .expect("static pattern")
});

static PERSON_APPROVED: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let verbs = r"(?:approved|consented|signed\s+off|authori[sz]ed|okayed|ok'd|green-?lit|said\s+yes|gave\s+(?:the\s+|their\s+|a\s+|his\s+|her\s+)?(?:go|go-ahead|ok|okay|approval|consent|sign-off))";
    [
        format!(r"(?i)\b{PERSON}\s+(?:has\s+|have\s+|just\s+|already\s+)*{verbs}"),
        format!(r"(?i)\b(?:approved|authori[sz]ed|signed\s+off|okayed|green-?lit)\s+by\s+{PERSON}\b"),
        format!(r"(?i)\b{PERSON}\s+(?:approval|consent|go-ahead|sign-off|go)\s+(?:is\s+|was\s+|has\s+been\s+)?(?:given|granted|received|obtained|in)\b"),
        format!(r"(?i)\bwith\s+{PERSON}\s+(?:approval|consent|go-ahead|sign-off|blessing|ok)\b"),
        format!(r"(?i)\b(?:approve[sd]?|consent(?:ed)?|authori[sz]e[sd]?)\s+(?:this\s+|it\s+)?(?:for|as)\s+{PERSON}\b"),
        r"(?i)\b(?:owner|person)(?:'s)?\s+go\b".to_owned(),
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static pattern"))
    .collect()
});

/// Words that make a sentence a report of what is still open, never a claim.
static OPEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:not|never|no|without|wait|waits|waiting|await|awaits|awaiting|need|needs|needed|require|requires|required|pending|missing|ask|asks|asking|request|requests|requested|until|before|unless|if|whether|once)\b|n't\b",
    )
    .expect("static pattern")
});

/// The sentences of `text` that are no question.
fn statements(text: &str) -> impl Iterator<Item = &str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if matches!(c, '.' | '!' | '?' | ';' | '\n') {
            if c != '?' {
                out.push(&text[start..i]);
            }
            start = i + c.len_utf8();
        }
    }
    out.push(&text[start..]);
    out.into_iter().filter(|s| !s.trim().is_empty())
}

/// The secondary text check: `Some(rule)` when `text` plainly speaks for a
/// person or states a person's approval.
pub fn text_rule(text: &str) -> Option<&'static str> {
    if SPEAKS_FOR_TEXT.is_match(text) {
        return Some(ON_BEHALF_OF);
    }
    statements(text)
        .filter(|s| !OPEN.is_match(s))
        .any(|s| PERSON_APPROVED.iter().any(|re| re.is_match(s)))
        .then_some(APPROVAL_ON_BEHALF)
}

/// The free-text fields of an agent's conversational message.
fn texts(kind: MessageType, body: &Value) -> Vec<&str> {
    let fields: &[&str] = match kind {
        MessageType::Question | MessageType::Answer => &["text"],
        MessageType::Offer => &["what"],
        MessageType::Handoff => &["note", "resource"],
        MessageType::Finding | MessageType::Progress => &[],
    };
    fields.iter().filter_map(|f| body[*f].as_str()).collect()
}

/// The person rules of an agent's publish, checked before its body shape
/// (`Publish::validate`), so the refusal names the rule.
pub fn check_agent_body(kind: MessageType, body: &Value) -> Result<(), Invalid> {
    if let Some(map) = body.as_object() {
        if map.contains_key(ON_BEHALF_OF) {
            return Err(refusal(ON_BEHALF_OF, SPEAKS_FOR));
        }
        if let Some(field) = DECISION_FIELDS.iter().find(|f| {
            map.get(**f).is_some_and(|v| {
                !v.is_null()
                    && (matches!(**f, "approval" | "approved_by" | "answered_by" | "consent")
                        || names_person(v))
            })
        }) {
            return Err(refusal(
                APPROVAL_ON_BEHALF,
                &format!("{field}: {PERSON_DECISION}"),
            ));
        }
    }
    if kind == MessageType::Handoff && body["resource"].as_str().is_some_and(is_person_subject) {
        return Err(refusal(
            APPROVAL_ON_BEHALF,
            &format!("a person's decision is not handed over: {PERSON_DECISION}"),
        ));
    }
    if let Some(rule) = texts(kind, body).into_iter().find_map(text_rule) {
        let why = if rule == ON_BEHALF_OF {
            SPEAKS_FOR
        } else {
            PERSON_DECISION
        };
        return Err(refusal(rule, why));
    }
    Ok(())
}

// --- the Queen ---------------------------------------------------------

/// `text` as the Queen rule reads it: a character that looks like printable
/// ASCII that ASCII, by its UTS #39 skeleton (`latin_skeleton`, the whole
/// Unicode `confusables.txt`), so `Quеen` with a Cyrillic `е` or `Qᴜeen`
/// with a small capital `ᴜ` reads `Queen`; every other character in its
/// NFKC compatibility form (fullwidth `［` is `[`, a no-break space a
/// space); every invisible character removed (the full Unicode
/// `Default_Ignorable_Code_Point` set — zero-width spaces and joiners, soft
/// hyphens, bidi controls, the combining grapheme joiner, variation
/// selectors — and the other format characters, `Cf`); and a visible blank
/// that is no `\s` (the Hangul fillers, the braille blank) a space.
pub fn fold(text: &str) -> String {
    Folded::of(text, Blank::Space).text
}

/// What a visible blank that is no `\s` becomes in a folded text: a space
/// between words, or nothing inside one. The Queen rule reads both.
#[derive(Debug, Clone, Copy)]
enum Blank {
    Space,
    Drop,
}

/// A blank that renders as empty space but is no `\s`: the Hangul fillers
/// (also after NFKC: U+3164 and U+FFA0 become U+1160) and the braille blank.
fn is_visible_blank(c: char) -> bool {
    matches!(
        c,
        '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}' | '\u{2800}'
    )
}

/// Every `Default_Ignorable_Code_Point` (the Unicode set, from the regex
/// crate's Unicode tables) and every other format character (`Cf`).
fn is_invisible(c: char) -> bool {
    use unicode_general_category::{GeneralCategory, get_general_category};
    static IGNORABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\p{Default_Ignorable_Code_Point}$").expect("static pattern")
    });
    let mut buf = [0u8; 4];
    get_general_category(c) == GeneralCategory::Format
        || IGNORABLE.is_match(c.encode_utf8(&mut buf))
}

/// What a non-ASCII character `c` reads as when it looks like printable
/// ASCII: its UTS #39 skeleton (`confusables.txt` of the Unicode version
/// [`unicode_security::UNICODE_VERSION`], through
/// [`unicode_security::skeleton`]) when that is printable ASCII, so the
/// Cyrillic `е`, the Armenian `գ`, the small capital `ᴜ` and the Coptic `Ⲛ`
/// read `e`, `q`, `u` and `N`. A character whose skeleton keeps a non-ASCII
/// part (a letter with a visible accent, a Cyrillic or Greek letter with no
/// Latin twin) is no look-alike: `None`.
///
/// The skeleton writes a glyph by its prototype, and two prototypes are no
/// letter of the words the Queen rule matches: `rn` is the prototype of `m`
/// (a fullwidth `ｍ` reads `m`), and `l` that of `l`, `I`, `1` and `|` (no
/// matched word has an `l`, so it reads `I`: a capital iota or palochka in
/// `UNVERIFIED` is the `I` it looks like).
fn latin_skeleton(c: char) -> Option<String> {
    if c.is_ascii() {
        return None;
    }
    let mut buf = [0u8; 4];
    let mut out = String::new();
    for d in unicode_security::skeleton(c.encode_utf8(&mut buf)) {
        if !d.is_ascii_graphic() {
            return None;
        }
        out.push(if d == 'l' { 'I' } else { d });
    }
    if out.is_empty() {
        return None;
    }
    Some(out.replace("rn", "m"))
}

/// [`fold`] with the way back: for each byte of the folded text, the byte
/// range of the character of `text` it came from.
struct Folded {
    text: String,
    origin: Vec<(usize, usize)>,
}

impl Folded {
    fn of(text: &str, blanks: Blank) -> Self {
        use unicode_normalization::UnicodeNormalization as _;
        let mut out = Self {
            text: String::with_capacity(text.len()),
            origin: Vec::with_capacity(text.len()),
        };
        for (at, c) in text.char_indices() {
            let range = (at, at + c.len_utf8());
            // The look-alike first: NFKC reads the long `ſ` as `s`, but it
            // looks like the `f` its skeleton names.
            if !is_visible_blank(c)
                && !is_invisible(c)
                && let Some(ascii) = latin_skeleton(c)
            {
                out.push_str(&ascii, range);
                continue;
            }
            for d in std::iter::once(c).nfkc() {
                // The blanks first: the Hangul fillers are default ignorable.
                if is_visible_blank(d) {
                    if matches!(blanks, Blank::Space) {
                        out.push_str(" ", range);
                    }
                } else if !is_invisible(d) {
                    match latin_skeleton(d) {
                        Some(ascii) => out.push_str(&ascii, range),
                        None => out.push_str(d.encode_utf8(&mut [0u8; 4]), range),
                    }
                }
            }
        }
        out
    }

    /// Appends `folded`, which came from the original bytes `range`.
    fn push_str(&mut self, folded: &str, range: (usize, usize)) {
        self.text.push_str(folded);
        self.origin.extend(std::iter::repeat_n(range, folded.len()));
    }

    /// Both readings of `text` the Queen rule checks: a visible blank as a
    /// space (between words) and as nothing (inside a word).
    fn both(text: &str) -> [Self; 2] {
        [Self::of(text, Blank::Space), Self::of(text, Blank::Drop)]
    }

    /// The range of the original text a match `start..end` of the folded
    /// text covers (whole original characters).
    fn original(&self, start: usize, end: usize) -> (usize, usize) {
        (self.origin[start].0, self.origin[end - 1].1)
    }
}

static QUEEN_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bqueen\b").expect("static pattern"));

static QUEEN_LABEL: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?im)^\s*[\[(]?\s*(?:a\s+)?(?:message|note|word|instruction|order)?\s*from\s+(?:the\s+)?queen\b",
        r"(?im)^\s*[\[(]?\s*(?:the\s+)?queen\s*[\])]?\s*(?::|—|–|\s-\s)",
        r"(?im)^\s*[\[(]\s*(?:the\s+)?queen\s*[\])]",
        r"(?i)\b(?:i\s+am|i'm|this\s+is|speaking\s+as|signed,?)\s+(?:the\s+)?queen\b",
        r"(?im)\s[—–-]\s*(?:the\s+)?queen\s*$",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static pattern"))
    .collect()
});

/// Who sends, as far as the Queen rule needs it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Speaker<'a> {
    /// The local agent id (`None`: an unregistered sender).
    pub agent_id: Option<&'a str>,
    pub role: Option<&'a str>,
    pub display_name: Option<&'a str>,
    /// The sender is a Queen session the app registered ([`registered_queen`]).
    pub registered_queen: bool,
}

impl<'a> Speaker<'a> {
    /// The speaker `agent` is (none: an unregistered sender, never the Queen).
    pub fn of(agent: Option<&'a lean_ctx::core::agents::AgentEntry>) -> Self {
        Self {
            agent_id: agent.map(|a| a.agent_id.as_str()),
            role: agent.and_then(|a| a.role.as_deref()),
            display_name: agent.and_then(|a| a.display_name.as_deref()),
            registered_queen: agent.is_some_and(registered_queen),
        }
    }

    /// The speaker `agent_id` is in `registry`: its name and role from its
    /// entry, and the registered Queen only while that presence is active
    /// (`AgentRegistry::list_active`: not finished, its process identity
    /// current, its durable identity active).
    pub fn of_registry(
        registry: &'a lean_ctx::core::agents::AgentRegistry,
        agent_id: Option<&str>,
    ) -> Self {
        let entry = agent_id.and_then(|id| registry.agents.iter().find(|a| a.agent_id == id));
        let mut speaker = Self::of(entry);
        speaker.registered_queen = speaker.registered_queen
            && agent_id
                .is_some_and(|id| registry.list_active(None).iter().any(|a| a.agent_id == id));
        speaker
    }

    /// The receiver's mark of an unverified Queen sender: its real name,
    /// agent id and role, each on one line, without a bracket that could end
    /// the mark early and at most [`MARK_FIELD_MAX`] characters long
    /// (`mark_field`).
    pub fn mark(&self) -> String {
        let agent = self.agent_id.unwrap_or("unregistered");
        format!(
            "{MARK_PREFIX}{}, agent {}, role {}]",
            mark_field(self.display_name.unwrap_or(agent)),
            mark_field(agent),
            mark_field(self.role.unwrap_or("none"))
        )
    }
}

/// The longest a name, agent id or role is in a mark ([`Speaker::mark`]).
pub const MARK_FIELD_MAX: usize = 80;

/// `field` as a mark carries it: trimmed; a bracket (also a look-alike,
/// [`fold`]) a parenthesis; a line break, control or other blank a space;
/// cut to [`MARK_FIELD_MAX`] characters with `…` at the cut.
fn mark_field(field: &str) -> String {
    let mut out = String::new();
    for (n, c) in field.trim().chars().enumerate() {
        if n == MARK_FIELD_MAX {
            out.pop();
            out.push('…');
            break;
        }
        let folded = fold(c.encode_utf8(&mut [0u8; 4]));
        let c = if folded.contains('[') {
            '('
        } else if folded.contains(']') {
            ')'
        } else if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        };
        out.push(c);
    }
    out
}

/// How the Queen rule applies (`[hive] queen_identity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueenMode {
    /// Pass, log and mark (the rollout default).
    Warn,
    /// Refuse.
    Enforce,
}

/// The env override of `[hive] queen_identity`.
pub const QUEEN_MODE_ENV: &str = "LEAN_CTX_HIVE_QUEEN_IDENTITY";
/// The start of the receiver's mark ([`Speaker::mark`]).
pub const MARK_PREFIX: &str = "[unverified Queen sender: ";

impl QueenMode {
    /// The mode of a configured value and an env value (the env wins):
    /// none set is `warn`; `warn` or `enforce` as written (case and
    /// surrounding blanks aside); anything else is `enforce` (fail closed).
    pub fn parse(config: Option<&str>, env: Option<&str>) -> Self {
        match env.or(config).map(|v| v.trim().to_ascii_lowercase()) {
            None => Self::Warn,
            Some(v) if v == "warn" => Self::Warn,
            Some(_) => Self::Enforce,
        }
    }

    /// The mode now: the env override, else `[hive] queen_identity`.
    pub fn current() -> Self {
        let env = std::env::var(QUEEN_MODE_ENV).ok();
        let config = lean_ctx::core::config::Config::load().hive.queen_identity;
        Self::parse(config.as_deref(), env.as_deref())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Enforce => "enforce",
        }
    }
}

/// Anything that reads like the receiver's mark ([`MARK_PREFIX`]), in any
/// case or spacing (matched on the [`fold`]ed text; a zero-width character
/// in place of a space leaves no space, so none is required). An `i` of
/// `unverified` is also the ASCII `l`, `1` or `|` it looks like in many
/// fonts (`[UNVERlFIED`); only the mark widens so, never a role or name.
static MARK_LIKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[\s*unver[il1|]f[il1|]ed\s*queen\s*sender\s*:").expect("static pattern")
});

/// What a mark-like text of the sender becomes: it can no longer be read
/// as a mark.
const NEUTRALISED: &str = "[text of the sender, not a mark: unverified Queen sender:";

/// `text` with every mark-like part neutralised: only Hive writes a mark.
/// A look-alike (fullwidth, zero-width characters, [`fold`]) is found too;
/// the rest of the text is kept as written.
pub fn neutralise_marks(text: &str) -> String {
    // Both readings of a visible blank ([`Folded::both`]); the second pass
    // reads the first one's result, whose neutralised parts no longer match.
    let spaced = neutralise_in(text, Blank::Space);
    neutralise_in(&spaced, Blank::Drop)
}

fn neutralise_in(text: &str, blanks: Blank) -> String {
    let folded = Folded::of(text, blanks);
    let mut out = String::with_capacity(text.len());
    let mut kept = 0;
    for m in MARK_LIKE.find_iter(&folded.text) {
        let (start, end) = folded.original(m.start(), m.end());
        // An NFKC expansion can map two matches onto one original
        // character: never slice behind what is already kept.
        let start = start.max(kept);
        out.push_str(&text[kept..start]);
        out.push_str(NEUTRALISED);
        kept = end.max(kept);
    }
    out.push_str(&text[kept..]);
    out
}

/// `text` behind the real `mark` (built from the authenticated sender,
/// [`Speaker::mark`]). Any mark-like part the sender wrote is neutralised,
/// so a sender cannot write a mark naming someone else. A text that already
/// starts with exactly this `mark` (marked by the sending tool, checked
/// again by the relay) is marked only once.
pub fn marked(mark: &str, text: &str) -> String {
    let rest = text
        .strip_prefix(mark)
        .map_or(text, |rest| rest.strip_prefix(' ').unwrap_or(rest));
    format!("{mark} {}", neutralise_marks(rest))
}

/// The refusal name of a text that does not fit its message type.
pub const TEXT_TOO_LONG: &str = "text_too_long";

/// Refuses, before it is sent, a `kind` message whose text as sent (`sent`:
/// `written` itself, or behind the receiver's mark) is over the type's limit
/// ([`super::v2::text_max`]). The mark's characters count: the refusal says
/// how many the mark takes and how many to cut, so a message is never
/// withheld later for its length without the sender knowing.
pub fn check_sent_length(kind: MessageType, written: &str, sent: &str) -> Result<(), Invalid> {
    let Some(max) = super::v2::text_max(kind) else {
        return Ok(());
    };
    // Counted as the relay sends it: trimmed.
    let (written, sent) = (written.trim(), sent.trim());
    let n = sent.chars().count();
    if n <= max {
        return Ok(());
    }
    let mark = if sent == written {
        String::new()
    } else {
        let share = n.saturating_sub(written.chars().count());
        format!(" with the receiver's mark in front ({share} of them)")
    };
    Err(Invalid(format!(
        "400 invalid_field reason: {TEXT_TOO_LONG}: the text is {n} characters{mark}; a {} \
         carries at most {max}: shorten it by {} and send it again",
        format!("{kind:?}").to_ascii_lowercase(),
        n - max
    )))
}

/// `text` cut to at most `max` characters, with a visible note of the cut at
/// its end: for a post the relay sends that was queued before its sending
/// tool checked its length ([`check_sent_length`]), so it is delivered
/// shortened instead of withheld. A text that fits is returned as it is; a
/// mark in front stays whole.
pub fn cut_to_fit(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_owned();
    }
    let note = |cut: usize| format!(" [cut on sending: {cut} more characters did not fit]");
    // The note's length depends on the count it names: shrink what is kept
    // until text and note fit together.
    let mut keep = max;
    loop {
        let fits = max.saturating_sub(note(n - keep).chars().count());
        if fits >= keep {
            break;
        }
        keep = fits;
    }
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(&note(n - keep));
    out
}

/// Whether `text` labels its sender as the Queen ("From the Queen: …"), also
/// as a look-alike ([`fold`]).
pub fn labels_queen(text: &str) -> bool {
    Folded::both(text).iter().any(|folded| {
        let text = &folded.text;
        // A mark-like text names a Queen sender too: only Hive writes a mark.
        MARK_LIKE.is_match(text) || QUEEN_LABEL.iter().any(|re| re.is_match(text))
    })
}

/// Whether a role or display name says Queen, also as a look-alike.
fn says_queen(name: &str) -> bool {
    Folded::both(name)
        .iter()
        .any(|folded| QUEEN_WORD.is_match(&folded.text))
}

/// Refuses a sender that presents itself as the Queen without being the
/// registered Queen: by role, display name, or a label in `texts`.
pub fn check_queen(sender: Speaker<'_>, texts: &[&str]) -> Result<(), Invalid> {
    if sender.registered_queen {
        return Ok(());
    }
    let what = if sender.role.is_some_and(says_queen) {
        "role queen"
    } else if sender.display_name.is_some_and(says_queen) {
        "the display name Queen"
    } else if texts.iter().any(|t| labels_queen(t)) {
        "a \"from the Queen\" label"
    } else {
        return Ok(());
    };
    Err(refusal(QUEEN_IDENTITY, &format!("{what}: {NOT_QUEEN}")))
}

/// Whether `agent` is a Queen session the app registered in `dir`: its own
/// launching session id is registered. A presence registered next to
/// another in the same process (a subagent) is never the Queen.
pub fn registered_queen_in(dir: &Path, agent: &lean_ctx::core::agents::AgentEntry) -> bool {
    agent.parent.is_none()
        && agent.status != lean_ctx::core::agents::AgentStatus::Finished
        && agent
            .app_session_id
            .as_deref()
            .is_some_and(|id| crate::queens::is_queen_in(dir, id))
}

/// [`registered_queen_in`] in this Instance's data dir; unreadable is no.
pub fn registered_queen(agent: &lean_ctx::core::agents::AgentEntry) -> bool {
    crate::queens::data_dir().is_ok_and(|dir| registered_queen_in(&dir, agent))
}

/// The Queen rule in `mode`: `Ok(None)` passes as written, `Ok(Some(mark))`
/// passes with the receiver's mark (`warn`), `Err` refuses (`enforce`).
pub fn queen_rule(
    sender: Speaker<'_>,
    texts: &[&str],
    mode: QueenMode,
) -> Result<Option<String>, Invalid> {
    match (check_queen(sender, texts), mode) {
        (Ok(()), _) => Ok(None),
        (Err(refused), QueenMode::Enforce) => Err(refused),
        (Err(_), QueenMode::Warn) => Ok(Some(sender.mark())),
    }
}

/// [`queen_rule`] for a sending tool: the text to send — `text` itself, or
/// marked and logged in `data_dir`'s `bus/queen-warnings.json` under `warn`. A
/// warning that cannot be logged refuses the message (fail closed). With
/// `limit`, the text to send must fit that type, mark included
/// ([`check_sent_length`]).
pub fn apply_queen_rule(
    data_dir: &Path,
    sender: Speaker<'_>,
    texts: &[&str],
    text: &str,
    limit: Option<MessageType>,
    mode: QueenMode,
) -> Result<String, Invalid> {
    let Some(mark) = queen_rule(sender, texts, mode)? else {
        if let Some(kind) = limit {
            check_sent_length(kind, text, text)?;
        }
        return Ok(text.to_owned());
    };
    let sent = marked(&mark, text);
    // Refused for its length before it is logged: a refused text never went.
    if let Some(kind) = limit {
        check_sent_length(kind, text, &sent)?;
    }
    note_queen_warning(data_dir, sender.agent_id, &mark).map_err(|error| {
        refusal(
            QUEEN_IDENTITY,
            &format!("an unverified Queen sender is logged before it is sent; not logged: {error}"),
        )
    })?;
    Ok(sent)
}

/// The type whose length limit a local post of `category` to `to` must
/// meet: a question to a vault bus leaves as a question (its text at most
/// [`super::v2::TEXT_MAX`], the mark included). Other posts stay local or
/// leave as a progress step the relay shortens itself.
fn relayed_kind(category: &str, to: Option<&str>) -> Option<MessageType> {
    let kind = post_body(category, "")?.0;
    (kind == MessageType::Question
        && matches!(
            super::address::Address::parse(to),
            Ok(super::address::Address::Vault { .. })
        ))
    .then_some(kind)
}

/// The checks of a local `ctx_agent post`; the text to post. The person
/// rules apply when its category is a conversational bus type (a post to a
/// vault bus leaves as that type; `progress` and `finding` stay free); then
/// the Queen rule in `mode` ([`apply_queen_rule`], logged in `data_dir`).
///
/// A question, offer or handoff post is also checked for a refused action
/// it passes on ([`check_not_refused_in`], `data_dir`'s records, fail
/// closed), like the same message sent typed.
pub fn check_local_post_in(
    data_dir: &Path,
    sender: Speaker<'_>,
    worker_id: Option<&str>,
    category: &str,
    text: &str,
    mode: QueenMode,
) -> Result<String, Invalid> {
    check_local_post_to_in(data_dir, sender, worker_id, category, None, text, mode)
}

/// [`check_local_post_in`] of a post addressed `to`: a question to a vault
/// bus is also refused when its text, with the receiver's mark, is over the
/// limit ([`check_sent_length`]), so the relay never withholds it for that.
pub fn check_local_post_to_in(
    data_dir: &Path,
    sender: Speaker<'_>,
    worker_id: Option<&str>,
    category: &str,
    to: Option<&str>,
    text: &str,
    mode: QueenMode,
) -> Result<String, Invalid> {
    if let Some((kind, body)) = post_body(category, text) {
        check_agent_body(kind, &body)?;
        check_not_refused_in(
            data_dir,
            kind,
            &[],
            &body,
            sender.agent_id.unwrap_or_default(),
            worker_id.unwrap_or_default(),
        )?;
    }
    apply_queen_rule(
        data_dir,
        sender,
        &[text],
        text,
        relayed_kind(category, to),
        mode,
    )
}

/// [`check_local_post_to_in`] for `agent_id` of `registry`, in this
/// Instance's Hive data dir and the configured [`QueenMode`]. Without a data
/// dir a check that needs it refuses.
pub fn check_local_post(
    registry: &lean_ctx::core::agents::AgentRegistry,
    agent_id: Option<&str>,
    category: &str,
    to: Option<&str>,
    text: &str,
) -> Result<String, Invalid> {
    let sender = Speaker::of_registry(registry, agent_id);
    let mode = QueenMode::current();
    let needs_records = post_body(category, text).is_some_and(|(kind, _)| asks_to_act(kind));
    if !needs_records && queen_rule(sender, &[text], mode)?.is_none() {
        // Only the person rules apply: no record is read or written.
        if let Some((kind, body)) = post_body(category, text) {
            check_agent_body(kind, &body)?;
        }
        if let Some(kind) = relayed_kind(category, to) {
            check_sent_length(kind, text, text)?;
        }
        return Ok(text.to_owned());
    }
    let store = crate::HiveStore::open().map_err(|error| {
        refusal(
            if needs_records {
                DENIED_FORWARDED
            } else {
                QUEEN_IDENTITY
            },
            &format!("the Hive records cannot be opened, so this post is not sent: {error}"),
        )
    })?;
    let worker_id = agent_id.and_then(|id| super::messages_v2::known_worker_of(&store, id));
    check_local_post_to_in(
        store.data_dir(),
        sender,
        worker_id.as_deref(),
        category,
        to,
        text,
        mode,
    )
}

/// The bus body a local post of `category` leaves as, for the conversational
/// types the person rules check; `None` for a status, progress or finding.
pub fn post_body(category: &str, text: &str) -> Option<(MessageType, Value)> {
    let kind = MessageType::parse(&category.trim().to_ascii_lowercase())?;
    let field = match kind {
        MessageType::Question | MessageType::Answer => "text",
        MessageType::Offer => "what",
        MessageType::Handoff => "note",
        MessageType::Finding | MessageType::Progress => return None,
    };
    Some((kind, serde_json::json!({ field: text })))
}

/// Refuses a question, offer or handoff that names an action refused for
/// `agent_id` (speaking as `worker_id`), from the records in `data_dir`
/// (`denied_forwarded`). Records that cannot be read refuse it too: what
/// was refused is unknown, so nothing that asks another agent goes.
pub fn check_not_refused_in(
    data_dir: &Path,
    kind: MessageType,
    refs: &[Ref],
    body: &Value,
    agent_id: &str,
    worker_id: &str,
) -> Result<(), Invalid> {
    if !asks_to_act(kind) {
        return Ok(());
    }
    let state = super::relay_v2::StateV2::read(data_dir).map_err(|_| records_unknown())?;
    let ledger = RefusedLedger::read(data_dir).map_err(|_| records_unknown())?;
    let refused = Refused::for_sender(
        &state,
        &super::handoff::Handoffs::load(data_dir),
        &ledger,
        &crate::resolution::current_view(data_dir),
        agent_id,
        worker_id,
    );
    check_forwarding(kind, refs, body, &refused)
}

fn records_unknown() -> Invalid {
    refusal(
        DENIED_FORWARDED,
        "the record of refused actions cannot be read right now, so a message that asks another \
         agent is not sent; retry shortly",
    )
}

/// The refused records the relay's outgoing pass checks posts against
/// (its `StateV2` and vault map it holds itself). `ledger: None`: the
/// ledger could not be read; posts that ask another agent wait.
#[derive(Debug, Clone)]
pub struct Records {
    pub handoffs: super::handoff::Handoffs,
    pub ledger: Option<RefusedLedger>,
}

impl Default for Records {
    fn default() -> Self {
        Self {
            handoffs: super::handoff::Handoffs::default(),
            ledger: Some(RefusedLedger::default()),
        }
    }
}

impl Records {
    pub fn load(data_dir: &Path) -> Self {
        Self {
            handoffs: super::handoff::Handoffs::load(data_dir),
            ledger: RefusedLedger::read(data_dir).ok(),
        }
    }
}

// --- refused actions ---------------------------------------------------

const REFUSED_FILE: &str = "refused.json";
/// Refusals remembered at most (the newest stay).
const MAX_REFUSED: usize = 1000;
/// A refusal is remembered this long.
const KEPT: chrono::Duration = chrono::Duration::days(7);

/// One action refused for a sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedAction {
    /// The local agent id or worker id it was refused for; `None`: refused
    /// on this machine, for every sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    /// The refusal's code (`place_denied`, `forbidden_right`, …).
    pub code: String,
    pub at: String,
}

/// `<data>/hive/bus/refused.json`: what was refused, by its reference
/// (a `client_msg_id`, a `msg_…` id, `task:<n>`, a path …).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedLedger {
    #[serde(default)]
    pub refused: BTreeMap<String, RefusedAction>,
}

impl RefusedLedger {
    fn path(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join("bus").join(REFUSED_FILE)
    }

    /// The ledger; missing is empty. Damaged or unreadable is `Err`: the
    /// caller refuses what it would have checked (fail closed). A damaged
    /// file is moved aside on this read, so only this one check fails and
    /// the next one reads an empty ledger.
    pub fn read(data_dir: &Path) -> Result<Self, HiveError> {
        load_or_quarantine(&Self::path(data_dir), |_: &Self| true).map(Option::unwrap_or_default)
    }
}

/// Remember that `reference` was refused for `sender` (`None`: for every
/// sender on this machine) with `code`, under the ledger's lock. A damaged
/// file is moved aside first (never overwritten).
pub fn note_refused(
    data_dir: &Path,
    reference: &str,
    sender: Option<&str>,
    code: &str,
) -> Result<(), HiveError> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now();
    let oldest = (now - KEPT).to_rfc3339();
    change_file(data_dir, REFUSED_FILE, |ledger: &mut RefusedLedger| {
        ledger.refused.retain(|_, r| r.at >= oldest);
        ledger.refused.insert(
            reference.to_owned(),
            RefusedAction {
                sender: sender.map(str::to_owned),
                code: code.to_owned(),
                at: now.to_rfc3339(),
            },
        );
        // Only real refusals are ever in this ledger, so only other real
        // refusals evict one (warnings are kept apart, [`QueenWarnings`]).
        while ledger.refused.len() > MAX_REFUSED {
            let Some(first) = ledger
                .refused
                .iter()
                .min_by(|a, b| a.1.at.cmp(&b.1.at))
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            ledger.refused.remove(&first);
        }
    })
}

/// Load, change and save the JSON file `name` of `data_dir/bus` under its
/// lock. Damaged and moved aside: start over (the older content is kept in
/// the moved copy). Unreadable and still there: never overwritten.
fn change_file<T>(data_dir: &Path, name: &str, change: impl FnOnce(&mut T)) -> Result<(), HiveError>
where
    T: Default + Serialize + serde::de::DeserializeOwned,
{
    let dir = data_dir.join("bus");
    let _lock = lock_state(&dir, name)?;
    let path = dir.join(name);
    let mut value: T = match load_or_quarantine(&path, |_: &T| true) {
        Ok(value) => value.unwrap_or_default(),
        Err(error) if path.exists() => return Err(error),
        Err(error) => {
            tracing::warn!("hive bus: {error}");
            T::default()
        }
    };
    change(&mut value);
    let bytes = serde_json::to_vec(&value).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(&dir, name, &bytes)
}

// --- warnings of an unverified Queen sender ------------------------------

const QUEEN_WARNINGS_FILE: &str = "queen-warnings.json";
/// Warnings remembered at most (the newest stay); their own cap, apart from
/// the refused actions, so no number of warnings evicts a refusal.
const MAX_QUEEN_WARNINGS: usize = 1000;

/// One message sent under `warn` by a sender presenting as the Queen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueenWarning {
    /// The local agent id (`None`: an unregistered sender).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    /// Always `queen_identity`.
    pub rule: String,
    /// Always `warn`: the message went, marked.
    pub mode: String,
    /// The receiver's mark it carried.
    pub mark: String,
    pub at: String,
}

/// `<data>/hive/bus/queen-warnings.json`, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueenWarnings {
    #[serde(default)]
    pub warnings: Vec<QueenWarning>,
}

impl QueenWarnings {
    /// The warnings; missing is empty, damaged is moved aside and `Err`.
    pub fn read(data_dir: &Path) -> Result<Self, HiveError> {
        load_or_quarantine(
            &data_dir.join("bus").join(QUEEN_WARNINGS_FILE),
            |_: &Self| true,
        )
        .map(Option::unwrap_or_default)
    }
}

/// Log that `agent_id` sent as an unverified Queen sender under `warn`, with
/// the receiver's `mark`: rule `queen_identity`, mode `warn`, in
/// `bus/queen-warnings.json` (never in the refused-actions ledger).
pub fn note_queen_warning(
    data_dir: &Path,
    agent_id: Option<&str>,
    mark: &str,
) -> Result<(), HiveError> {
    let now = chrono::Utc::now();
    let oldest = (now - KEPT).to_rfc3339();
    change_file(data_dir, QUEEN_WARNINGS_FILE, |log: &mut QueenWarnings| {
        log.warnings.retain(|w| w.at >= oldest);
        log.warnings.push(QueenWarning {
            sender: agent_id.map(str::to_owned),
            rule: QUEEN_IDENTITY.to_owned(),
            mode: QueenMode::Warn.as_str().to_owned(),
            mark: mark.to_owned(),
            at: now.to_rfc3339(),
        });
        let over = log.warnings.len().saturating_sub(MAX_QUEEN_WARNINGS);
        log.warnings.drain(..over);
    })
}

/// The references refused for one sender, from every record this machine
/// keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refused {
    ids: BTreeSet<String>,
}

/// Ids shorter than this match a ref only exactly, longer ones also inside
/// it (a `msg_…` id in a URL).
const CONTAINED_MIN: usize = 16;

impl Refused {
    pub fn insert(&mut self, id: &str) {
        let id = id.trim();
        if !id.is_empty() {
            self.ids.insert(id.to_owned());
        }
    }

    /// The refused id `target` names, if any.
    pub fn named_by(&self, target: &str) -> Option<&str> {
        let target = target.trim();
        self.ids
            .iter()
            .find(|id| {
                target == id.as_str() || (id.len() >= CONTAINED_MIN && target.contains(id.as_str()))
            })
            .map(String::as_str)
    }

    /// What was refused for `agent_id` (speaking as `worker_id`): places Hive
    /// denied (by key, id and vault address), vetoed locks and handoffs, the
    /// agent's vetoed handoffs, and the ledger's refusals for it or for
    /// every sender.
    pub fn for_sender(
        state: &super::relay_v2::StateV2,
        handoffs: &super::handoff::Handoffs,
        ledger: &RefusedLedger,
        map: &crate::vault::VaultMap,
        agent_id: &str,
        worker_id: &str,
    ) -> Self {
        let mut out = Self::default();
        for place in state.denied.keys() {
            out.insert(place);
            if let Some((_, id)) = place.split_once(':') {
                out.insert(id);
                if let Some(record) = map.vaults.values().find(|r| r.vault_id == id) {
                    out.insert(&super::relay_v2::address_of(record));
                }
            }
        }
        for control in state.controls.values() {
            if control.action == super::v2::ControlAction::Veto
                && let Some(r) = &control.target_ref
            {
                out.insert(&r.id);
            }
        }
        for (client_msg_id, offer) in &handoffs.offers {
            if offer.from == agent_id
                && matches!(offer.state, super::handoff::OfferState::Vetoed { .. })
            {
                out.insert(client_msg_id);
                if let Some(id) = &offer.msg_id {
                    out.insert(id);
                }
            }
        }
        for (reference, action) in &ledger.refused {
            if action
                .sender
                .as_deref()
                .is_none_or(|s| s == agent_id || s == worker_id)
            {
                out.insert(reference);
            }
        }
        out
    }
}

/// Refuses a question, offer or handoff whose refs or resource name an
/// action refused for its sender.
pub fn check_forwarding(
    kind: MessageType,
    refs: &[Ref],
    body: &Value,
    refused: &Refused,
) -> Result<(), Invalid> {
    if !asks_to_act(kind) {
        return Ok(());
    }
    // Refs and the resource by id; the text only by a long id inside it.
    let named = refs
        .iter()
        .map(|r| r.target.as_str())
        .chain(body["resource"].as_str())
        .chain(texts(kind, body))
        .find_map(|t| refused.named_by(t));
    match named {
        Some(id) => Err(refusal(
            DENIED_FORWARDED,
            &format!("{id} was refused: {FORWARDED}"),
        )),
        None => Ok(()),
    }
}

/// The kinds [`check_forwarding`] checks: a question, offer or handoff can
/// ask another agent to act.
pub fn asks_to_act(kind: MessageType) -> bool {
    matches!(
        kind,
        MessageType::Question | MessageType::Offer | MessageType::Handoff
    )
}

#[cfg(test)]
#[path = "person_rules_tests.rs"]
mod tests;
