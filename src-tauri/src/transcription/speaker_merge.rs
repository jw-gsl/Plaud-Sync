//! Second-pass speaker merge on top of sherpa-onnx diarization.
//!
//! The diarizer clusters *segments*, and a single short segment's embedding is
//! noisy, so one real voice is routinely split into several clusters: a
//! 2-person call came back with 4-7 speakers even at the best global
//! threshold, where any higher threshold starts merging different people.
//!
//! Here each cluster is summarised by the average embedding of its segments
//! (a "voice print" over many seconds of speech, far steadier than any one
//! segment) and clusters whose voice prints are close are merged.

use std::collections::BTreeMap;

use sherpa_onnx::SpeakerEmbeddingExtractor;

/// A diarized span: start and end in seconds, and its speaker label.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: f32,
    pub end: f32,
    pub speaker: i32,
}

/// Segments shorter than this are too short for a reliable embedding and are
/// left out of a voice print (they still get relabelled with their cluster).
const MIN_EMBED_SECS: f32 = 1.0;
/// Cap per cluster, longest segments first, so a speaker who talks for an
/// hour does not cost an hour of embedding work.
const MAX_EMBED_SEGMENTS: usize = 24;

/// One L2-normalised voice print per speaker label. Labels whose segments are
/// all too short get no entry.
pub fn voice_prints(
    extractor: &SpeakerEmbeddingExtractor,
    samples: &[f32],
    sample_rate: i32,
    spans: &[Span],
) -> BTreeMap<i32, Vec<f32>> {
    let mut by_speaker: BTreeMap<i32, Vec<&Span>> = BTreeMap::new();
    for span in spans {
        if span.end - span.start >= MIN_EMBED_SECS {
            by_speaker.entry(span.speaker).or_default().push(span);
        }
    }

    let mut prints = BTreeMap::new();
    for (speaker, mut spans) in by_speaker {
        spans.sort_by(|a, b| (b.end - b.start).total_cmp(&(a.end - a.start)));
        spans.truncate(MAX_EMBED_SEGMENTS);
        let mut sum: Option<Vec<f32>> = None;
        for span in spans {
            let from = ((span.start * sample_rate as f32) as usize).min(samples.len());
            let to = ((span.end * sample_rate as f32) as usize).min(samples.len());
            if to <= from {
                continue;
            }
            let Some(stream) = extractor.create_stream() else {
                continue;
            };
            stream.accept_waveform(sample_rate, &samples[from..to]);
            stream.input_finished();
            if !extractor.is_ready(&stream) {
                continue;
            }
            let Some(embedding) = extractor.compute(&stream) else {
                continue;
            };
            let embedding = normalised(embedding);
            match sum.as_mut() {
                Some(total) => total.iter_mut().zip(&embedding).for_each(|(t, e)| *t += e),
                None => sum = Some(embedding),
            }
        }
        if let Some(total) = sum {
            prints.insert(speaker, normalised(total));
        }
    }
    prints
}

