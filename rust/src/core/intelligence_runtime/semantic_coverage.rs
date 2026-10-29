// SPDX-License-Identifier: Apache-2.0
//! What a semantic model set declares it was qualified for, and whether a
//! text lies inside that. A set states per model the languages and document
//! types it covers (descriptor schema `leanctx.semantic-models/v2`); the host
//! classifies the text cheaply and deterministically and does not ask a
//! model about text outside its declaration. Such text is reported as not
//! inspected, never as covered. A set without declarations (schema v1) is
//! still asked, so its findings mask, but it claims no coverage: every
//! category it examines is reported as not inspected, and its answers say
//! that coverage is unknown.
//!
//! The classifiers err towards "not covered": a language is named only on
//! clear evidence, and text that looks like code or structured data is never
//! taken for prose.

use super::semantic_detectors::CategorySet;

/// What one model of the installed set declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredModel {
    /// The categories its outputs map to.
    pub(crate) categories: CategorySet,
    /// `None`: not declared (coverage unknown along this dimension).
    pub(crate) languages: Option<Vec<String>>,
    pub(crate) document_types: Option<Vec<String>>,
}

/// The declarations of an installed model set, in descriptor order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Declared {
    pub(crate) models: Vec<DeclaredModel>,
}

impl Declared {
    /// Read from descriptor bytes the caller has already bound to the signed
    /// digest. Only the fields the host needs are read.
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        #[derive(serde::Deserialize)]
        struct Descriptor {
            models: Vec<Model>,
        }
        #[derive(serde::Deserialize)]
        struct Model {
            /// Per output index: `none` or the category that index stands for.
            labels: Vec<String>,
            #[serde(default)]
            coverage: Option<Coverage>,
        }
        #[derive(serde::Deserialize)]
        struct Coverage {
            #[serde(default)]
            languages: Option<Vec<String>>,
            #[serde(default)]
            document_types: Option<Vec<String>>,
        }
        let descriptor: Descriptor = serde_json::from_slice(bytes).ok()?;
        Some(Self {
            models: descriptor
                .models
                .into_iter()
                .map(|model| {
                    let categories = CategorySet::EVERY
                        .into_iter()
                        .filter(|category| model.labels.iter().any(|l| l == category.as_str()))
                        .fold(CategorySet::NONE, CategorySet::with);
                    let (languages, document_types) = model
                        .coverage
                        .map_or((None, None), |c| (c.languages, c.document_types));
                    DeclaredModel {
                        categories,
                        languages,
                        document_types,
                    }
                })
                .collect(),
        })
    }

    /// Every category some model examines.
    pub(crate) fn categories(&self) -> CategorySet {
        self.models
            .iter()
            .fold(CategorySet::NONE, |set, model| set.union(model.categories))
    }

    /// Whether any model declares its coverage (a v2 set). The worker of
    /// such a set answers only a host that says it applies declarations.
    pub(crate) fn declares_coverage(&self) -> bool {
        self.models
            .iter()
            .any(|model| model.languages.is_some() || model.document_types.is_some())
    }
}

/// Dimensions along which an inspection's coverage is not declared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Unknown {
    pub(crate) language: bool,
    pub(crate) document_type: bool,
}

impl Unknown {
    /// The coverage reason for an inspection of a set without declarations.
    pub(crate) const fn reason(self) -> Option<&'static str> {
        match (self.language, self.document_type) {
            (true, true) => Some("semantic.coverage_unknown"),
            (true, false) => Some("semantic.language_coverage_unknown"),
            (false, true) => Some("semantic.document_type_coverage_unknown"),
            (false, false) => None,
        }
    }
}

/// Where a text stands against the declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// No model is declared for this text: nothing is asked, and every
    /// category is reported as not inspected for the given reason.
    Outside(&'static str),
    /// Some model is asked. `categories` are those of the models declared
    /// for this text along every dimension: only these count as inspected.
    /// A category of an undeclared model is reported as not inspected; its
    /// findings still mask (fail-safe: no claimed coverage without a
    /// declaration).
    Inside {
        categories: CategorySet,
        unknown: Unknown,
    },
}

