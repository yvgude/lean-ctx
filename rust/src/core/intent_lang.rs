//! Multilingual building blocks for intent classification (#591).
//!
//! `intent_engine::PHRASE_RULES` only understands English keywords, so
//! non-English tasks ("räum die Funktion auf") used to fall back to
//! Explore/0.3. This module adds language support without ML:
//! verb-stem tables (de/fr/es), function-word language detection, and
//! per-language stopword lists.

use super::intent_engine::TaskType;

/// Verb stems matched via `word.starts_with(stem)` — morphology-tolerant
/// ("beheb" matches behebe/beheben/behebt). Entries containing a space are
/// matched as substrings of the whole lowercased query, like `PHRASE_RULES`.
///
/// Every stem is vetted against common English vocabulary so it cannot
/// shadow an English rule (e.g. "genera" is excluded because it would match
/// "general"; "implementa" because it would match "implementation").
pub(crate) const STEM_RULES: &[(&[&str], TaskType, f64)] = &[
    (
        &[
            // de
            "erstell",
            "schreib",
            "implementier",
            "generier",
            "hinzufüg",
            "baue",
            // fr
            "ajout",
            "crée",
            "créer",
            "implément",
            "écri",
            // es
            "crea",
            "añad",
            "agreg",
            "escrib",
        ],
        TaskType::Generate,
        0.9,
    ),
    (
        &[
            // de
            "beheb",
            "korrigier",
            "fehler",
            "kaputt",
            "funktioniert nicht",
            "geht nicht",
            "stürzt ab",
            // de/fr/es (reparieren / réparer / reparar)
            "repar",
            "répar",
            // fr
            "corrig",
            "ne fonctionne pas",
            // es
            "correg",
            "arregl",
            "soluciona",
            "no funciona",
        ],
        TaskType::FixBug,
        0.95,
    ),
    (
        &[
            // de
            "refaktor",
            "aufräum",
            "räum",
            "bereinig",
            "vereinfach",
            "umbenenn",
            "restruktur",
            "extrahier",
            // en/de/fr/es shared roots
            "refactor",
            "reorganis",
            "reorganiz",
            "simplifi",
            // fr
            "nettoi",
            "renomm",
            // es
            "limpi",
        ],
        TaskType::Refactor,
        0.9,
    ),
    (
        &[
            // de
            "erklär",
            "zeig",
            "versteh",
            "warum",
            "wieso",
            "wie funktioniert",
            "was macht",
            "wo ist",
            // fr
            "pourquoi",
            "expliqu",
            "montre",
            "cherch",
            "comprend",
            "comment fonctionne",
            "à quoi sert",
            // es
            "explica",
            "explíc",
            "busca",
            "busqu",
            "muestr",
            "dónde",
            "cómo",
            "qué hace",
            "por qué",
        ],
        TaskType::Explore,
        0.85,
    ),
    // 0.92 (not 0.9) breaks the tie against Generate verbs so that
    // "schreibe tests" / "écris des tests" routes to Test — mirroring the
    // English "unit test" phrase bonus.
    (
        &["test", "prüf", "verifizi", "vérifi", "verific", "prueb"],
        TaskType::Test,
        0.92,
    ),
    (
        &["debugg", "untersuch", "diagnos", "débog", "depur"],
        TaskType::Debug,
        0.9,
    ),
    (
        &["konfigurier", "einricht", "einstell", "instal", "configur"],
        TaskType::Config,
        0.85,
    ),
    (
        &[
            "veröffentlich",
            "publizier",
            "publie",
            "déploi",
            "despleg",
            "despli",
        ],
        TaskType::Deploy,
        0.85,
    ),
    (
        &[
            "überprüf",
            "begutacht",
            "bewert",
            "kontrollier",
            "évalu",
            "evalu",
            "revis",
            "révis",
        ],
        TaskType::Review,
        0.8,
    ),
];

