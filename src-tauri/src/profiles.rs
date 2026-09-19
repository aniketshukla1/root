//! Profiles: where a process actually spends itself.
//!
//! A trace says which service was slow. A profile says which *function* was,
//! inside that service, and it is the only signal root carries that answers
//! "why" without someone reading code. Gigapipe serves it at
//! `/pyroscope/render`, and the wire format is Pyroscope's flamebearer.
//!
//! ## The flamebearer, and why it is decoded here
//!
//! A flamebearer is not a tree. It is one array of symbol names and one array
//! per row of the graph, each row a flat run of four-integer groups:
//!
//! ```text
//! [ x offset (delta), total samples, self samples, index into names ]
//! ```
//!
//! The offset is delta encoded against the *end of the previous frame on the
//! same row*, so a decoder that reads it as an absolute position produces a
//! graph that is subtly, plausibly wrong — every frame shifted left, the wide
//! ones still wide. That is the worst kind of wrong for a diagnostic, so the
//! decode happens once here, in Rust, with tests, rather than in the renderer.
//!
//! Both implementations of this format encode it the same way: Pyroscope's
//! `deltaEncoding` and gigapipe's `BFS` reset the running offset after every
//! emitted frame and add the parent's self samples between sibling groups.
//! Neither promises the rows arrive in ascending offset order, so parents are
//! resolved by containment against a sorted copy of the row above rather than
//! by walking both in step.
//!
//! ## Frames, not a tree
//!
//! What comes out is a flat, positioned frame list with a parent index. A
//! flame graph draws rectangles, and a tree would have to be flattened back
//! into rectangles to draw it; the parent index is the one thing rectangles
//! cannot recover on their own cheaply, so it is carried rather than derived.
//!
//! ## The ranking is the answer
//!
//! A flame graph is a picture, and a picture is not a finding. The same
//! function usually appears in a dozen places in the graph — once per call
//! path — and no single rectangle is the answer to "what is expensive". So
//! self samples are also summed per symbol and ranked, and that ranking leads
//! the screen. The graph is how you check it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// Integers per frame in the `single` format. `double` (a diff) uses seven,
/// and is refused rather than misread — `render-diff` is a different endpoint
/// answering a different question.
const SINGLE_STRIDE: usize = 4;

/// Symbols listed in the ranking. Past this the tail is noise: a profile with
/// a flat distribution has no hot function, and printing forty of them would
/// imply otherwise.
const MAX_HOT: usize = 12;

/// Frames a profile may carry before it is refused.
///
/// `maxNodes` already asks the store to collapse the tail into an `other`
/// node, so hitting this means the store ignored it. Drawing a hundred
/// thousand rectangles would hang the renderer rather than failing.
const MAX_FRAMES: usize = 50_000;

/// The flamebearer as it arrives.
///
/// Gigapipe answers with the `FlamebearerProfileV1` body — `flamebearer` and
/// `metadata`, and a `timeline` that its render path never fills in. Only the
/// two that are always present are required here.
#[derive(Debug, Clone, Deserialize)]
pub struct Flamebearer {
    pub flamebearer: FlamebearerData,
    #[serde(default)]
    pub metadata: FlamebearerMetadata,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlamebearerData {
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub levels: Vec<Vec<i64>>,
    #[serde(default)]
    pub num_ticks: i64,
    #[serde(default)]
    pub max_self: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlamebearerMetadata {
    /// `single` or `double`. Empty from a store that omits it, which is read
    /// as `single` because that is what `/pyroscope/render` returns.
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub units: String,
    #[serde(default)]
    pub sample_rate: i64,
    /// What was profiled — `cpu`, `inuse_space`, `alloc_objects`.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub spy_name: String,
}

/// One rectangle.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub name: String,
    /// Row in the graph. Zero is the synthetic root spanning everything.
    pub depth: usize,
    /// Samples from the left edge of the profile. Absolute, already decoded.
    pub offset: i64,
    /// Samples in this frame and everything it called.
    pub total: i64,
    /// Samples in this frame alone. `self` is a keyword.
    pub own: i64,
    /// Index into `frames` of the frame this one sits inside.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
}

/// One symbol, with every call site of it summed.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HotSymbol {
    pub name: String,
    /// Self samples across every frame carrying this name.
    pub own: i64,
    /// That, as a share of the profile. Zero when the profile is empty.
    pub share: f64,
    /// How many separate places in the graph it appeared. A symbol reached
    /// from eleven call paths is a different problem from one reached from
    /// one, and the number is the only hint of which it is.
    pub sites: usize,
}

