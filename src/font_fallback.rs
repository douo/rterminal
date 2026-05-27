use std::sync::OnceLock;

const MAX_AUTO_FALLBACKS: usize = 32;

const PUA_PROBES: &[char] = &[
    '\u{e0a0}', // Powerline branch
    '\u{e0b0}', // Powerline separator
    '\u{e700}', // Devicons range
    '\u{f000}', '\u{f013}', '\u{f07b}', '\u{f120}', '\u{f1c0}', '\u{f303}', '\u{f489}', '\u{f4d3}',
];

const EMOJI_PROBES: &[char] = &['\u{1f4c1}', '\u{1f600}', '\u{1f680}', '\u{2764}'];

const SYMBOL_PROBES: &[char] = &[
    '\u{2713}', '\u{26a0}', '\u{2190}', '\u{2192}', '\u{2318}', '\u{25cf}', '\u{25cb}',
];

#[derive(Clone, Debug)]
struct FontCandidate {
    family: String,
    score: i32,
    tie_breaker: i32,
}

pub(crate) fn font_fallback_families(raw: &[String]) -> Vec<String> {
    merge_font_fallback_families(raw, terminal_fallback_families())
}

fn terminal_fallback_families() -> Vec<String> {
    static FALLBACKS: OnceLock<Vec<String>> = OnceLock::new();
    FALLBACKS
        .get_or_init(discover_terminal_fallback_families)
        .clone()
}

fn discover_terminal_fallback_families() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    terminal_fallback_families_from_db(&db)
}

fn terminal_fallback_families_from_db(db: &fontdb::Database) -> Vec<String> {
    let mut candidates = Vec::new();
    for face in db.faces() {
        let Some(family) = preferred_family_name(face) else {
            continue;
        };
        let Some((coverage_score, coverage_tie_breaker)) = glyph_coverage_score(db, face.id) else {
            continue;
        };
        let score = family_name_score(&family, face.monospaced) + coverage_score;
        if score < 120 {
            continue;
        }

        upsert_candidate(
            &mut candidates,
            FontCandidate {
                family,
                score,
                tie_breaker: coverage_tie_breaker + face_style_tie_breaker(face),
            },
        );
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.tie_breaker.cmp(&left.tie_breaker))
            .then_with(|| left.family.cmp(&right.family))
    });
    candidates
        .into_iter()
        .take(MAX_AUTO_FALLBACKS)
        .map(|candidate| candidate.family)
        .collect()
}

pub(crate) fn merge_font_fallback_families<I>(raw: &[String], auto: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut fallbacks = Vec::new();
    for font in raw
        .iter()
        .map(|font| font.trim())
        .filter(|font| !font.is_empty())
    {
        push_unique_family(&mut fallbacks, font);
    }
    for font in auto {
        push_unique_family(&mut fallbacks, font.trim());
    }
    fallbacks
}

fn preferred_family_name(face: &fontdb::FaceInfo) -> Option<String> {
    face.families
        .iter()
        .map(|(family, _)| family.trim())
        .find(|family| !family.is_empty())
        .map(str::to_string)
}

fn upsert_candidate(candidates: &mut Vec<FontCandidate>, candidate: FontCandidate) {
    if let Some(existing) = candidates
        .iter_mut()
        .find(|existing| existing.family.eq_ignore_ascii_case(&candidate.family))
    {
        if (candidate.score, candidate.tie_breaker) > (existing.score, existing.tie_breaker) {
            *existing = candidate;
        }
    } else {
        candidates.push(candidate);
    }
}

fn push_unique_family(fallbacks: &mut Vec<String>, family: &str) {
    if !fallbacks
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(family))
    {
        fallbacks.push(family.to_string());
    }
}

fn family_name_score(family: &str, monospaced: bool) -> i32 {
    let normalized = family.to_ascii_lowercase();
    let mut score = 0;

    if normalized.contains("nerd font") {
        score += 420;
    }
    if normalized.contains("powerline") {
        score += 360;
    }
    if normalized.contains("codicon")
        || normalized.contains("octicon")
        || normalized.contains("devicon")
        || normalized.contains("font awesome")
        || normalized.contains("material icon")
    {
        score += 320;
    }
    if normalized.contains("emoji") {
        score += 300;
    }
    if normalized.contains("sf symbols") {
        score += 280;
    } else if normalized.contains("symbol") {
        score += 220;
    }
    if normalized.contains("mono") || normalized.contains("monospace") {
        score += 70;
    }
    if monospaced {
        score += 40;
    }

    score
}

fn glyph_coverage_score(db: &fontdb::Database, id: fontdb::ID) -> Option<(i32, i32)> {
    db.with_face_data(id, |data, face_index| {
        let face = ttf_parser::Face::parse(data, face_index).ok()?;
        let pua_hits = count_glyph_hits(&face, PUA_PROBES);
        let emoji_hits = count_glyph_hits(&face, EMOJI_PROBES);
        let symbol_hits = count_glyph_hits(&face, SYMBOL_PROBES);

        if pua_hits == 0 && emoji_hits == 0 && symbol_hits < 3 {
            return None;
        }

        let score = pua_hits * 90 + emoji_hits * 70 + symbol_hits * 20;
        let tie_breaker = pua_hits * 100 + emoji_hits * 20 + symbol_hits;
        Some((score, tie_breaker))
    })?
}

fn count_glyph_hits(face: &ttf_parser::Face<'_>, probes: &[char]) -> i32 {
    probes
        .iter()
        .filter(|probe| face.glyph_index(**probe).is_some())
        .count() as i32
}

fn face_style_tie_breaker(face: &fontdb::FaceInfo) -> i32 {
    let mut score = 0;
    if face.style == fontdb::Style::Normal {
        score += 20;
    }
    score -= (i32::from(face.weight.0) - i32::from(fontdb::Weight::NORMAL.0)).abs() / 50;
    if face.monospaced {
        score += 10;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::{family_name_score, merge_font_fallback_families};

    #[test]
    fn merge_font_fallback_families_keeps_user_fonts_first_and_dedupes() {
        let raw = vec![
            " Hack Nerd Font Mono ".to_string(),
            "Apple Color Emoji".to_string(),
            "hack nerd font mono".to_string(),
        ];
        let merged = merge_font_fallback_families(
            &raw,
            vec![
                "Apple Color Emoji".to_string(),
                "Symbols Nerd Font Mono".to_string(),
            ],
        );

        assert_eq!(
            merged,
            vec![
                "Hack Nerd Font Mono".to_string(),
                "Apple Color Emoji".to_string(),
                "Symbols Nerd Font Mono".to_string(),
            ]
        );
    }

    #[test]
    fn family_name_score_prioritizes_terminal_symbol_fonts() {
        assert!(
            family_name_score("JetBrainsMono Nerd Font Mono", true)
                > family_name_score("Helvetica", false)
        );
        assert!(family_name_score("Apple Color Emoji", false) > family_name_score("Arial", false));
        assert!(family_name_score("SF Symbols", false) > family_name_score("Times", false));
    }
}
