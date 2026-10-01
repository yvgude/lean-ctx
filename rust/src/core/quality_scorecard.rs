//! Human/judge rubric for *code-review answers* (correctness, completeness, …).
//!
//! This is graded answer evidence for one comparison arm, not a context-quality measure:
//! scores come from a reviewer (`ScoreConfidence::Human`) or an automated judge and must
//! be reported separately from the deterministic context-quality checks in
//! [`crate::core::context_quality`]. The v4 evidence flow consumes it; keep its API
//! stable on `main` so that branch re-merges cleanly.

use chrono::Utc;
use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};

const MAX_SCORE_MILLI: u16 = 1_000;
const DIMENSION_WEIGHT: u64 = 1;

/// Dimensions used to evaluate a code review response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityDimension {
    /// Findings are technically true and supported by the code.
    Correctness,
    /// Important issues are not missed.
    Completeness,
    /// A developer can understand and apply the recommendation.
    Actionability,
    /// Claims are supported and do not expose secrets.
    Safety,
    /// Findings matter to the code under review.
    Relevance,
}

impl QualityDimension {
    /// Stable iteration order for scorecard comparisons.
    pub const ALL: [Self; 5] = [
        Self::Correctness,
        Self::Completeness,
        Self::Actionability,
        Self::Safety,
        Self::Relevance,
    ];
}

/// Source of confidence for a dimension score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreConfidence {
    Human,
    Automated,
    Unavailable,
}

/// A score for one quality dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DimensionScore {
    pub dimension: QualityDimension,
    #[serde(deserialize_with = "deserialize_score_milli")]
    pub score_milli: u16,
    pub confidence: ScoreConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Quality scores for one arm of a comparison run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityScorecard {
    pub scorecard_id: String,
    pub run_id: String,
    pub arm_type: String,
    pub dimensions: Vec<DimensionScore>,
    pub overall_score_milli: u16,
    pub reviewer: String,
    pub timestamp: String,
}

