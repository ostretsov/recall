//! Packing speech for Whisper and mapping its times back.
//!
//! A call track is mostly silence (the other person talks).  Voice activity
//! detection finds the speech; the pieces are packed back to back into
//! windows of at most [`WINDOW`] seconds (Whisper's input is 30 s), so no
//! time is spent on silence.  Whisper's token times inside a window are then
//! mapped back to the original recording, and the text is split where the
//! original had a real pause.

/// Longest packed window (Whisper works on 30 s).
pub const WINDOW: f64 = 28.0;
/// Silence put between packed pieces, so Whisper hears a break.
pub const GAP: f64 = 0.2;
/// Longer silence between pieces that were separate turns (paused for
/// [`SPLIT`] or more): Whisper then ends its segment there, and token times
/// near the turn change are clearly on one side.
pub const TURN_GAP: f64 = 2.0;
/// Speech pieces closer than this are joined before packing.
pub const JOIN: f64 = 0.5;
/// A pause this long in the original starts a new transcript line.
pub const SPLIT: f64 = 1.5;

/// One stretch of speech: `start` in the original track, placed at `packed`
/// in its window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub len: f64,
    pub packed: f64,
}

/// Speech pieces packed into one Whisper input.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Window {
    pub pieces: Vec<Piece>,
}

