//! Text segmentation for streaming synthesis.
//!
//! Supertonic-3 synthesizes each chunk of text independently — there is no
//! cross-chunk conditioning, so the model deduces tone and intonation purely
//! from the chunk it is handed. That makes chunk boundaries load-bearing for
//! prosody: a chunk must be a complete, natural unit (a sentence or a small
//! group of sentences) ending in punctuation, never a mid-phrase fragment.
//!
//! [`plan_chunks`] turns a raw (possibly markdown) document into an ordered
//! list of [`PlannedChunk`]s ready to synthesize one at a time. The pipeline is
//! markdown normalization → paragraph split → sentence boundary detection →
//! accumulate sentences up to a target length (hard-splitting over-long
//! sentences at clause boundaries). Each chunk carries the silence to insert
//! after it, scaled to whether it ends a clause, a sentence, or a paragraph, so
//! a streaming consumer can play gaplessly with natural pauses.
//!
//! The numbers below follow streaming-TTS practice: target ~150–250 characters
//! per chunk, hard cap ~300 (long inputs make this model rush), a short first
//! chunk to minimize time-to-first-audio, and ~200 ms inter-sentence /
//! ~300–400 ms inter-paragraph silence (Azure's documented default is 200 ms).

use regex::Regex;

/// Target characters per chunk: accumulate whole sentences up to this length.
const TARGET_LEN: usize = 240;
/// First chunk only: a hard cap, plus flush as soon as it reaches
/// [`FIRST_CHUNK_MIN`] at a sentence boundary, so the first audio is heard
/// sooner instead of waiting on a full target-length chunk.
const FIRST_CHUNK_LEN: usize = 120;
/// First chunk only: don't flush before this length, to avoid opening with a
/// flat two-word fragment.
const FIRST_CHUNK_MIN: usize = 24;
/// Hard cap. Sentences longer than this are split at clause boundaries; this
/// model rushes and slurs on much longer inputs.
const MAX_LEN: usize = 300;

/// Where a chunk ends, which sets how long a pause follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    /// Mid-sentence, at a clause boundary (a long sentence we had to split).
    Clause,
    /// End of a full sentence.
    Sentence,
    /// End of a paragraph (or a heading).
    Paragraph,
}

/// One unit of streaming synthesis: text ready for the model plus the silence
/// (seconds) to play after its audio before the next chunk.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedChunk {
    pub text: String,
    pub gap_after: f32,
}

/// Plan how to split `raw` into chunks for streaming synthesis. `silence` is
/// the inter-paragraph pause in seconds; inter-sentence and inter-clause pauses
/// are derived from it. The final chunk always has `gap_after == 0.0`.
pub fn plan_chunks(raw: &str, silence: f32) -> Vec<PlannedChunk> {
    let clause_gap = silence * 0.18;
    let sentence_gap = silence * 0.5;
    let paragraph_gap = silence;

    let paragraphs = normalize_to_paragraphs(raw);
    let mut planned: Vec<(String, Boundary)> = Vec::new();
    let mut is_doc_start = true;

    for para in &paragraphs {
        let (marker, body) = split_list_marker(para);
        let mut sentences = split_sentences(&body);
        if let (Some(marker), Some(first)) = (marker, sentences.first_mut()) {
            // Keep an ordered-list number with the text it introduces, so the
            // model reads "One. Costs..." as one prosodic unit and the marker's
            // period never looks like a sentence boundary.
            *first = format!("{marker} {first}");
        }

        let mut para_chunks = build_paragraph_chunks(&sentences, is_doc_start);
        if let Some(last) = para_chunks.last_mut() {
            // Whatever it ended on, a paragraph's final chunk gets a paragraph
            // pause after it.
            last.1 = Boundary::Paragraph;
            is_doc_start = false;
        }
        planned.append(&mut para_chunks);
    }

    // Drop chunks with nothing speakable (punctuation-only or an orphan marker
    // like "-" or "1."), so we never spend an inference pass on them. Done
    // before the gap pass so the final-chunk gap stays correct.
    planned.retain(|(text, _)| text.chars().any(|c| c.is_alphanumeric()));

    let last_index = planned.len().saturating_sub(1);
    planned
        .into_iter()
        .enumerate()
        .map(|(i, (text, boundary))| {
            let gap_after = if i == last_index {
                0.0
            } else {
                match boundary {
                    Boundary::Clause => clause_gap,
                    Boundary::Sentence => sentence_gap,
                    Boundary::Paragraph => paragraph_gap,
                }
            };
            PlannedChunk { text, gap_after }
        })
        .collect()
}