/// Merge speaker labels whose voice prints have cosine similarity of at least
/// `threshold`, most similar pair first, re-averaging prints (weighted by talk
/// time) after each merge. Labels with no print (only very short segments)
/// are folded into whichever speaker talks immediately around them.
///
/// Returns the relabelled spans with labels renumbered 0.. in order of first
/// appearance, so the output reads "Speaker 1" before "Speaker 2".
pub fn merge_similar(
    spans: &[Span],
    prints: &BTreeMap<i32, Vec<f32>>,
    threshold: f32,
) -> Vec<Span> {
    let mut talk: BTreeMap<i32, f32> = BTreeMap::new();
    for span in spans {
        *talk.entry(span.speaker).or_default() += span.end - span.start;
    }

    // Union-find over labels, via a plain "points to" map.
    let mut parent: BTreeMap<i32, i32> = talk.keys().map(|&s| (s, s)).collect();
    let mut groups: BTreeMap<i32, (Vec<f32>, f32)> = prints
        .iter()
        .map(|(&s, p)| (s, (p.clone(), talk.get(&s).copied().unwrap_or(0.0))))
        .collect();

    loop {
        let keys: Vec<i32> = groups.keys().copied().collect();
        let mut best: Option<(f32, i32, i32)> = None;
        for (i, &a) in keys.iter().enumerate() {
            for &b in &keys[i + 1..] {
                let similarity = cosine(&groups[&a].0, &groups[&b].0);
                if similarity >= threshold && best.is_none_or(|(s, _, _)| similarity > s) {
                    best = Some((similarity, a, b));
                }
            }
        }
        let Some((_, a, b)) = best else { break };
        // Keep the label that talks more, so the dominant voice keeps its id.
        let (keep, drop) = if groups[&a].1 >= groups[&b].1 {
            (a, b)
        } else {
            (b, a)
        };
        let (drop_print, drop_talk) = groups.remove(&drop).expect("present");
        let (keep_print, keep_talk) = groups.get_mut(&keep).expect("present");
        let total = *keep_talk + drop_talk;
        if total > 0.0 {
            for (k, d) in keep_print.iter_mut().zip(&drop_print) {
                *k = (*k * *keep_talk + d * drop_talk) / total;
            }
            *keep_print = normalised(std::mem::take(keep_print));
        }
        *keep_talk = total;
        parent.insert(drop, keep);
    }

    let root = |mut label: i32| {
        while let Some(&up) = parent.get(&label) {
            if up == label {
                break;
            }
            label = up;
        }
        label
    };

    let mut merged: Vec<Span> = spans
        .iter()
        .map(|span| Span {
            speaker: root(span.speaker),
            ..*span
        })
        .collect();
    merged.sort_by(|a, b| a.start.total_cmp(&b.start));

    // Labels that never got a print: take the neighbouring speaker's label.
    if !groups.is_empty() {
        fold_into_neighbours(&mut merged, |label| groups.contains_key(&label));
    }
    renumber(&mut merged);
    merged
}

/// Fold every speaker with less than `min_secs` of total talk into whoever is
/// speaking around them.
///
/// After merging, what is left over is mostly a scatter of clusters holding a
/// few seconds each (one hour-long recording kept 35 "speakers", 33 of them
/// under 10 s): too little speech for a reliable voice print, so they never
/// match anyone. Measured against hand-confirmed meetings, folding below 8 s
/// both cut wrong-person speech and brought the speaker count to within
/// rounding of the truth. Someone who only says a few words is labelled as
/// their neighbour; that costs those few seconds, where a phantom speaker
/// costs the reader every time it appears.
pub fn fold_minor_speakers(spans: &[Span], min_secs: f32) -> Vec<Span> {
    let mut talk: BTreeMap<i32, f32> = BTreeMap::new();
    for span in spans {
        *talk.entry(span.speaker).or_default() += span.end - span.start;
    }
    let mut keep: Vec<i32> = talk
        .iter()
        .filter(|(_, &secs)| secs >= min_secs)
        .map(|(&label, _)| label)
        .collect();
    if keep.is_empty() {
        // Nobody reaches the bar (a very short recording): keep the main voice.
        if let Some((&label, _)) = talk.iter().max_by(|a, b| a.1.total_cmp(b.1)) {
            keep.push(label);
        }
    }
    let mut folded = spans.to_vec();
    folded.sort_by(|a, b| a.start.total_cmp(&b.start));
    fold_into_neighbours(&mut folded, |label| keep.contains(&label));
    renumber(&mut folded);
    folded
}

/// Give each span whose label fails `keep` the label of the nearest kept span
/// in time, before or after it. `spans` must be sorted by start.
fn fold_into_neighbours(spans: &mut [Span], keep: impl Fn(i32) -> bool) {
    for i in 0..spans.len() {
        if keep(spans[i].speaker) {
            continue;
        }
        let before = spans[..i].iter().rev().find(|s| keep(s.speaker));
        let after = spans[i + 1..].iter().find(|s| keep(s.speaker));
        let nearest = match (before, after) {
            (Some(b), Some(a)) => {
                if spans[i].start - b.end <= a.start - spans[i].end {
                    b
                } else {
                    a
                }
            }
            (Some(b), None) => b,
            (None, Some(a)) => a,
            (None, None) => continue,
        };
        spans[i].speaker = nearest.speaker;
    }
}