impl Window {
    /// Length of the packed audio in seconds.
    pub fn len(&self) -> f64 {
        self.pieces.last().map_or(0.0, |p| p.packed + p.len)
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Index of the piece a packed time belongs to (a time in a gap belongs
    /// to the next piece).
    pub fn piece_at(&self, packed: f64) -> usize {
        self.pieces
            .iter()
            .position(|p| packed < p.packed + p.len)
            .unwrap_or(self.pieces.len().saturating_sub(1))
    }

    /// Original time of a packed time.
    pub fn original(&self, packed: f64) -> f64 {
        let Some(p) = self.pieces.get(self.piece_at(packed)) else {
            return packed;
        };
        p.start + (packed - p.packed).clamp(0.0, p.len)
    }

    /// The packed audio: `rate` samples per second of `track`.
    pub fn samples(&self, track: &[f32], rate: u32) -> Vec<f32> {
        let r = rate as f64;
        let mut out = vec![0.0; (self.len() * r).ceil() as usize];
        for p in &self.pieces {
            let from = ((p.start * r) as usize).min(track.len());
            let to = (((p.start + p.len) * r) as usize).min(track.len());
            let at = (p.packed * r) as usize;
            let n = (to - from).min(out.len().saturating_sub(at));
            out[at..at + n].copy_from_slice(&track[from..from + n]);
        }
        out
    }
}

/// Packs speech regions (`(start, end)` seconds, sorted) into windows.
/// Regions longer than a window are cut.
pub fn pack(regions: &[(f64, f64)]) -> Vec<Window> {
    // Join regions separated by tiny pauses (one breath is not a turn).
    let mut joined: Vec<(f64, f64)> = Vec::new();
    for &(s, e) in regions {
        if e <= s {
            continue;
        }
        match joined.last_mut() {
            Some(last) if s - last.1 < JOIN && e - last.0 <= WINDOW => last.1 = last.1.max(e),
            _ => joined.push((s, e)),
        }
    }
    let mut windows = Vec::new();
    let mut cur = Window::default();
    let mut prev_end: Option<f64> = None;
    for (mut s, e) in joined {
        let gap = match prev_end {
            Some(p) if s - p >= SPLIT => TURN_GAP,
            _ => GAP,
        };
        prev_end = Some(e);
        while s < e {
            let len = (e - s).min(WINDOW);
            let at = if cur.is_empty() { 0.0 } else { cur.len() + gap };
            if at + len > WINDOW && !cur.is_empty() {
                windows.push(std::mem::take(&mut cur));
                continue;
            }
            cur.pieces.push(Piece {
                start: s,
                len,
                packed: at,
            });
            s += len;
        }
    }
    if !cur.is_empty() {
        windows.push(cur);
    }
    windows
}

/// A recognized token: its text bytes and packed start/end time.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub bytes: Vec<u8>,
    pub t0: f64,
    pub t1: f64,
}

/// A transcript line in original time.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Turn number of each piece: pieces separated by less than [`SPLIT`]
/// belong to the same turn.
pub fn turns(window: &Window) -> Vec<usize> {
    let mut out = Vec::with_capacity(window.pieces.len());
    let mut turn = 0;
    for (i, p) in window.pieces.iter().enumerate() {
        if i > 0 {
            let prev = window.pieces[i - 1];
            if p.start - (prev.start + prev.len) >= SPLIT {
                turn += 1;
            }
        }
        out.push(turn);
    }
    out
}

/// Turns one Whisper segment's tokens into lines in original time: one
/// line per turn of the original recording.
///
/// Token times (even DTW ones) can be off by a word or two, so a segment
/// spanning two turns is not cut exactly where the times change but at the
/// word boundary — preferably a sentence end — that agrees best with them.
pub fn lines(window: &Window, tokens: &[Token]) -> Vec<Line> {
    if tokens.is_empty() || window.is_empty() {
        return Vec::new();
    }
    let piece_turn = turns(window);
    let turn_of: Vec<usize> = tokens
        .iter()
        .map(|t| piece_turn[window.piece_at((t.t0 + t.t1) / 2.0)])
        .collect();
    let mut out = Vec::new();
    split(window, &piece_turn, tokens, &turn_of, &mut out);
    out
}

fn split(
    window: &Window,
    piece_turn: &[usize],
    tokens: &[Token],
    turn_of: &[usize],
    out: &mut Vec<Line>,
) {
    let first = turn_of.iter().copied().min().unwrap_or(0);
    let last = turn_of.iter().copied().max().unwrap_or(0);
    if first != last {
        // Best cut between `first` and the later turns.
        let mut best: Option<(f64, usize)> = None;
        for i in 1..tokens.len() {
            if tokens[i].bytes.first() != Some(&b' ') {
                continue; // Only between words.
            }
            let wrong = turn_of[..i].iter().filter(|&&t| t != first).count()
                + turn_of[i..].iter().filter(|&&t| t == first).count();
            let sentence_end = tokens[i - 1]
                .bytes
                .last()
                .is_some_and(|b| matches!(b, b'.' | b'?' | b'!' | b':'));
            let cost = wrong as f64 + if sentence_end { 0.0 } else { 3.5 };
            if best.is_none_or(|(c, _)| cost < c) {
                best = Some((cost, i));
            }
        }
        // Keeping it whole costs the tokens outside the main turn.
        let mut counts = std::collections::BTreeMap::new();
        for t in turn_of {
            *counts.entry(*t).or_insert(0usize) += 1;
        }
        let whole = (turn_of.len() - counts.values().max().copied().unwrap_or(0)) as f64;
        if let Some((cost, i)) = best
            && cost < whole
        {
            split(window, piece_turn, &tokens[..i], &turn_of[..i], out);
            split(window, piece_turn, &tokens[i..], &turn_of[i..], out);
            return;
        }
    }
    line(window, piece_turn, tokens, turn_of, out);
}

/// One line from tokens that belong to one turn (the majority's).
fn line(
    window: &Window,
    piece_turn: &[usize],
    tokens: &[Token],
    turn_of: &[usize],
    out: &mut Vec<Line>,
) {
    let bytes: Vec<u8> = tokens
        .iter()
        .flat_map(|t| t.bytes.iter().copied())
        .collect();
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    if text.is_empty() {
        return;
    }
    let mut counts = std::collections::BTreeMap::new();
    for t in turn_of {
        *counts.entry(*t).or_insert(0) += 1;
    }
    let turn = counts
        .iter()
        .max_by_key(|(t, n)| (**n, std::cmp::Reverse(**t)))
        .map_or(0, |(t, _)| *t);
    // The turn's extent in the original recording.
    let pieces: Vec<&Piece> = window
        .pieces
        .iter()
        .zip(piece_turn)
        .filter(|(_, t)| **t == turn)
        .map(|(p, _)| p)
        .collect();
    let (lo, hi) = match (pieces.first(), pieces.last()) {
        (Some(a), Some(b)) => (a.start, b.start + b.len),
        _ => return,
    };
    let start = window.original(tokens[0].t0).clamp(lo, hi);
    let end = window
        .original(tokens[tokens.len() - 1].t1)
        .clamp(start, hi);
    out.push(Line { start, end, text });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, t0: f64, t1: f64) -> Token {
        Token {
            bytes: text.as_bytes().to_vec(),
            t0,
            t1,
        }
    }

    #[test]
    fn packs_speech_back_to_back() {
        let w = pack(&[(4.0, 6.0), (20.0, 25.0), (40.0, 41.0)]);
        assert_eq!(w.len(), 1);
        let p = &w[0].pieces;
        assert_eq!(
            p[0],
            Piece {
                start: 4.0,
                len: 2.0,
                packed: 0.0
            }
        );
        assert_eq!(
            p[1],
            Piece {
                start: 20.0,
                len: 5.0,
                packed: 4.0
            }
        );
        assert!((p[2].packed - 11.0).abs() < 1e-9);
        assert!((w[0].len() - 12.0).abs() < 1e-9);
        // A short pause (1 s) gets the short gap.
        let w = pack(&[(0.0, 2.0), (3.0, 4.0)]);
        assert!((w[0].pieces[1].packed - 2.2).abs() < 1e-9);
    }