/// Accumulate a paragraph's sentences into chunks no longer than the target,
/// flushing over-long sentences as their own clause-split chunks.
fn build_paragraph_chunks(sentences: &[String], doc_start: bool) -> Vec<(String, Boundary)> {
    let mut out: Vec<(String, Boundary)> = Vec::new();
    let mut current = String::new();
    // Only the very first chunk of the whole document uses the short limit.
    let mut first_pending = doc_start;

    let flush = |current: &mut String, out: &mut Vec<(String, Boundary)>| {
        let t = current.trim();
        if !t.is_empty() {
            out.push((t.to_string(), Boundary::Sentence));
        }
        current.clear();
    };

    for sentence in sentences {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }

        if char_len(sentence) > MAX_LEN {
            flush(&mut current, &mut out);
            first_pending = false;
            let parts = hard_split(sentence);
            let last = parts.len().saturating_sub(1);
            for (k, part) in parts.into_iter().enumerate() {
                let boundary = if k == last {
                    Boundary::Sentence
                } else {
                    Boundary::Clause
                };
                out.push((part, boundary));
            }
            continue;
        }

        let limit = if first_pending {
            FIRST_CHUNK_LEN
        } else {
            TARGET_LEN
        };
        if !current.is_empty() && char_len(&current) + 1 + char_len(sentence) > limit {
            flush(&mut current, &mut out);
            first_pending = false;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(sentence);
        // The first chunk flushes as soon as it clears the minimum, so the
        // listener hears the opener sooner; later chunks fill to the target.
        let flush_at = if first_pending {
            FIRST_CHUNK_MIN
        } else {
            TARGET_LEN
        };
        if char_len(&current) >= flush_at {
            flush(&mut current, &mut out);
            first_pending = false;
        }
    }
    flush(&mut current, &mut out);
    out
}

/// Split an over-long sentence into parts no longer than [`MAX_LEN`], cutting at
/// clause boundaries (comma / semicolon / colon) and, only as a last resort,
/// between words. Parts keep their trailing punctuation so the model reads them
/// with continuation rather than terminal intonation.
fn hard_split(sentence: &str) -> Vec<String> {
    let clauses = split_clauses(sentence);
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for clause in clauses {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }
        if char_len(clause) > MAX_LEN {
            if !current.is_empty() {
                out.push(current.trim().to_string());
                current.clear();
            }
            out.extend(split_words(clause));
            continue;
        }
        if !current.is_empty() && char_len(&current) + 1 + char_len(clause) > MAX_LEN {
            out.push(current.trim().to_string());
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(clause);
    }
    if !current.is_empty() {
        out.push(current.trim().to_string());
    }
    if out.is_empty() {
        out.push(sentence.trim().to_string());
    }
    out
}

/// Split a sentence after clause delimiters (`,` `;` `:`) followed by a space,
/// keeping the delimiter attached to the preceding clause.
fn split_clauses(sentence: &str) -> Vec<String> {
    let chars: Vec<char> = sentence.chars().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == ',' || c == ';' || c == ':') && i + 1 < chars.len() && chars[i + 1].is_whitespace()
        {
            let part: String = chars[start..=i].iter().collect();
            parts.push(part);
            start = i + 2;
            i += 2;
            continue;
        }
        i += 1;
    }
    if start < chars.len() {
        parts.push(chars[start..].iter().collect());
    }
    parts
}

