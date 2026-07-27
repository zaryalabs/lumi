//! Hierarchical-summary planning and citation-coverage risk probe.

use std::collections::BTreeSet;

const SECTION_COUNT: usize = 96;
const SECTIONS_PER_BATCH: usize = 8;

/// Stable result of the deterministic large-book summary probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HierarchicalSummaryProbeReport {
    /// Number of source sections in the fixture book.
    pub source_sections: usize,
    /// Number of bounded first-pass batches.
    pub first_pass_batches: usize,
    /// Number of synthesis levels after first-pass summaries.
    pub synthesis_levels: usize,
    /// Whether every final claim kept at least one revision-bound citation.
    pub citation_coverage_complete: bool,
    /// Maximum number of source or summary units admitted into one request.
    pub max_input_units_per_request: usize,
}

#[derive(Clone, Debug)]
struct SummaryUnit {
    text: String,
    citations: BTreeSet<String>,
}

/// Run a deterministic hierarchical reduction over a large fixture book.
#[must_use]
pub fn run_hierarchical_summary_probe() -> HierarchicalSummaryProbeReport {
    let sections = (0..SECTION_COUNT)
        .map(|index| SummaryUnit {
            text: format!("Раздел {index}: ключевая идея и переход."),
            citations: BTreeSet::from([format!("revision-fixture:section-{index}")]),
        })
        .collect::<Vec<_>>();
    let first_pass = sections
        .chunks(SECTIONS_PER_BATCH)
        .map(synthesize)
        .collect::<Vec<_>>();
    let first_pass_batches = first_pass.len();
    let (final_summary, synthesis_levels) = reduce(first_pass);

    HierarchicalSummaryProbeReport {
        source_sections: SECTION_COUNT,
        first_pass_batches,
        synthesis_levels,
        citation_coverage_complete: !final_summary.text.is_empty()
            && final_summary.citations.len() == SECTION_COUNT,
        max_input_units_per_request: SECTIONS_PER_BATCH,
    }
}

fn reduce(mut level: Vec<SummaryUnit>) -> (SummaryUnit, usize) {
    let mut levels = 0;
    while level.len() > 1 {
        level = level.chunks(SECTIONS_PER_BATCH).map(synthesize).collect();
        levels += 1;
    }
    (
        level.pop().unwrap_or_else(|| SummaryUnit {
            text: String::new(),
            citations: BTreeSet::new(),
        }),
        levels,
    )
}

fn synthesize(units: &[SummaryUnit]) -> SummaryUnit {
    SummaryUnit {
        text: units
            .iter()
            .map(|unit| unit.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        citations: units
            .iter()
            .flat_map(|unit| unit.citations.iter().cloned())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_book_reduction_is_bounded_and_preserves_citation_coverage() {
        let report = run_hierarchical_summary_probe();

        assert_eq!(
            report,
            HierarchicalSummaryProbeReport {
                source_sections: 96,
                first_pass_batches: 12,
                synthesis_levels: 2,
                citation_coverage_complete: true,
                max_input_units_per_request: 8,
            }
        );
    }
}