    #[test]
    fn joins_breaths_and_splits_windows() {
        let w = pack(&[(0.0, 1.0), (1.3, 2.0)]);
        assert_eq!(w[0].pieces.len(), 1, "0.3 s pause joined");
        // 20 s + 20 s don't fit in one window.
        let w = pack(&[(0.0, 20.0), (30.0, 50.0)]);
        assert_eq!(w.len(), 2);
        assert_eq!(w[1].pieces[0].packed, 0.0);
        // A 70 s monologue is cut into windows.
        let w = pack(&[(10.0, 80.0)]);
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].pieces[0].start, 38.0);
        assert!(w.iter().all(|x| x.len() <= WINDOW + 1e-9));
        assert!(pack(&[]).is_empty());
        assert!(pack(&[(5.0, 5.0)]).is_empty());
    }

    #[test]
    fn maps_times_back() {
        let w = &pack(&[(4.0, 6.0), (20.0, 25.0)])[0];
        assert_eq!(w.original(0.0), 4.0);
        assert_eq!(w.original(1.5), 5.5);
        // In the gap between pieces: start of the next piece.
        assert_eq!(w.original(3.0), 20.0);
        assert!((w.original(5.0) - 21.0).abs() < 1e-9);
        assert_eq!(w.original(99.0), 25.0);
    }

    #[test]
    fn packed_samples() {
        let track: Vec<f32> = (0..100).map(|i| i as f32).collect();
        // rate 10: pieces 1.0-2.0 s and 3.0-4.0 s (short pause: short gap).
        let w = &pack(&[(1.0, 2.0), (3.0, 4.0)])[0];
        let s = w.samples(&track, 10);
        assert_eq!(s.len(), 22);
        assert_eq!(&s[0..10], &track[10..20]);
        assert_eq!(&s[10..12], &[0.0, 0.0]);
        assert_eq!(&s[12..22], &track[30..40]);
    }

    #[test]
    fn lines_split_at_real_pauses() {
        // "Hello there." at 4 s, then (after 14 s) "Next topic."
        let w = &pack(&[(4.0, 6.0), (20.0, 22.0)])[0];
        let toks = [
            tok(" Hello", 0.0, 0.5),
            tok(" there.", 0.5, 1.8),
            tok(" Next", 4.1, 4.7),
            tok(" topic.", 4.7, 5.8),
        ];
        let l = lines(w, &toks);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].text, "Hello there.");
        assert_eq!((l[0].start, l[0].end), (4.0, 5.8));
        assert_eq!(l[1].text, "Next topic.");
        assert!((l[1].start - 20.1).abs() < 1e-9);
    }

    #[test]
    fn cut_lands_on_the_sentence_end_despite_drifting_times() {
        // Turn 1 "Yes, of course." (0-3 s), turn 2 "It is because of the
        // campaign." (20-24 s).  The time of "It" drifts into turn 1.
        let w = &pack(&[(0.0, 3.0), (20.0, 24.0)])[0];
        let toks = [
            tok(" Yes,", 0.2, 0.6),
            tok(" of", 0.6, 1.0),
            tok(" course.", 1.0, 2.0),
            tok(" It", 2.8, 2.95),
            tok(" is", 4.2, 4.4),
            tok(" because", 4.4, 5.0),
            tok(" of", 5.2, 5.4),
            tok(" the", 5.4, 5.6),
            tok(" campaign.", 5.6, 6.5),
        ];
        let l = lines(w, &toks);
        let texts: Vec<&str> = l.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, ["Yes, of course.", "It is because of the campaign."]);
        assert_eq!(l[1].start, 20.0, "clamped into its turn");
    }

    #[test]
    fn turn_numbers() {
        let w = &pack(&[(0.0, 1.0), (1.5, 2.0), (5.0, 6.0)])[0];
        assert_eq!(turns(w), vec![0, 0, 1]);
    }

    #[test]
    fn short_pauses_and_split_utf8_stay_together() {
        // Two pieces 1 s apart in the original: one line.
        let w = &pack(&[(0.0, 1.0), (2.0, 3.0)])[0];
        let p = "привет".as_bytes();
        let toks = [
            Token {
                bytes: [b" ", &p[..3]].concat(),
                t0: 0.0,
                t1: 0.4,
            },
            Token {
                bytes: p[3..].to_vec(),
                t0: 0.4,
                t1: 0.9,
            },
            tok(" мир", 1.3, 2.0),
        ];
        let l = lines(w, &toks);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].text, "привет мир");
    }
}