/// Split a clause into word groups no longer than [`MAX_LEN`]. Last resort for
/// pathological comma-free text; each group is a hard cut with no punctuation.
fn split_words(clause: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in clause.split_whitespace() {
        // A single token with no whitespace can itself exceed the cap (a long
        // URL, a hash, or a CJK/agglutinative run with no spaces). Cut it on
        // character boundaries so the hard cap always holds.
        if char_len(word) > MAX_LEN {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            out.extend(split_chars(word));
            continue;
        }
        if !current.is_empty() && char_len(&current) + 1 + char_len(word) > MAX_LEN {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Cut a single over-long token into pieces of at most [`MAX_LEN`] characters,
/// slicing on character boundaries (never byte indices) so multibyte text is
/// never split mid-codepoint.
fn split_chars(token: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    chars
        .chunks(MAX_LEN)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

/// Abbreviations whose trailing period is not a sentence boundary.
const ABBREVIATIONS: &[&str] = &[
    "dr.", "mr.", "mrs.", "ms.", "prof.", "sr.", "jr.", "st.", "ave.", "rd.", "blvd.", "dept.",
    "inc.", "ltd.", "co.", "corp.", "etc.", "vs.", "no.", "i.e.", "e.g.", "ph.d.", "a.m.", "p.m.",
    "u.k.", "u.s.", "u.s.a.",
];

/// Split a single normalized paragraph into sentences, scanning by character so
/// we can avoid false boundaries that a naive `[.!?]\s` regex would hit:
/// abbreviations, decimals and clause references (`4.4`, `5.2.1`), and trailing
/// quotes/brackets after the terminal punctuation.
fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut i = 0;

    while i < n {
        let c = chars[i];
        if c == '.' || c == '!' || c == '?' {
            // Consume a run of terminal punctuation (e.g. "?!", "...").
            let mut j = i;
            while j < n && matches!(chars[j], '.' | '!' | '?') {
                j += 1;
            }
            // Consume closing quotes/brackets that belong to this sentence.
            let mut k = j;
            while k < n && matches!(chars[k], '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '»') {
                k += 1;
            }
            let at_boundary = k >= n || chars[k].is_whitespace();
            // A lone '.' between digits ("4.4") is a decimal, not a boundary.
            let is_decimal = j == i + 1
                && c == '.'
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && j < n
                && chars[j].is_ascii_digit();
            if at_boundary && !is_decimal && !ends_with_abbreviation(&chars[start..j]) {
                let sent: String = chars[start..k].iter().collect();
                let t = sent.trim();
                if !t.is_empty() {
                    sentences.push(t.to_string());
                }
                let mut m = k;
                while m < n && chars[m].is_whitespace() {
                    m += 1;
                }
                start = m;
                i = m;
                continue;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    if start < n {
        let t: String = chars[start..].iter().collect();
        let t = t.trim();
        if !t.is_empty() {
            sentences.push(t.to_string());
        }
    }
    if sentences.is_empty() {
        let t = text.trim();
        if t.is_empty() {
            Vec::new()
        } else {
            vec![t.to_string()]
        }
    } else {
        sentences
    }
}

/// Whether the text ending at a terminal period is a known abbreviation, by
/// inspecting the final whitespace-delimited token (including its period).
fn ends_with_abbreviation(slice: &[char]) -> bool {
    let token: String = slice
        .iter()
        .rev()
        .take_while(|c| !c.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let lower = token.to_ascii_lowercase();
    ABBREVIATIONS.contains(&lower.as_str())
}

/// Count Unicode scalar values; chunk limits are about spoken length, not bytes.
fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// If a paragraph begins with an ordered-list marker (`1.`, `2)`, ...), return
/// the marker and the remaining body; otherwise `(None, paragraph)`.
fn split_list_marker(para: &str) -> (Option<String>, String) {
    let re = Regex::new(r"^\s*(\d{1,3})[.)]\s+(.*)$").unwrap();
    if let Some(caps) = re.captures(para) {
        let num = caps.get(1).unwrap().as_str();
        let rest = caps.get(2).unwrap().as_str().to_string();
        return (Some(format!("{num}.")), rest);
    }
    (None, para.to_string())
}

/// Normalize a raw markdown document into a list of plain-text paragraphs.
/// Headings become their own paragraph (so they get a clean contour and a
/// paragraph pause), horizontal rules and fenced code blocks are dropped, and
/// inline markdown (emphasis, links, code) is reduced to its spoken text.
fn normalize_to_paragraphs(raw: &str) -> Vec<String> {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;

    let flush = |current: &mut String, paragraphs: &mut Vec<String>| {
        let collapsed = collapse_whitespace(current);
        if !collapsed.is_empty() {
            paragraphs.push(collapsed);
        }
        current.clear();
    };

    for raw_line in raw.lines() {
        let trimmed = raw_line.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if trimmed.is_empty() || is_horizontal_rule(trimmed) {
            flush(&mut current, &mut paragraphs);
            continue;
        }
        if let Some(heading) = heading_text(trimmed) {
            flush(&mut current, &mut paragraphs);
            let collapsed = collapse_whitespace(&inline_normalize(heading));
            if !collapsed.is_empty() {
                paragraphs.push(collapsed);
            }
            continue;
        }

        let line = strip_leading_bullet(trimmed);
        let normalized = inline_normalize(line);
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&normalized);
    }
    flush(&mut current, &mut paragraphs);
    paragraphs
}

/// Whether a line is a markdown horizontal rule (`---`, `***`, `___`).
fn is_horizontal_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
}

/// If a line is an ATX heading (`#`..`######` then a space), return its text
/// with the markers stripped.
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if rest.starts_with(' ') || rest.starts_with('\t') {
            return Some(rest.trim().trim_end_matches('#').trim());
        }
    }
    None
}

/// Strip a leading unordered-list bullet or blockquote marker. Ordered-list
/// markers are kept (handled later by [`split_list_marker`]).
fn strip_leading_bullet(line: &str) -> &str {
    for prefix in ["- ", "+ ", "> "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    line
}

/// Reduce inline markdown to spoken text: keep link/text, drop URLs and images,
/// remove emphasis and code markers.
fn inline_normalize(text: &str) -> String {
    let image = Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap();
    let link = Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap();
    let mut out = image.replace_all(text, "").to_string();
    out = link.replace_all(&out, "$1").to_string();
    for marker in ["**", "__", "*", "`", "~~"] {
        out = out.replace(marker, "");
    }
    out
}

/// Collapse runs of whitespace into single spaces and trim.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(chunks: &[PlannedChunk]) -> Vec<&str> {
        chunks.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn splits_plain_sentences() {
        assert_eq!(
            split_sentences("First sentence. Second sentence. Third one."),
            vec!["First sentence.", "Second sentence.", "Third one."]
        );
    }

    #[test]
    fn single_giant_token_is_capped() {
        // A whitespace-free token longer than MAX_LEN (e.g. a URL) must still be
        // cut so no chunk exceeds the hard cap.
        let chunks = plan_chunks(&("x".repeat(900) + "."), 0.3);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(
                c.text.chars().count() <= MAX_LEN,
                "chunk over cap: {}",
                c.text.chars().count()
            );
        }
    }

    #[test]
    fn caps_multibyte_token_on_char_boundaries() {
        // CJK text with no ASCII spaces: must cut on char boundaries (never
        // panic on a byte-index slice) and stay within the cap.
        let chunks = plan_chunks(&"図書館".repeat(150), 0.3);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.text.chars().count() <= MAX_LEN);
        }
    }

    #[test]
    fn drops_chunks_with_no_speakable_content() {
        // Punctuation-only / orphan-marker inputs (no alphanumeric) yield no
        // chunks rather than wasting an inference pass.
        for raw in ["...", "!!!", ". . .", "#", "- ", ">", "***"] {
            assert!(
                plan_chunks(raw, 0.3).is_empty(),
                "expected no chunks for {raw:?}"
            );
        }
        // Content with any alphanumeric survives (a lone "1." reads as "one").
        assert!(!plan_chunks("1.", 0.3).is_empty());
        assert_eq!(
            texts(&plan_chunks("Hello world.", 0.3)),
            vec!["Hello world."]
        );
    }

    #[test]
    fn does_not_split_on_decimals_or_clause_references() {
        // The periods inside 4.4 and 5.2.1 are not sentence boundaries; only
        // the period before "Thanks" is.
        assert_eq!(
            split_sentences("Confirm clause 4.4 and 5.2.1 apply here. Thanks."),
            vec!["Confirm clause 4.4 and 5.2.1 apply here.", "Thanks."]
        );
    }

    #[test]
    fn does_not_split_on_abbreviations() {
        assert_eq!(
            split_sentences("Dr. Smith and Mr. Jones met at 9 a.m. sharp."),
            vec!["Dr. Smith and Mr. Jones met at 9 a.m. sharp."]
        );
    }

    #[test]
    fn keeps_trailing_quote_with_its_sentence() {
        assert_eq!(
            split_sentences("She said \"hello.\" Then she left."),
            vec!["She said \"hello.\"", "Then she left."]
        );
    }

    #[test]
    fn strips_markdown_emphasis_and_headings() {
        let raw = "# Title Here\n\nWe are happy to **accept** the `terms`.";
        let chunks = plan_chunks(raw, 0.3);
        assert_eq!(
            texts(&chunks),
            vec!["Title Here", "We are happy to accept the terms."]
        );
    }

    #[test]
    fn horizontal_rule_is_a_paragraph_break_not_spoken() {
        let raw = "One.\n\n---\n\nTwo.";
        let chunks = plan_chunks(raw, 0.3);
        assert_eq!(texts(&chunks), vec!["One.", "Two."]);
    }

    #[test]
    fn keeps_ordered_list_number_and_does_not_split_it() {
        let raw = "1. Costs are confirmed and limited as agreed.";
        let chunks = plan_chunks(raw, 0.3);
        // The "1." stays attached to the sentence rather than becoming its own
        // chunk.
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].text,
            "1. Costs are confirmed and limited as agreed."
        );
    }

    #[test]
    fn strips_unordered_bullets_and_links() {
        let raw = "- See [the docs](https://example.com) for details.";
        let chunks = plan_chunks(raw, 0.3);
        assert_eq!(texts(&chunks), vec!["See the docs for details."]);
    }

    #[test]
    fn hard_splits_overlong_sentence_at_clause_boundaries() {
        let long = format!(
            "This clause confirms {}, and it also confirms {} for completeness.",
            "a".repeat(200),
            "b".repeat(200)
        );
        let chunks = plan_chunks(&long, 0.3);
        assert!(chunks.len() >= 2, "expected a long sentence to be split");
        for c in &chunks {
            assert!(
                c.text.chars().count() <= MAX_LEN,
                "chunk exceeded MAX_LEN: {}",
                c.text
            );
        }
        // The split lands at the clause boundary: the first part ends with a
        // comma (continuation), not the terminal period.
        assert!(chunks[0].text.ends_with(','));
    }

    #[test]
    fn first_chunk_is_short_for_low_latency() {
        let raw = "Short opener. This is the second sentence that follows on. \
                   And here is a third sentence that keeps going for a while.";
        let chunks = plan_chunks(raw, 0.3);
        // The opener flushes early rather than being merged into one big chunk,
        // and stays under the first-chunk cap so first audio comes sooner.
        assert!(
            chunks.len() >= 2,
            "first chunk should flush early, got {chunks:?}"
        );
        assert!(chunks[0].text.chars().count() <= FIRST_CHUNK_LEN);
        assert!(chunks[0].text.starts_with("Short opener."));
    }

    #[test]
    fn tiny_first_sentence_merges_to_avoid_a_fragment() {
        // A 2-word opener is below the minimum, so it pulls in the next
        // sentence rather than being synthesized as a flat fragment.
        let chunks = plan_chunks("Hi there. Welcome to the full briefing today.", 0.3);
        assert_eq!(
            chunks[0].text,
            "Hi there. Welcome to the full briefing today."
        );
    }

    #[test]
    fn gaps_scale_with_boundary_kind() {
        let raw = "First para sentence. Another sentence here.\n\nSecond paragraph here.";
        let chunks = plan_chunks(raw, 0.3);
        // Two paragraphs -> the boundary between them is a paragraph gap, and
        // the final chunk has no trailing gap.
        assert_eq!(chunks.len(), 2);
        assert!((chunks[0].gap_after - 0.3).abs() < 1e-6);
        assert_eq!(chunks[1].gap_after, 0.0);
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(plan_chunks("   \n\n  ", 0.3).is_empty());
    }

    #[test]
    fn drops_fenced_code_blocks() {
        let raw = "Before.\n\n```\nlet x = 1;\n```\n\nAfter.";
        let chunks = plan_chunks(raw, 0.3);
        assert_eq!(texts(&chunks), vec!["Before.", "After."]);
    }
}

#[cfg(test)]
mod adversarial_probe {
    use super::*;

    #[test]
    fn probe_zero_chunk_cases() {
        let fence = plan_chunks("```\nlet x = 1;\n```", 0.3);
        let rule = plan_chunks("---", 0.3);
        let img = plan_chunks("![img](x.png)", 0.3);
        let stars = plan_chunks("***", 0.3);
        eprintln!("FENCE_LEN={}", fence.len());
        eprintln!("RULE_LEN={}", rule.len());
        eprintln!("IMG_LEN={}", img.len());
        eprintln!("STARS_LEN={}", stars.len());
    }
}
