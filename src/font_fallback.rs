use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_AUTO_FALLBACKS: usize = 32;

static DISCOVERED_FALLBACKS: OnceLock<Vec<String>> = OnceLock::new();
static SCAN_SPAWNED: AtomicBool = AtomicBool::new(false);

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

const CJK_TEXT_PROBES: &[char] = &[
    '\u{4e2d}', // CJK ideograph: 中
    '\u{6587}', // CJK ideograph: 文
    '\u{4f60}', // CJK ideograph: 你
    '\u{597d}', // CJK ideograph: 好
    '\u{570b}', // CJK ideograph: 國
    '\u{6f22}', // CJK ideograph: 漢
    '\u{3042}', // Hiragana: あ
    '\u{30a2}', // Katakana: ア
    '\u{ac00}', // Hangul: 가
    '\u{d55c}', // Hangul: 한
];

#[derive(Clone, Debug)]
struct FontCandidate {
    family: String,
    score: i32,
    tie_breaker: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GlyphCoverageHits {
    pua: i32,
    emoji: i32,
    symbol: i32,
    cjk_text: i32,
}

pub(crate) fn font_fallback_families(raw: &[String]) -> Vec<String> {
    merge_font_fallback_families(raw, terminal_fallback_families())
}

/// 系统字体全量扫描（`ttf_parser` 逐面解析）在字体多的机器上要数百 ms。
/// 首窗口创建路径不等它（PERF-2）：先返回保守回退表，扫描在后台线程做，
/// 完成后由终端侧的周期任务检测 [`background_scan_complete`] 并换装完整表。
fn terminal_fallback_families() -> Vec<String> {
    if let Some(discovered) = DISCOVERED_FALLBACKS.get() {
        return discovered.clone();
    }

    if !SCAN_SPAWNED.swap(true, Ordering::SeqCst) {
        let _ = std::thread::Builder::new()
            .name("font-fallback-scan".to_string())
            .spawn(|| {
                let _ = DISCOVERED_FALLBACKS.set(discover_terminal_fallback_families());
            });
    }

    conservative_fallback_families()
}

/// 后台扫描是否已完成（完成后 [`font_fallback_families`] 返回完整表）。
pub(crate) fn background_scan_complete() -> bool {
    DISCOVERED_FALLBACKS.get().is_some()
}

/// macOS 必装字体，覆盖 CJK / emoji / 常用符号：扫描完成前的保守回退。
fn conservative_fallback_families() -> Vec<String> {
    [
        "PingFang SC",
        "Hiragino Sans",
        "Apple Color Emoji",
        "Apple Symbols",
        "Menlo",
    ]
    .map(String::from)
    .to_vec()
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
        // 点前缀是 macOS 私有字体（".Apple Color Emoji UI"、".SF NS Mono"…）：
        // CoreText 拒绝按名解析这类字体并回退到 TimesNewRoman，放进 fallback
        // 表只会产生一堆警告加一个错误的兜底字体。
        if family.starts_with('.') {
            continue;
        }
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
    // 优先取英文家族名（DSP-14）：`families[0]` 在非英文 locale 下可能是本地化
    // 家族名（如「苹方-简」），交给 GPUI/CoreText 按名字解析存在失配风险；
    // 英文名（PingFang SC）才是稳定标识。没有英文名时再退回第一个非空名。
    face.families
        .iter()
        .find(|(family, language)| {
            *language == fontdb::Language::English_UnitedStates && !family.trim().is_empty()
        })
        .or_else(|| {
            face.families
                .iter()
                .find(|(family, _)| !family.trim().is_empty())
        })
        .map(|(family, _)| family.trim().to_string())
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
        terminal_symbol_coverage_score(GlyphCoverageHits {
            pua: count_glyph_hits(&face, PUA_PROBES),
            emoji: count_glyph_hits(&face, EMOJI_PROBES),
            symbol: count_glyph_hits(&face, SYMBOL_PROBES),
            cjk_text: count_glyph_hits(&face, CJK_TEXT_PROBES),
        })
    })?
}

fn terminal_symbol_coverage_score(hits: GlyphCoverageHits) -> Option<(i32, i32)> {
    if hits.cjk_text > 0 || (hits.pua == 0 && hits.emoji == 0 && hits.symbol < 3) {
        return None;
    }

    let score = hits.pua * 90 + hits.emoji * 70 + hits.symbol * 20;
    let tie_breaker = hits.pua * 100 + hits.emoji * 20 + hits.symbol;
    Some((score, tie_breaker))
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
    use super::{
        GlyphCoverageHits, family_name_score, merge_font_fallback_families,
        terminal_symbol_coverage_score,
    };

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

    #[test]
    fn terminal_symbol_coverage_score_rejects_cjk_text_fonts() {
        let cjk_font_with_symbol_coverage = GlyphCoverageHits {
            pua: 0,
            emoji: 0,
            symbol: 7,
            cjk_text: 1,
        };

        assert_eq!(
            terminal_symbol_coverage_score(cjk_font_with_symbol_coverage),
            None
        );
    }

    #[test]
    fn terminal_symbol_coverage_score_keeps_symbol_only_fonts() {
        let symbol_font = GlyphCoverageHits {
            pua: 1,
            emoji: 0,
            symbol: 2,
            cjk_text: 0,
        };

        assert_eq!(
            terminal_symbol_coverage_score(symbol_font),
            Some((130, 102))
        );
    }
}