/// Urgency markers beyond the English list in `detect_urgency`.
pub(crate) const URGENT_WORDS_I18N: &[&str] = &[
    "dringend",
    "sofort",
    "umgehend",
    "kritisch",
    "urgente",
    "urgence",
    "immédiat",
    "inmediat",
    "critique",
    "crítico",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryLang {
    En,
    De,
    Fr,
    Es,
}

const EN_MARKERS: &[&str] = &[
    "the", "and", "is", "not", "with", "for", "on", "please", "can", "what", "how", "why", "this",
    "that", "of", "in", "it", "to", "does", "me",
];
const DE_MARKERS: &[&str] = &[
    "der", "die", "das", "und", "ist", "nicht", "ein", "eine", "mit", "für", "auf", "bitte",
    "kannst", "möchte", "dass", "wie", "den", "dem", "beim", "ich",
];
const FR_MARKERS: &[&str] = &[
    "le", "les", "est", "pas", "dans", "pour", "sur", "qui", "cette", "vous", "peux", "fais",
    "une", "des", "ce", "que", "moi", "du",
];
const ES_MARKERS: &[&str] = &[
    "el", "los", "las", "una", "en", "para", "por", "qué", "cómo", "esta", "puedes", "quiero",
    "esto", "hay", "del", "lo", "que", "la",
];

/// Function-word overlap detection: cheap, deterministic, no ML.
/// Expects lowercased words. Ties (incl. all-zero) resolve to English.
pub(crate) fn detect_query_lang(words: &[&str]) -> QueryLang {
    let count = |markers: &[&str]| -> usize {
        words
            .iter()
            .filter(|w| {
                let w = w.trim_matches(|c: char| !c.is_alphanumeric());
                markers.contains(&w)
            })
            .count()
    };

    let mut best = QueryLang::En;
    let mut best_n = count(EN_MARKERS);
    for (lang, markers) in [
        (QueryLang::De, DE_MARKERS),
        (QueryLang::Fr, FR_MARKERS),
        (QueryLang::Es, ES_MARKERS),
    ] {
        let n = count(markers);
        if n > best_n {
            best = lang;
            best_n = n;
        }
    }
    best
}

// Only words that survive the `len > 3` byte filter in extract_keywords
// matter here; short articles are filtered before stopwords apply.
const DE_STOPWORDS: &[&str] = &[
    "bitte", "kannst", "kann", "möchte", "mache", "machen", "eine", "einen", "einem", "einer",
    "nicht", "auch", "noch", "dann", "wenn", "aber", "oder", "dass", "sind", "wird", "werden",
    "sollte", "könnte", "müssen", "diese", "dieser", "dieses", "alle", "beim", "über", "für",
    "danke", "dich", "mein", "sein",
];
const FR_STOPWORDS: &[&str] = &[
    "dans", "pour", "cette", "vous", "avec", "mais", "plus", "tout", "être", "fait", "faire",
    "fais", "elle", "sont", "cela", "ceci", "comme", "ainsi", "plaît", "peux", "veux", "merci",
    "s'il", "nous", "leur",
];
const ES_STOPWORDS: &[&str] = &[
    "para", "como", "cómo", "este", "esta", "esto", "todo", "pero", "más", "unos", "unas",
    "puedes", "quiero", "favor", "hace", "tiene", "cuando", "aquí", "gracias", "ahora", "también",
    "donde", "dónde",
];

/// Language-specific stopwords (expects a lowercased word).
/// English stopwords stay in `intent_engine::is_stop_word`.
pub(crate) fn is_stop_word_for(lang: QueryLang, w: &str) -> bool {
    let list: &[&str] = match lang {
        QueryLang::En => return false,
        QueryLang::De => DE_STOPWORDS,
        QueryLang::Fr => FR_STOPWORDS,
        QueryLang::Es => ES_STOPWORDS,
    };
    list.contains(&w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lang_of(q: &str) -> QueryLang {
        let q = q.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        detect_query_lang(&words)
    }

    #[test]
    fn detects_german() {
        assert_eq!(lang_of("bitte räum die Funktion auf"), QueryLang::De);
        assert_eq!(lang_of("kannst du den Fehler beheben?"), QueryLang::De);
    }

    #[test]
    fn detects_french() {
        assert_eq!(
            lang_of("peux-tu corriger le bug dans ce module"),
            QueryLang::Fr
        );
    }

    #[test]
    fn detects_spanish() {
        assert_eq!(
            lang_of("puedes corregir el error en la función"),
            QueryLang::Es
        );
    }

    #[test]
    fn defaults_to_english() {
        assert_eq!(lang_of("fix the bug in auth.rs"), QueryLang::En);
        assert_eq!(lang_of("xyz qqq bbb"), QueryLang::En);
    }

    #[test]
    fn stems_do_not_shadow_common_english() {
        // English words that previously caused vetting rejections (e.g.
        // "genera" → "general") must not be caught by any single-token stem.
        // Aligned overlaps like "crea"→"created" (same type as the English
        // rule) are intentional and not listed here.
        let dangerous = [
            "general",
            "implementation",
            "public",
            "comment",
            "where",
            "review",
            "explicit",
            "instead",
            "despite",
        ];
        for word in dangerous {
            for &(stems, task_type, _) in STEM_RULES {
                for stem in stems.iter().filter(|s| !s.contains(' ')) {
                    assert!(
                        !word.starts_with(stem),
                        "stem '{stem}' ({task_type:?}) shadows English word '{word}'"
                    );
                }
            }
        }
    }
}
