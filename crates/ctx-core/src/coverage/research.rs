use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::DocumentEvidence;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JudgmentClass {
    Established,
    Inferred,
    Ambiguous,
    Conflicting,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceKind {
    Code,
    Test,
    Context,
    History,
    Comment,
    Documentation,
    Developer,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub id: String,
    pub kind: EvidenceKind,
    pub source: String,
    pub excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion: Option<DocumentEvidence>,
}

#[derive(Debug, Serialize)]
pub struct ResearchInput {
    pub behavior: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    pub id: String,
    pub quote: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Judgment {
    pub class: JudgmentClass,
    pub explanation: String,
    #[serde(default)]
    pub hypotheses: Vec<String>,
    #[serde(default)]
    pub citations: Vec<Citation>,
    #[serde(default)]
    pub suspected_divergence: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchJudgment {
    pub judgment: Judgment,
    pub document: Option<DocumentEvidence>,
    pub evidence: Vec<Evidence>,
    pub prompt_fingerprint: String,
    pub model: Option<String>,
}

impl ResearchInput {
    /// Validates quotations and restricts automatic resolution to current assertions.
    ///
    /// # Errors
    /// Returns an error if any citation lies outside the evidence package.
    pub fn validate(
        &self,
        mut judgment: Judgment,
        prompt_fingerprint: String,
        model: Option<String>,
    ) -> Result<ResearchJudgment, String> {
        let mut cited = Vec::new();
        for cite in &judgment.citations {
            let evidence = self
                .evidence
                .iter()
                .find(|item| item.id == cite.id)
                .filter(|item| !cite.quote.trim().is_empty() && item.excerpt.contains(&cite.quote))
                .ok_or_else(|| format!("unverified citation {}", cite.id))?;
            cited.push(evidence);
        }
        let assertions: BTreeSet<_> = cited
            .iter()
            .filter_map(|item| item.assertion.as_ref())
            .collect();
        let document = if judgment.class == JudgmentClass::Established
            && !judgment.explanation.trim().is_empty()
            && assertions.len() == 1
        {
            assertions.first().map(|document| (*document).clone())
        } else {
            if judgment.class == JudgmentClass::Established {
                judgment.class = JudgmentClass::Inferred;
            }
            None
        };
        if cited.is_empty() {
            judgment.class = JudgmentClass::Unknown;
        }
        judgment.suspected_divergence &= cited.iter().any(|item| item.kind == EvidenceKind::Code)
            && cited.iter().any(|item| item.assertion.is_some());
        if judgment.suspected_divergence {
            judgment.class = JudgmentClass::Conflicting;
        }
        Ok(ResearchJudgment {
            document: document.filter(|_| judgment.class == JudgmentClass::Established),
            judgment,
            evidence: cited.into_iter().cloned().collect(),
            prompt_fingerprint,
            model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(assertion: Option<DocumentEvidence>) -> ResearchInput {
        ResearchInput {
            behavior: "First retry".to_owned(),
            evidence: vec![Evidence {
                id: "E1".to_owned(),
                kind: if assertion.is_some() {
                    EvidenceKind::Context
                } else {
                    EvidenceKind::Test
                },
                source: "source".to_owned(),
                excerpt: "First retry skips delay.".to_owned(),
                assertion,
            }],
        }
    }

    fn judgment(id: &str) -> Judgment {
        Judgment {
            class: JudgmentClass::Established,
            explanation: "First retry skips delay.".to_owned(),
            hypotheses: vec![],
            citations: vec![Citation {
                id: id.to_owned(),
                quote: "skips delay".to_owned(),
            }],
            suspected_divergence: false,
        }
    }

    #[test]
    fn rejects_unverified_citations_and_downgrades_test_only_explanations() {
        let result = input(None)
            .validate(judgment("E1"), "hash".to_owned(), None)
            .unwrap();
        assert_eq!(result.judgment.class, JudgmentClass::Inferred);
        assert!(
            input(None)
                .validate(judgment("E2"), "hash".to_owned(), None)
                .is_err()
        );
        let asserted = DocumentEvidence {
            id: "INV-1".to_owned(),
            fingerprint: "hash".to_owned(),
        };
        let result = input(Some(asserted.clone()))
            .validate(judgment("E1"), "hash".to_owned(), None)
            .unwrap();
        assert_eq!(result.judgment.class, JudgmentClass::Established);
        assert_eq!(result.document, Some(asserted));
    }
}