/// Why one model is not declared for a text, most fundamental first: when
/// several models are left out, the reported reason is the first of these
/// that applies to any of them.
const GAPS: [&str; 3] = [
    "semantic.document_type_not_covered",
    "semantic.language_undetermined",
    "semantic.language_not_covered",
];

/// `declared` is `None` when the set's declarations cannot be read: the
/// text is then asked about, with every dimension unknown and no category
/// claimed as inspected.
///
/// A language declaration applies to prose only. Code and structured data
/// are not written in a natural language; a model declared for them is
/// asked whatever the words in identifiers or comments are.
pub(crate) fn scope(declared: Option<&Declared>, text: &str) -> Scope {
    let Some(declared) = declared.filter(|declared| !declared.models.is_empty()) else {
        return Scope::Inside {
            categories: CategorySet::NONE,
            unknown: Unknown {
                language: true,
                document_type: true,
            },
        };
    };
    let kind = document_type(text);
    let language = (kind == "prose"
        && declared
            .models
            .iter()
            .any(|model| model.languages.is_some()))
    .then(|| language(text))
    .flatten();
    let mut asked = false;
    let mut categories = CategorySet::NONE;
    let mut unknown = Unknown::default();
    let mut gap: Option<usize> = None;
    for model in &declared.models {
        let type_ok = model
            .document_types
            .as_ref()
            .is_none_or(|declared| declared.iter().any(|item| item == kind));
        let language_ok = kind != "prose"
            || model.languages.as_ref().is_none_or(|declared| {
                language.is_some_and(|found| declared.iter().any(|item| item == found))
            });
        if type_ok && language_ok {
            asked = true;
            let language_unknown = kind == "prose" && model.languages.is_none();
            let type_unknown = model.document_types.is_none();
            // Only a model declared for this text along every dimension
            // speaks for its categories; an undeclared one is asked (its
            // findings mask) but its silence covers nothing.
            if !language_unknown && !type_unknown {
                categories = categories.union(model.categories);
            }
            unknown.language |= language_unknown;
            unknown.document_type |= type_unknown;
        } else {
            let this = if !type_ok {
                0
            } else if language.is_none() {
                1
            } else {
                2
            };
            gap = Some(gap.map_or(this, |seen| seen.min(this)));
        }
    }
    if asked {
        Scope::Inside {
            categories,
            unknown,
        }
    } else {
        Scope::Outside(GAPS[gap.unwrap_or(2)])
    }
}

/// Function words of the declarable languages (`de`, `en`, `fr`, `it`) and
/// of neighbouring ones (`es`, `pt`, `nl`). Each word is frequent in its own
/// language and rare in all the others listed; words two of them share
/// (`la`, `in`, `des`, `del`, `con`, `que`, `die`/Dutch, …) are left out. A
/// neighbouring language is never named: its evidence only keeps a
/// declarable one from being named.
const FUNCTION_WORDS: [(&str, &[&str]); 7] = [
    (
        "de",
        &[
            "der", "die", "das", "und", "ist", "nicht", "ein", "eine", "einer", "den", "dem",
            "mit", "auch", "auf", "für", "von", "zu", "sich", "wird", "werden", "wurde", "sie",
            "im", "sind", "bei", "aus", "nach", "über", "noch", "dass", "oder",
        ],
    ),
    (
        "en",
        &[
            "the", "and", "are", "were", "of", "to", "that", "it", "for", "with", "as", "be", "by",
            "this", "have", "has", "not", "from", "at", "which", "but", "they", "their", "would",
            "been",
        ],
    ),
    (
        "fr",
        &[
            "et", "est", "une", "qui", "dans", "pour", "pas", "sur", "au", "aux", "ce", "cette",
            "sont", "avec", "par", "elle", "été", "nous", "vous", "leur", "être", "ont", "était",
            "comme", "lui", "sans",
        ],
    ),
    (
        "it",
        &[
            "gli", "è", "di", "che", "non", "della", "delle", "dei", "degli", "sono", "nel",
            "nella", "per", "alla", "questo", "questa", "anche", "più", "essere", "loro", "stato",
            "sulla", "dalla", "cui",
        ],
    ),
    (
        "es",
        &[
            "el", "los", "las", "por", "para", "está", "están", "pero", "más", "sus", "muy",
            "también", "hay", "fue", "sobre", "esta", "este", "como", "entre", "puede", "ser",
        ],
    ),
    (
        "pt",
        &[
            "não", "uma", "um", "os", "pelo", "pela", "com", "ao", "aos", "também", "são", "foi",
            "nos", "lhe", "seu", "sua", "isso", "muito", "em", "ou", "mais", "dos",
        ],
    ),
    (
        "nl",
        &[
            "het", "een", "van", "niet", "zijn", "voor", "dat", "met", "op", "aan", "ook", "wordt",
            "worden", "bij", "naar", "maar", "nog", "hij", "deze", "ik", "wij", "wat",
        ],
    ),
];