/// A decoded profile, ready to draw.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub frames: Vec<Frame>,
    /// Ranked by self samples, worst first.
    pub hot: Vec<HotSymbol>,
    /// Total samples in the profile — the width of the root.
    pub total: i64,
    /// Largest self value in any one frame, which is what the store's own
    /// heat colouring is scaled against.
    pub max_self: i64,
    /// `samples`, `bytes`, `objects`, `nanoseconds`. Empty when the store did
    /// not say, in which case nothing is rendered in a unit rather than being
    /// rendered in a guessed one.
    pub unit: String,
    pub sample_rate: i64,
    /// What was profiled, as the store names it.
    pub name: String,
    /// The profiler that produced it, when the store says.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub spy: String,
}

impl Profile {
    /// An answered query that found nothing, which is not an error.
    ///
    /// A selector matching no series and a window with no samples in it both
    /// land here, and both are ordinary — the screen says so rather than
    /// showing a failure.
    pub fn empty(metadata: &FlamebearerMetadata) -> Self {
        Profile {
            frames: Vec::new(),
            hot: Vec::new(),
            total: 0,
            max_self: 0,
            unit: metadata.units.clone(),
            sample_rate: metadata.sample_rate,
            name: metadata.name.clone(),
            spy: metadata.spy_name.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// Decode a flamebearer into positioned frames and a self-time ranking.
pub fn decode(raw: &Flamebearer) -> Result<Profile> {
    // `double` is a diff of two profiles with seven integers per frame. Read
    // as `single` it would produce four frames' worth of nonsense from every
    // seven integers, so it is refused by name.
    if raw.metadata.format == "double" {
        return Err(AppError::Collector(
            "that is a diff profile, which this screen does not draw. \
             Ask for one profile rather than two."
                .to_string(),
        ));
    }

    let data = &raw.flamebearer;
    let frame_count: usize = data.levels.iter().map(|l| l.len() / SINGLE_STRIDE).sum();
    if frame_count > MAX_FRAMES {
        return Err(AppError::Collector(format!(
            "that profile has {frame_count} frames, which is more than this screen will \
             draw. Narrow the selector or the window."
        )));
    }

    let mut frames: Vec<Frame> = Vec::with_capacity(frame_count);
    // (offset, total, index into `frames`) for the row above, sorted by
    // offset, so a child finds its parent by binary search.
    let mut above: Vec<(i64, i64, usize)> = Vec::new();

    for (depth, level) in data.levels.iter().enumerate() {
        if level.is_empty() {
            // gigapipe's breadth-first walk appends one empty row past the
            // deepest real one. A row of frames cannot follow an empty row, so
            // this is the end of the graph rather than a hole in it.
            break;
        }
        if level.len() % SINGLE_STRIDE != 0 {
            return Err(AppError::Collector(format!(
                "row {depth} of that profile has {} values, which is not a whole number of \
                 frames — the response was cut short",
                level.len()
            )));
        }

        let mut current: Vec<(i64, i64, usize)> = Vec::with_capacity(level.len() / SINGLE_STRIDE);
        // Where the previous frame on this row ended. The stored offset is the
        // gap from here, not a position.
        let mut cursor: i64 = 0;

        let (chunks, _) = level.as_chunks::<SINGLE_STRIDE>();
        for chunk in chunks {
            let offset = cursor.saturating_add(chunk[0]);
            let total = chunk[1];
            let own = chunk[2];
            cursor = offset.saturating_add(total);

            // An index the names array does not have means the two halves of
            // the response disagree. Naming it is better than blanking the
            // graph: the shape is still true, and one unnamed frame is
            // visibly one unnamed frame.
            let name = usize::try_from(chunk[3])
                .ok()
                .and_then(|i| data.names.get(i))
                .cloned()
                .unwrap_or_else(|| "(unnamed)".to_string());

            let index = frames.len();
            frames.push(Frame {
                name,
                depth,
                offset,
                total,
                own,
                parent: parent_of(&above, offset),
            });
            current.push((offset, total, index));
        }

        current.sort_unstable_by_key(|(offset, _, _)| *offset);
        above = current;
    }

    if frames.is_empty() {
        return Ok(Profile::empty(&raw.metadata));
    }

    // `numTicks` is the store's own total. Falling back to the root frame's
    // width keeps the shares honest against a store that omits it.
    let total = if data.num_ticks > 0 {
        data.num_ticks
    } else {
        frames[0].total
    };

    Ok(Profile {
        hot: rank(&frames, total),
        max_self: if data.max_self > 0 {
            data.max_self
        } else {
            frames.iter().map(|f| f.own).max().unwrap_or(0)
        },
        frames,
        total,
        unit: raw.metadata.units.clone(),
        sample_rate: raw.metadata.sample_rate,
        name: raw.metadata.name.clone(),
        spy: raw.metadata.spy_name.clone(),
    })
}

/// The frame on the row above that contains `offset`.
///
/// Siblings never overlap, so the candidate is the last one starting at or
/// before this offset — and it is only the parent if it also reaches it. A
/// frame whose parent was dropped by the store's node limit has none, which
/// is a fact about the response rather than an error in it.
fn parent_of(above: &[(i64, i64, usize)], offset: i64) -> Option<usize> {
    if above.is_empty() {
        return None;
    }
    let candidate = match above.binary_search_by_key(&offset, |(start, _, _)| *start) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    let (start, total, index) = above[candidate];
    (offset < start.saturating_add(total)).then_some(index)
}

/// Sum self samples per symbol and rank them.
///
/// Per symbol rather than per frame, because the same function called from
/// eleven places is one problem and the graph shows it as eleven slivers —
/// each too thin to notice, and together the reason the process is slow.
fn rank(frames: &[Frame], total: i64) -> Vec<HotSymbol> {
    let mut sums: HashMap<&str, (i64, usize)> = HashMap::new();
    for frame in frames {
        if frame.own <= 0 {
            continue;
        }
        let entry = sums.entry(frame.name.as_str()).or_insert((0, 0));
        entry.0 += frame.own;
        entry.1 += 1;
    }

    let mut ranked: Vec<HotSymbol> = sums
        .into_iter()
        .map(|(name, (own, sites))| HotSymbol {
            name: name.to_string(),
            own,
            share: if total > 0 { own as f64 / total as f64 } else { 0.0 },
            sites,
        })
        .collect();

    // Name breaks the tie so the same profile ranks the same way twice.
    ranked.sort_by(|a, b| b.own.cmp(&a.own).then_with(|| a.name.cmp(&b.name)));
    ranked.truncate(MAX_HOT);
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a row the way both stores do: subtract the running end, then
    /// advance it past this frame. Tests then describe absolute positions and
    /// the encoder produces the wire form, so a test cannot accidentally agree
    /// with a decoder bug.
    fn encode_level(frames: &[(i64, i64, i64, i64)]) -> Vec<i64> {
        let mut out = Vec::new();
        let mut prev = 0;
        for &(offset, total, own, name) in frames {
            out.extend_from_slice(&[offset - prev, total, own, name]);
            prev = offset + total;
        }
        out
    }

    fn profile(names: &[&str], levels: Vec<Vec<i64>>, num_ticks: i64) -> Flamebearer {
        Flamebearer {
            flamebearer: FlamebearerData {
                names: names.iter().map(|n| n.to_string()).collect(),
                levels,
                num_ticks,
                max_self: 0,
            },
            metadata: FlamebearerMetadata {
                format: "single".into(),
                units: "samples".into(),
                sample_rate: 100,
                name: "cpu".into(),
                spy_name: "gospy".into(),
            },
        }
    }

    /// total(100) → [serve(0..60), flush(60..40)], serve → [parse(10..40)].
    fn three_rows() -> Flamebearer {
        profile(
            &["total", "serve", "flush", "parse"],
            vec![
                encode_level(&[(0, 100, 0, 0)]),
                encode_level(&[(0, 60, 20, 1), (60, 40, 40, 2)]),
                encode_level(&[(10, 40, 40, 3)]),
            ],
            100,
        )
    }

    #[test]
    fn offsets_are_absolute_after_decoding() {
        let p = decode(&three_rows()).unwrap();
        let at = |name: &str| p.frames.iter().find(|f| f.name == name).unwrap().clone();
        assert_eq!(at("serve").offset, 0);
        // The delta on the wire is 0 — flush begins exactly where serve ends —
        // and reading it as an absolute position would stack the two.
        assert_eq!(at("flush").offset, 60);
        assert_eq!(at("parse").offset, 10);
    }

    #[test]
    fn a_frame_is_placed_inside_the_frame_it_was_called_from() {
        let p = decode(&three_rows()).unwrap();
        let index = |name: &str| p.frames.iter().position(|f| f.name == name).unwrap();
        assert_eq!(p.frames[index("serve")].parent, Some(index("total")));
        assert_eq!(p.frames[index("flush")].parent, Some(index("total")));
        assert_eq!(p.frames[index("parse")].parent, Some(index("serve")));
        assert_eq!(p.frames[index("total")].parent, None);
    }

    #[test]
    fn a_frame_beyond_its_neighbours_reach_is_not_adopted_by_them() {
        // `orphan` starts at 70, past the end of the only frame on the row
        // above (0..60). Attaching it to `serve` would draw it outside its own
        // parent, which is how a decoder bug looks on screen.
        let raw = profile(
            &["total", "serve", "orphan"],
            vec![
                encode_level(&[(0, 60, 0, 0)]),
                encode_level(&[(0, 60, 60, 1)]),
                encode_level(&[(70, 10, 10, 2)]),
            ],
            60,
        );
        let p = decode(&raw).unwrap();
        let orphan = p.frames.iter().find(|f| f.name == "orphan").unwrap();
        assert_eq!(orphan.parent, None);
    }

    #[test]
    fn a_row_arriving_out_of_order_still_resolves_its_children() {
        // Same graph as `three_rows`, with the second row's two frames written
        // right-to-left. Neither store promises an order, and a decoder that
        // walked both rows in step would hand `parse` the wrong parent.
        let mut level = Vec::new();
        level.extend_from_slice(&[60, 40, 40, 2]);
        level.extend_from_slice(&[-100, 60, 20, 1]);
        let raw = profile(
            &["total", "serve", "flush", "parse"],
            vec![
                encode_level(&[(0, 100, 0, 0)]),
                level,
                encode_level(&[(10, 40, 40, 3)]),
            ],
            100,
        );
        let p = decode(&raw).unwrap();
        let index = |name: &str| p.frames.iter().position(|f| f.name == name).unwrap();
        assert_eq!(p.frames[index("serve")].offset, 0);
        assert_eq!(p.frames[index("flush")].offset, 60);
        assert_eq!(p.frames[index("parse")].parent, Some(index("serve")));
    }

    #[test]
    fn the_same_symbol_in_several_places_is_ranked_once() {
        // `lock` is called from both branches, 30 samples each. Neither frame
        // is the widest thing on screen; together they are the answer.
        let raw = profile(
            &["total", "read", "write", "lock"],
            vec![
                encode_level(&[(0, 100, 0, 0)]),
                encode_level(&[(0, 50, 20, 1), (50, 50, 20, 2)]),
                encode_level(&[(20, 30, 30, 3), (70, 30, 30, 3)]),
            ],
            100,
        );
        let p = decode(&raw).unwrap();
        assert_eq!(p.hot[0].name, "lock");
        assert_eq!(p.hot[0].own, 60);
        assert_eq!(p.hot[0].sites, 2);
        assert!((p.hot[0].share - 0.6).abs() < 1e-9, "{}", p.hot[0].share);
    }

    #[test]
    fn the_ranking_ignores_frames_that_spent_nothing_themselves() {
        let p = decode(&three_rows()).unwrap();
        assert!(
            !p.hot.iter().any(|h| h.name == "total"),
            "a frame with no self samples was ranked: {:?}",
            p.hot
        );
        assert_eq!(p.hot[0].name, "flush");
        assert_eq!(p.hot[0].own, 40);
    }

    #[test]
    fn two_symbols_with_the_same_weight_rank_in_a_stable_order() {
        let raw = profile(
            &["total", "beta", "alpha"],
            vec![
                encode_level(&[(0, 100, 0, 0)]),
                encode_level(&[(0, 50, 50, 1), (50, 50, 50, 2)]),
            ],
            100,
        );
        let first = decode(&raw).unwrap();
        let second = decode(&raw).unwrap();
        assert_eq!(first.hot[0].name, "alpha");
        assert_eq!(first.hot, second.hot);
    }

    #[test]
    fn a_name_index_the_response_does_not_carry_is_named_rather_than_dropped() {
        let raw = profile(
            &["total"],
            vec![
                encode_level(&[(0, 10, 0, 0)]),
                encode_level(&[(0, 10, 10, 9)]),
            ],
            10,
        );
        let p = decode(&raw).unwrap();
        assert_eq!(p.frames.len(), 2);
        assert_eq!(p.frames[1].name, "(unnamed)");
    }

    #[test]
    fn a_truncated_row_is_refused_rather_than_half_read() {
        let raw = profile(&["total"], vec![vec![0, 10, 0]], 10);
        let error = decode(&raw).expect_err("three values is not a frame");
        assert!(error.to_string().contains("cut short"), "{error}");
    }

    #[test]
    fn a_diff_profile_is_refused_by_name() {
        let mut raw = three_rows();
        raw.metadata.format = "double".into();
        let error = decode(&raw).expect_err("a diff has seven values per frame");
        assert!(error.to_string().contains("diff profile"), "{error}");
    }

    #[test]
    fn a_store_with_nothing_to_report_is_an_empty_profile_and_not_an_error() {
        let raw = profile(&[], Vec::new(), 0);
        let p = decode(&raw).unwrap();
        assert!(p.is_empty());
        assert_eq!(p.total, 0);
        assert_eq!(p.unit, "samples");
    }

    #[test]
    fn the_empty_row_gigapipe_appends_ends_the_graph_rather_than_holing_it() {
        let raw = profile(
            &["total", "serve"],
            vec![
                encode_level(&[(0, 10, 0, 0)]),
                encode_level(&[(0, 10, 10, 1)]),
                Vec::new(),
            ],
            10,
        );
        let p = decode(&raw).unwrap();
        assert_eq!(p.frames.len(), 2);
        assert_eq!(p.frames.iter().map(|f| f.depth).max(), Some(1));
    }

    #[test]
    fn a_store_that_omits_its_totals_still_produces_honest_shares() {
        let mut raw = three_rows();
        raw.flamebearer.num_ticks = 0;
        raw.flamebearer.max_self = 0;
        let p = decode(&raw).unwrap();
        assert_eq!(p.total, 100, "the root's width is the fallback total");
        assert_eq!(p.max_self, 40);
        assert!((p.hot[0].share - 0.4).abs() < 1e-9);
    }

    #[test]
    fn the_ranking_is_capped_so_a_flat_profile_does_not_claim_a_culprit() {
        let names: Vec<String> = (0..40).map(|i| format!("fn{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let row: Vec<(i64, i64, i64, i64)> =
            (0..40).map(|i| (i * 10, 10, 10, i)).collect();
        let raw = profile(&refs, vec![encode_level(&row)], 400);
        let p = decode(&raw).unwrap();
        assert_eq!(p.hot.len(), MAX_HOT);
    }

    #[test]
    fn a_profile_too_large_to_draw_is_refused_before_it_is_built() {
        let level: Vec<i64> = (0..(MAX_FRAMES + 1) * SINGLE_STRIDE).map(|_| 1).collect();
        let raw = profile(&["total"], vec![level], 1);
        let error = decode(&raw).expect_err("too many frames");
        assert!(error.to_string().contains("more than this screen"), "{error}");
    }
}