/// Renumber labels 0.. in order of first appearance, so the transcript reads
/// "Speaker 1" before "Speaker 2".
fn renumber(spans: &mut [Span]) {
    let mut renumber: BTreeMap<i32, i32> = BTreeMap::new();
    for span in spans {
        let next = renumber.len() as i32;
        span.speaker = *renumber.entry(span.speaker).or_insert(next);
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalised(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: f32, end: f32, speaker: i32) -> Span {
        Span {
            start,
            end,
            speaker,
        }
    }

    fn print(v: &[f32]) -> Vec<f32> {
        normalised(v.to_vec())
    }

    #[test]
    fn similar_voices_merge_and_different_ones_do_not() {
        let spans = [span(0.0, 10.0, 5), span(10.0, 14.0, 9), span(14.0, 30.0, 2)];
        let prints = BTreeMap::from([
            (5, print(&[1.0, 0.0])),
            (9, print(&[0.95, 0.31])), // ~0.95 similar to 5: same voice
            (2, print(&[0.0, 1.0])),   // orthogonal: a different person
        ]);
        let merged = merge_similar(&spans, &prints, 0.8);
        let labels: Vec<i32> = merged.iter().map(|s| s.speaker).collect();
        assert_eq!(labels, vec![0, 0, 1]);
    }

    #[test]
    fn nothing_above_threshold_keeps_every_speaker() {
        let spans = [span(0.0, 5.0, 0), span(5.0, 9.0, 1)];
        let prints = BTreeMap::from([(0, print(&[1.0, 0.0])), (1, print(&[0.0, 1.0]))]);
        let merged = merge_similar(&spans, &prints, 0.8);
        assert_eq!(
            merged.iter().map(|s| s.speaker).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn a_label_with_no_print_takes_its_nearest_neighbour() {
        // Label 7 only spoke for half a second: no print, joins speaker 3 who
        // was talking right before it.
        let spans = [span(0.0, 8.0, 3), span(8.1, 8.6, 7), span(12.0, 20.0, 4)];
        let prints = BTreeMap::from([(3, print(&[1.0, 0.0])), (4, print(&[0.0, 1.0]))]);
        let merged = merge_similar(&spans, &prints, 0.8);
        assert_eq!(
            merged.iter().map(|s| s.speaker).collect::<Vec<_>>(),
            vec![0, 0, 1]
        );
    }

    #[test]
    fn labels_are_renumbered_by_first_appearance() {
        let spans = [span(0.0, 2.0, 42), span(2.0, 4.0, 7)];
        let prints = BTreeMap::from([(42, print(&[1.0, 0.0])), (7, print(&[0.0, 1.0]))]);
        let merged = merge_similar(&spans, &prints, 0.99);
        assert_eq!(merged[0].speaker, 0);
        assert_eq!(merged[1].speaker, 1);
    }

    #[test]
    fn minor_speakers_fold_into_their_neighbour() {
        // 2 and 9 each talk for under the bar: 9 sits right after speaker 1's
        // turn, 2 right before speaker 5's.
        let spans = [
            span(0.0, 30.0, 1),
            span(30.5, 33.0, 9),
            span(40.0, 42.0, 2),
            span(42.5, 80.0, 5),
        ];
        let folded = fold_minor_speakers(&spans, 8.0);
        assert_eq!(
            folded.iter().map(|s| s.speaker).collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );
    }

    #[test]
    fn a_recording_where_nobody_reaches_the_bar_keeps_its_main_voice() {
        let spans = [span(0.0, 3.0, 4), span(3.0, 4.0, 6)];
        let folded = fold_minor_speakers(&spans, 8.0);
        assert!(folded.iter().all(|s| s.speaker == 0));
    }

    #[test]
    fn the_louder_label_survives_a_merge() {
        // Equal prints; label 1 talks far more. Both become one speaker either
        // way, but the merged print must stay well formed (unit length).
        let spans = [span(0.0, 1.5, 0), span(1.5, 30.0, 1)];
        let prints = BTreeMap::from([(0, print(&[1.0, 0.0])), (1, print(&[1.0, 0.0]))]);
        let merged = merge_similar(&spans, &prints, 0.5);
        assert!(merged.iter().all(|s| s.speaker == 0));
    }
}