/// Only these may be named; the others only count against a decision.
const DECLARABLE: [&str; 4] = ["de", "en", "fr", "it"];
/// At least this many function words of the named language …
const MIN_LANGUAGE_HITS: usize = 3;
/// … making up at least this share (in percent) of all words …
const MIN_LANGUAGE_SHARE_PERCENT: usize = 12;
/// … and every other language at most this fraction (1/N) of its count.
/// Text that mixes languages beyond that is undetermined.
const RUNNER_UP_DIVISOR: usize = 4;

/// The language of `text` among `de`, `en`, `fr`, `it`, or `None` unless
/// the evidence is clear: enough function words of one declarable language,
/// a sufficient share of all words, and no other listed language (declarable
/// or neighbouring) above a quarter of it. Deterministic, linear, no model.
pub(crate) fn language(text: &str) -> Option<&'static str> {
    let mut hits = [0_usize; FUNCTION_WORDS.len()];
    let mut words = 0_usize;
    for word in text
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
    {
        words += 1;
        if word.len() > 16 {
            continue;
        }
        let word = word.to_lowercase();
        for (index, (_, list)) in FUNCTION_WORDS.iter().enumerate() {
            if list.contains(&word.as_str()) {
                hits[index] += 1;
            }
        }
    }
    let best = (0..hits.len()).max_by_key(|index| (hits[*index], std::cmp::Reverse(*index)))?;
    let count = hits[best];
    let runner_up = hits
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != best)
        .map(|(_, hits)| *hits)
        .max()
        .unwrap_or(0);
    let name = FUNCTION_WORDS[best].0;
    (DECLARABLE.contains(&name)
        && count >= MIN_LANGUAGE_HITS
        && count * 100 >= words * MIN_LANGUAGE_SHARE_PERCENT
        && runner_up * RUNNER_UP_DIVISOR <= count)
        .then_some(name)
}

/// Characters that are frequent in source code and rare in running text.
const CODE_SYMBOLS: &[char] = &[
    '{', '}', '[', ']', '(', ')', ';', '=', '<', '>', '$', '\\', '|',
];

/// A line that only source code starts this way.
fn code_line(line: &str) -> bool {
    static CODE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    CODE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"^\s*(?:",
            r"def\s+\w+\s*\(|class\s+\w+.*[:({]\s*$|import\s+[\w.]+|from\s+[\w.]+\s+import\b",
            r"|(?:pub\s+)?fn\s+\w+|(?:let|const|var)\s+\w+\s*[:=]|function\b\s*\w*\s*\(",
            r"|use\s+[\w:]+|#include\b|@\w+|package\s+[\w.]+;?\s*$|func\s+\w+\s*\(",
            r"|(?:if|elif|else|for|while|with|try|except|finally)\b.*:\s*$",
            r"|return\b.*;\s*$|\}\s*$|.*\{\s*$",
            r")"
        ))
        .unwrap_or_else(|_| unreachable!("constant pattern"))
    })
    .is_match(line)
}

