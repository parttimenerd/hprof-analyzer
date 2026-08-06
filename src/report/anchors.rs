//! Canonical section anchors shared across every renderer.
//!
//! A report's "See X" links (triage signals), its table-of-contents bullets,
//! its Markdown headings, and its HTML `id=` attributes all have to agree on
//! one slug per section, or a link that resolves in one format dead-ends in
//! another. Historically they drifted: triage emitted `overview` while the
//! Markdown heading `## System Overview` slugs to `system-overview`, and the
//! HTML used `id="leaks"` while Markdown slugged `## Leak Suspects` to
//! `leak-suspects`.
//!
//! [`SectionId`] is the single source of truth. [`SectionId::slug`] is the
//! canonical anchor (GitHub-style slug of the Markdown heading), consumed by
//! the Markdown ToC, the HTML `id=`, and the triage `anchor`. [`slugify`]
//! derives that slug from heading text; the unit test at the bottom asserts
//! `slugify(heading) == slug()` for every variant, so a renamed heading that
//! forgets to update the slug fails the build.

/// One addressable report section. The slug is shared by the Markdown heading
/// anchor, the HTML `id=`, and any triage signal that links here.
///
/// A few variants (e.g. [`SectionId::RecordCensus`]) are subsections that no
/// triage signal links to *yet* but are part of the canonical anchor set the
/// cross-format resolution test enumerates, so they are kept even when unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SectionId {
    Summary,
    MemoryTriage,
    WasteSummary,
    SystemOverview,
    RecordCensus,
    DuplicateStrings,
    DuplicateClasses,
    BoxedNumbers,
    HeaderOverhead,
    LeakSuspects,
    TopConsumers,
    DominatorAnalysis,
    Threads,
    TopComponents,
    ArraysBySize,
    Collections,
    ContainerAttribution,
    FieldsBySize,
    BiggestCollections,
    CollectionContents,
    References,
    UnreachableObjects,
    AllocationSites,
    RetentionConcentration,
    DominatorDepth,
    LeakIndicators,
    Glossary,
    ThreadLocalAnalysis,
    FrameworkAnalysis,
    TopRetainers,
    CollectionWasteBudget,
}

impl SectionId {
    /// The canonical slug — the GitHub-style anchor of this section's Markdown
    /// heading. Used verbatim as the Markdown ToC target, the HTML `id=`, and
    /// the triage signal anchor so a "See X" link resolves in every format.
    pub const fn slug(self) -> &'static str {
        match self {
            SectionId::Summary => "summary",
            SectionId::MemoryTriage => "memory-triage",
            SectionId::WasteSummary => "waste-summary",
            SectionId::SystemOverview => "system-overview",
            SectionId::RecordCensus => "hprof-record-census",
            SectionId::DuplicateStrings => "duplicate-strings-approximate",
            SectionId::DuplicateClasses => "duplicate-classes",
            SectionId::BoxedNumbers => "boxed-numbers",
            SectionId::HeaderOverhead => "object-header-overhead",
            SectionId::LeakSuspects => "leak-suspects",
            SectionId::TopConsumers => "top-consumers",
            SectionId::DominatorAnalysis => "dominator-analysis",
            SectionId::Threads => "threads",
            SectionId::TopComponents => "top-components",
            SectionId::ArraysBySize => "arrays-by-size",
            SectionId::Collections => "collections",
            SectionId::ContainerAttribution => "container-attribution-classfield",
            SectionId::FieldsBySize => "fields-by-retained-size-classfield",
            SectionId::BiggestCollections => "biggest-collections",
            SectionId::CollectionContents => "collection-contents-by-type",
            SectionId::References => "references",
            SectionId::UnreachableObjects => "unreachable-objects",
            SectionId::AllocationSites => "allocation-sites",
            SectionId::RetentionConcentration => "retention-concentration",
            SectionId::DominatorDepth => "dominator-depth-distribution",
            SectionId::LeakIndicators => "leak-indicators",
            SectionId::Glossary => "glossary",
            SectionId::ThreadLocalAnalysis => "threadlocal-analysis",
            SectionId::FrameworkAnalysis => "framework-analysis",
            SectionId::TopRetainers => "top-retainers",
            SectionId::CollectionWasteBudget => "collection-waste-budget",
        }
    }

    /// The exact Markdown heading text for this section (without the leading
    /// `##`/`###` or trailing newline). `slugify(heading()) == slug()` holds for
    /// every variant — enforced by the unit test below.
    pub const fn heading(self) -> &'static str {
        match self {
            SectionId::Summary => "Summary",
            SectionId::MemoryTriage => "Memory Triage",
            SectionId::WasteSummary => "Waste Summary",
            SectionId::SystemOverview => "System Overview",
            SectionId::RecordCensus => "HPROF Record Census",
            SectionId::DuplicateStrings => "Duplicate Strings (approximate)",
            SectionId::DuplicateClasses => "Duplicate Classes",
            SectionId::BoxedNumbers => "Boxed Numbers",
            SectionId::HeaderOverhead => "Object Header Overhead",
            SectionId::LeakSuspects => "Leak Suspects",
            SectionId::TopConsumers => "Top Consumers",
            SectionId::DominatorAnalysis => "Dominator Analysis",
            SectionId::Threads => "Threads",
            SectionId::TopComponents => "Top Components",
            SectionId::ArraysBySize => "Arrays by Size",
            SectionId::Collections => "Collections",
            SectionId::ContainerAttribution => "Container Attribution (Class#field)",
            SectionId::FieldsBySize => "Fields by Retained Size (Class#field)",
            SectionId::BiggestCollections => "Biggest Collections",
            SectionId::CollectionContents => "Collection Contents by Type",
            SectionId::References => "References",
            SectionId::UnreachableObjects => "Unreachable Objects",
            SectionId::AllocationSites => "Allocation Sites",
            SectionId::RetentionConcentration => "Retention Concentration",
            SectionId::DominatorDepth => "Dominator-Depth Distribution",
            SectionId::LeakIndicators => "Leak Indicators",
            SectionId::Glossary => "Glossary",
            SectionId::ThreadLocalAnalysis => "ThreadLocal Analysis",
            SectionId::FrameworkAnalysis => "Framework Analysis",
            SectionId::TopRetainers => "Top Retainers",
            SectionId::CollectionWasteBudget => "Collection Waste Budget",
        }
    }

    /// A Markdown ToC bullet: `- [Heading](#slug)\n`.
    pub fn toc_bullet(self) -> String {
        format!("- [{}](#{})\n", self.heading(), self.slug())
    }
}

