//! Masks every line of every corpus file and snapshots the result.
//!
//! Tuning a pattern should show exactly which lines moved. That is the whole
//! point of reviewing these diffs rather than asserting on them one at a time.
//!
//! Two tiers, because the honest corpus and the hermetic corpus are not the
//! same corpus. The committed tier runs always. The fetched tier runs only when
//! someone has run `scripts/fetch-corpus.sh`, so a clone with no network is
//! still green.

use oarfish_mask::curated;

/// Renders one file as `raw` / `masked` pairs, blank-line separated. A pair is
/// easier to review than a bare list of templates: you can see what was lost.
fn render(contents: &str) -> String {
    let bundle = curated();
    let mut out = String::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let masked = bundle.mask(line);
        out.push_str("raw:    ");
        out.push_str(masked.raw());
        out.push_str("\nmasked: ");
        out.push_str(masked.template());
        out.push_str("\n\n");
    }
    out
}

#[test]
fn the_committed_corpus_masks_as_snapshotted() {
    insta::glob!("corpus/*.log", |path| {
        let contents = std::fs::read_to_string(path).expect("read corpus file");
        insta::assert_snapshot!(render(&contents));
    });
}

/// Every corpus line masks to the same thing twice. Idempotence proved against
/// real shapes rather than generated strings.
#[test]
fn the_committed_corpus_masks_idempotently() {
    let bundle = curated();
    insta::glob!("corpus/*.log", |path| {
        let contents = std::fs::read_to_string(path).expect("read corpus file");
        for line in contents.lines().filter(|l| !l.trim().is_empty()) {
            let once = bundle.mask(line).template().to_owned();
            let twice = bundle.mask(&once).template().to_owned();
            assert_eq!(once, twice, "not idempotent in {}: {line}", path.display());
        }
    });
}