/// A line of configuration or tabular data: `key: value`, `key = value`,
/// `KEY=value`, an `[section]` header or a list item of such a file.
fn data_line(line: &str) -> bool {
    static DATA: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    DATA.get_or_init(|| {
        regex::Regex::new(
            r#"^\s*(?:(?:export\s+)?["']?[A-Za-z_][\w.\-]*["']?\s*[:=]\s*\S.*|[A-Za-z_][\w.\-]*\s*:\s*$|\[[^\]\n]+\]\s*$|-\s+[\w"'][\w.\-"']*\s*:\s*\S.*)$"#,
        )
        .unwrap_or_else(|_| unreachable!("constant pattern"))
    })
    .is_match(line)
}

/// `code`, `data` or `prose`, from the shape of the text alone:
/// - `data` when at least half of the non-blank lines are key/value pairs,
///   section headers or such list items (YAML, `.env`, INI, properties);
/// - `code` when at least one in twenty non-blank characters is a code
///   symbol, or at least one in seven non-blank lines is a line only code
///   starts with (a docstring-heavy Python file still has its `def`s);
/// - `prose` otherwise.
pub(crate) fn document_type(text: &str) -> &'static str {
    let (mut visible, mut symbols) = (0_usize, 0_usize);
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        visible += 1;
        symbols += usize::from(CODE_SYMBOLS.contains(&c));
    }
    let (mut lines, mut data, mut code) = (0_usize, 0_usize, 0_usize);
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        lines += 1;
        data += usize::from(data_line(line));
        code += usize::from(code_line(line));
    }
    if lines > 0 && data * 2 >= lines {
        "data"
    } else if (visible > 0 && symbols * 20 >= visible) || (lines > 0 && code * 7 >= lines) {
        "code"
    } else {
        "prose"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::intelligence_runtime::semantic_detectors::SemanticCategory;

    const GERMAN: &str = "Den Vertrag unterzeichnete Anna Meier, die mit ihrer Familie in der \
        Stadt wohnt und für die Firma arbeitet. Sie ist seit Jahren bei dem Unternehmen.\n";
    const ENGLISH: &str = "The contract was signed by Anna Meier, who lives with her family in \
        the city and works for the company. She has been with it for years.\n";
    const FRENCH: &str = "Le contrat est signé par Anna Meier, qui habite dans la ville avec sa \
        famille et travaille pour une entreprise.\n";
    const ITALIAN: &str = "Il contratto è stato firmato da Anna Meier, che vive nella città con la \
        sua famiglia e lavora per una ditta della regione.\n";

    #[test]
    fn a_language_is_named_only_on_clear_evidence() {
        assert_eq!(language(GERMAN), Some("de"));
        assert_eq!(language(ENGLISH), Some("en"));
        assert_eq!(language(FRENCH), Some("fr"));
        assert_eq!(language(ITALIAN), Some("it"));
        // Neighbours are never taken for a declarable language.
        let spanish = "El contrato lo firmó Anna Meier con una empresa del sector, que no es \
                       pública. La empresa está en la ciudad y también tiene sus oficinas por \
                       el centro.\n";
        let portuguese = "O contrato foi assinado por Anna Meier, que trabalha com uma empresa \
                          do setor e não é pública. A empresa também tem os seus escritórios \
                          no centro da cidade.\n";
        let dutch = "Het contract is door Anna Meier ondertekend, die met haar familie in de \
                     stad woont en voor een bedrijf werkt.\n";
        for text in [spanish, portuguese, dutch] {
            assert_eq!(language(text), None, "{text}");
        }
        // Mixed beyond a quarter: undetermined, either way round.
        let mixed = format!("{GERMAN}{GERMAN}{GERMAN}{ENGLISH}{ENGLISH}");
        assert_eq!(language(&mixed), None);
        let seventy_thirty = format!("{}{}", GERMAN.repeat(7), ENGLISH.repeat(3));
        assert_eq!(language(&seventy_thirty), None);
        let mixed = format!("{GERMAN}{ENGLISH}");
        assert_eq!(language(&mixed), None);
        // Too few words to tell.
        assert_eq!(language("Anna Meier\n"), None);
        assert_eq!(language("Anna Meier und die Firma\n"), None);
    }

    #[test]
    fn code_and_structured_data_are_never_prose() {
        assert_eq!(document_type(GERMAN), "prose");
        assert_eq!(document_type(ENGLISH), "prose");
        for config in [
            "password: hunter2\n",
            "database:\n  host: db.internal\n  user: admin\n  password: hunter2\n",
            "DATABASE_URL=postgres://db\nSECRET_KEY=abc\n",
            "[server]\nhost = example.org\nport = 8080\n",
        ] {
            assert_eq!(document_type(config), "data", "{config}");
        }
        let python = concat!(
            "import os\n\n",
            "def sign(contract):\n",
            "    \"\"\"Sign the contract on behalf of the company.\n\n",
            "    The signer is the person named in the file, usually the managing\n",
            "    director, and the signature is recorded with the date of signing.\n",
            "    \"\"\"\n",
            "    return contract\n",
        );
        assert_eq!(document_type(python), "code");
        assert_eq!(document_type("fn main() { println!(\"x\"); }\n"), "code");
    }

    fn model(
        categories: CategorySet,
        languages: Option<&[&str]>,
        kinds: Option<&[&str]>,
    ) -> DeclaredModel {
        let owned = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        DeclaredModel {
            categories,
            languages: languages.map(owned),
            document_types: kinds.map(owned),
        }
    }

    #[test]
    fn scope_takes_each_model_for_what_it_declares() {
        let pii = CategorySet::NONE.with(SemanticCategory::Pii);
        let injection = CategorySet::NONE.with(SemanticCategory::PromptInjection);
        let set = Declared {
            models: vec![
                model(pii, Some(&["de"]), Some(&["prose"])),
                model(injection, Some(&["en"]), Some(&["code", "prose"])),
            ],
        };
        let known = Unknown::default();
        assert_eq!(
            scope(Some(&set), GERMAN),
            Scope::Inside {
                categories: pii,
                unknown: known
            }
        );
        assert_eq!(
            scope(Some(&set), ENGLISH),
            Scope::Inside {
                categories: injection,
                unknown: known
            }
        );
        // Code: no language applies; only the model declared for code.
        assert_eq!(
            scope(Some(&set), "fn main() { let x = 1; }\n"),
            Scope::Inside {
                categories: injection,
                unknown: known
            }
        );
        // The most fundamental gap of all left-out models is reported.
        assert_eq!(
            scope(Some(&set), "password: hunter2\n"),
            Scope::Outside("semantic.document_type_not_covered")
        );
        assert_eq!(
            scope(Some(&set), FRENCH),
            Scope::Outside("semantic.language_not_covered")
        );
        assert_eq!(
            scope(Some(&set), "Anna Meier\n"),
            Scope::Outside("semantic.language_undetermined")
        );
        // Partial declarations: the undeclared dimension is unknown, and the
        // model is asked but claims nothing.
        let partial = Declared {
            models: vec![model(pii, None, Some(&["prose"]))],
        };
        assert_eq!(
            scope(Some(&partial), FRENCH),
            Scope::Inside {
                categories: CategorySet::NONE,
                unknown: Unknown {
                    language: true,
                    document_type: false
                }
            }
        );
        // A declared model next to an undeclared one: only the declared
        // model's categories count.
        let mixed = Declared {
            models: vec![
                model(pii, Some(&["de"]), Some(&["prose"])),
                model(injection, None, None),
            ],
        };
        assert_eq!(
            scope(Some(&mixed), GERMAN),
            Scope::Inside {
                categories: pii,
                unknown: Unknown {
                    language: true,
                    document_type: true
                }
            }
        );
        // Nothing declared or readable: asked, everything unknown, nothing
        // claimed.
        for undeclared in [
            None,
            Some(&Declared {
                models: vec![model(CategorySet::ALL, None, None)],
            }),
        ] {
            assert_eq!(
                scope(undeclared, FRENCH),
                Scope::Inside {
                    categories: CategorySet::NONE,
                    unknown: Unknown {
                        language: true,
                        document_type: true
                    }
                }
            );
        }
        assert!(
            set.declares_coverage()
                && !Declared {
                    models: vec![model(pii, None, None)]
                }
                .declares_coverage()
        );
    }
}