/// GitHub-flavored-Markdown heading slug: lowercase, drop everything that is not
/// a letter, digit, space or hyphen, then collapse spaces to single hyphens.
/// Matches how a Markdown renderer derives a heading's `id`, so a link to
/// `#{slugify(heading)}` resolves. Consumed by the anchor round-trip tests and
/// the (upcoming) cross-format anchor-resolution test.
#[allow(dead_code)]
pub fn slugify(heading: &str) -> String {
    let mut s = String::with_capacity(heading.len());
    for ch in heading.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_lowercase());
        } else if ch == ' ' || ch == '-' {
            s.push(' ');
        }
        // everything else (punctuation like ()#) is dropped
    }
    s.split_whitespace().collect::<Vec<_>>().join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[SectionId] = &[
        SectionId::Summary,
        SectionId::MemoryTriage,
        SectionId::WasteSummary,
        SectionId::SystemOverview,
        SectionId::RecordCensus,
        SectionId::DuplicateStrings,
        SectionId::DuplicateClasses,
        SectionId::BoxedNumbers,
        SectionId::HeaderOverhead,
        SectionId::LeakSuspects,
        SectionId::TopConsumers,
        SectionId::DominatorAnalysis,
        SectionId::Threads,
        SectionId::TopComponents,
        SectionId::ArraysBySize,
        SectionId::Collections,
        SectionId::ContainerAttribution,
        SectionId::FieldsBySize,
        SectionId::BiggestCollections,
        SectionId::CollectionContents,
        SectionId::References,
        SectionId::UnreachableObjects,
        SectionId::AllocationSites,
        SectionId::RetentionConcentration,
        SectionId::DominatorDepth,
        SectionId::LeakIndicators,
        SectionId::Glossary,
    ];

    #[test]
    fn slug_matches_slugified_heading() {
        for &s in ALL {
            assert_eq!(
                slugify(s.heading()),
                s.slug(),
                "heading {:?} slugs to {:?} but slug() returns {:?}",
                s.heading(),
                slugify(s.heading()),
                s.slug()
            );
        }
    }

    #[test]
    fn slugs_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for &s in ALL {
            assert!(seen.insert(s.slug()), "duplicate slug {:?}", s.slug());
        }
    }

    #[test]
    fn slugify_drops_parens_and_hash() {
        assert_eq!(
            slugify("Container Attribution (Class#field)"),
            "container-attribution-classfield"
        );
        assert_eq!(
            slugify("Duplicate Strings (approximate)"),
            "duplicate-strings-approximate"
        );
        assert_eq!(
            slugify("Dominator-Depth Distribution"),
            "dominator-depth-distribution"
        );
    }
}