impl QualityScorecard {
    /// Create an empty scorecard with a unique identifier and RFC 3339 time.
    pub fn new(
        run_id: impl Into<String>,
        arm_type: impl Into<String>,
        reviewer: impl Into<String>,
    ) -> Self {
        Self {
            scorecard_id: format!("quality-scorecard-{}", uuid::Uuid::new_v4()),
            run_id: run_id.into(),
            arm_type: arm_type.into(),
            dimensions: Vec::new(),
            overall_score_milli: 0,
            reviewer: reviewer.into(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    /// Add or replace a dimension score and refresh the overall score.
    pub fn add_score(
        &mut self,
        dimension: QualityDimension,
        score_milli: u16,
        confidence: ScoreConfidence,
        notes: Option<String>,
    ) {
        let score = DimensionScore {
            dimension,
            score_milli: score_milli.min(MAX_SCORE_MILLI),
            confidence,
            notes,
        };

        if let Some(existing) = self
            .dimensions
            .iter_mut()
            .find(|existing| existing.dimension == dimension)
        {
            *existing = score;
        } else {
            self.dimensions.push(score);
        }

        self.overall_score_milli = self.compute_overall();
    }

    /// Compute the equal-weighted average of available dimension scores.
    ///
    /// Unavailable dimensions do not contribute to the denominator.  An empty
    /// scorecard, or one with no available dimensions, therefore scores zero.
    pub fn compute_overall(&self) -> u16 {
        let mut weighted_total = 0_u64;
        let mut total_weight = 0_u64;

        for score in &self.dimensions {
            if score.confidence == ScoreConfidence::Unavailable {
                continue;
            }

            weighted_total += u64::from(score.score_milli.min(MAX_SCORE_MILLI)) * DIMENSION_WEIGHT;
            total_weight += DIMENSION_WEIGHT;
        }

        if total_weight == 0 {
            return 0;
        }

        let average = (weighted_total + total_weight / 2) / total_weight;
        u16::try_from(average).unwrap_or(MAX_SCORE_MILLI)
    }
}

/// Baseline-versus-treatment quality comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityComparison {
    pub baseline_scorecard: QualityScorecard,
    pub treatment_scorecard: QualityScorecard,
    pub quality_preserved: bool,
    pub regression_dimensions: Vec<QualityDimension>,
    pub tolerance_milli: u16,
}

impl QualityComparison {
    /// Default tolerance: five percent of the 0..=1000 milli-unit scale.
    pub const DEFAULT_TOLERANCE_MILLI: u16 = 50;

    /// Compare overall and per-dimension scores using the supplied tolerance.
    pub fn compare(
        baseline: &QualityScorecard,
        treatment: &QualityScorecard,
        tolerance: u16,
    ) -> Self {
        let regression_dimensions: Vec<_> = QualityDimension::ALL
            .into_iter()
            .filter(|dimension| {
                let Some(baseline_score) = score_for(baseline, *dimension) else {
                    return false;
                };
                let Some(treatment_score) = score_for(treatment, *dimension) else {
                    return false;
                };

                if baseline_score.confidence == ScoreConfidence::Unavailable
                    || treatment_score.confidence == ScoreConfidence::Unavailable
                {
                    return false;
                }

                treatment_score.score_milli < baseline_score.score_milli.saturating_sub(tolerance)
            })
            .collect();

        let overall_ok =
            treatment.overall_score_milli >= baseline.overall_score_milli.saturating_sub(tolerance);

        Self {
            baseline_scorecard: baseline.clone(),
            treatment_scorecard: treatment.clone(),
            quality_preserved: overall_ok && regression_dimensions.is_empty(),
            regression_dimensions,
            tolerance_milli: tolerance,
        }
    }
}

fn score_for(scorecard: &QualityScorecard, dimension: QualityDimension) -> Option<&DimensionScore> {
    scorecard
        .dimensions
        .iter()
        .find(|score| score.dimension == dimension)
}

fn deserialize_score_milli<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u16::deserialize(deserializer)?;
    if value <= MAX_SCORE_MILLI {
        Ok(value)
    } else {
        Err(DeError::custom(format!(
            "score_milli must be between 0 and {MAX_SCORE_MILLI}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated_scorecard(arm_type: &str, score_milli: u16) -> QualityScorecard {
        let mut scorecard = QualityScorecard::new("run-1", arm_type, "automated");
        for dimension in QualityDimension::ALL {
            scorecard.add_score(dimension, score_milli, ScoreConfidence::Automated, None);
        }
        scorecard
    }

    #[test]
    fn scorecard_serialization_round_trip() {
        let mut scorecard = QualityScorecard::new("run-1", "baseline", "Ada");
        scorecard.add_score(
            QualityDimension::Correctness,
            975,
            ScoreConfidence::Human,
            Some("verified against the changed code".to_owned()),
        );

        let encoded = serde_json::to_string(&scorecard).expect("scorecard should serialize");
        let decoded: QualityScorecard =
            serde_json::from_str(&encoded).expect("scorecard should deserialize");

        assert_eq!(decoded, scorecard);
        assert!(encoded.contains("\"correctness\""));
        assert!(
            serde_json::from_str::<QualityScorecard>(
                &encoded.replace('}', ",\"unexpected\":true}")
            )
            .is_err()
        );
    }

    #[test]
    fn overall_score_is_weighted_average_of_available_dimensions() {
        let mut scorecard = QualityScorecard::new("run-1", "baseline", "automated");
        scorecard.add_score(
            QualityDimension::Correctness,
            900,
            ScoreConfidence::Human,
            None,
        );
        scorecard.add_score(
            QualityDimension::Completeness,
            800,
            ScoreConfidence::Automated,
            None,
        );
        scorecard.add_score(
            QualityDimension::Actionability,
            700,
            ScoreConfidence::Human,
            None,
        );
        scorecard.add_score(
            QualityDimension::Safety,
            600,
            ScoreConfidence::Automated,
            None,
        );
        scorecard.add_score(
            QualityDimension::Relevance,
            500,
            ScoreConfidence::Human,
            None,
        );

        assert_eq!(scorecard.compute_overall(), 700);
        assert_eq!(scorecard.overall_score_milli, 700);
    }

    #[test]
    fn comparison_detects_regression() {
        let baseline = populated_scorecard("baseline", 900);
        let mut treatment = populated_scorecard("treatment", 900);
        treatment.add_score(
            QualityDimension::Correctness,
            700,
            ScoreConfidence::Automated,
            None,
        );

        let comparison = QualityComparison::compare(&baseline, &treatment, 50);

        assert!(!comparison.quality_preserved);
        assert_eq!(
            comparison.regression_dimensions,
            vec![QualityDimension::Correctness]
        );
    }

    #[test]
    fn comparison_passes_within_tolerance() {
        let baseline = populated_scorecard("baseline", 900);
        let mut treatment = populated_scorecard("treatment", 900);
        treatment.add_score(
            QualityDimension::Correctness,
            850,
            ScoreConfidence::Automated,
            None,
        );

        let comparison = QualityComparison::compare(
            &baseline,
            &treatment,
            QualityComparison::DEFAULT_TOLERANCE_MILLI,
        );

        assert!(comparison.quality_preserved);
        assert!(comparison.regression_dimensions.is_empty());
    }

    #[test]
    fn empty_and_unavailable_scorecards_score_zero() {
        let mut scorecard = QualityScorecard::new("run-1", "baseline", "automated");
        assert_eq!(scorecard.compute_overall(), 0);

        for dimension in QualityDimension::ALL {
            scorecard.add_score(dimension, 1_000, ScoreConfidence::Unavailable, None);
        }

        assert_eq!(scorecard.compute_overall(), 0);
        assert_eq!(scorecard.overall_score_milli, 0);
    }

    #[test]
    fn scores_are_clamped_when_added_and_rejected_when_decoded() {
        let mut scorecard = QualityScorecard::new("run-1", "baseline", "automated");
        scorecard.add_score(
            QualityDimension::Safety,
            u16::MAX,
            ScoreConfidence::Automated,
            None,
        );
        assert_eq!(scorecard.dimensions[0].score_milli, MAX_SCORE_MILLI);

        let invalid = r#"{
            "dimension":"safety",
            "score_milli":1001,
            "confidence":"automated"
        }"#;
        assert!(serde_json::from_str::<DimensionScore>(invalid).is_err());
    }
}
