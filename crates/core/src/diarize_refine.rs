//! 语言线索纠偏:短应答继承 / 回合约束 / 称呼反推。
//!
//! diarization 模型只看声学特征,短应答与簇跳变是它的弱项;这三条规则用
//! 转写文本的时间/内容线索纠偏,在 diarize 之后、命名之前跑(纯函数 + 单测):
//! 1. 短应答继承:≤1.5s 且 ≤4 字的应答词("嗯/好/对/OK…")继承最近的前一发言者
//!    (优先同采集来源)——这类段落 diarization 最容易标错簇;
//! 2. 回合约束:同来源里 <1s 的孤立短 run,前后是同一其他簇 → 并入前后簇;
//! 3. 称呼反推:段首称呼("张总，…")记录 (称呼, 发言者簇) 约束——喊"张总"
//!    的簇不可能是"张总",命名阶段用于过滤声纹预填名。

use crate::db::Segment;

/// 纠偏结果:改动的 (idx, 新簇键) + 称呼约束 (称呼名, 发言者簇键)
pub struct RefineReport {
    /// 需落库的说话人改动:(segments.idx, 新簇键)
    pub changes: Vec<(i64, String)>,
    pub inherited: usize,
    pub flipped: usize,
    /// 称呼约束:(称呼名, 发言者簇键)——喊这个称呼的簇不可能是这个名字
    pub vocatives: Vec<(String, String)>,
}

/// 应答词表(精确匹配,≤4 字)
const ACK_WORDS: &[&str] = &[
    "嗯", "嗯嗯", "嗯呢", "啊", "哦", "噢", "喔", "哎", "诶",
    "好", "好的", "好吧", "好嘞", "好滴", "行", "行吧", "对", "对的",
    "是", "是的", "要", "要的", "不用", "没有", "没问题", "明白", "了解", "收到",
    "ok", "okay", "yes", "yeah", "yep", "no", "nope", "sure", "right",
];

/// 称谓后缀(段首"N+称谓+标点"识别;单字取最后一位,不含易误报的单字"工")
const TITLE_SUFFIXES: &[char] = &[
    '总', '经', '理', '师', '哥', '姐', '任', '长', '授', '董', '博',
];

fn is_ack(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    !t.is_empty() && t.chars().count() <= 4 && ACK_WORDS.contains(&t.as_str())
}

/// 段首称呼识别:"张总，…" → Some("张总");要求称呼后跟标点/空格
fn parse_vocative(text: &str) -> Option<String> {
    let chars: Vec<char> = text.trim_start().chars().take(6).collect();
    if chars.is_empty() {
        return None;
    }
    for pos in 1..chars.len().min(5) {
        let c = chars[pos];
        if !TITLE_SUFFIXES.contains(&c) {
            continue;
        }
        // 名字部分必须全是 CJK/字母
        let name_ok = chars[..=pos].iter().all(|ch| {
            ch.is_alphabetic() && !ch.is_ascii_punctuation()
        });
        if !name_ok {
            continue;
        }
        // 称谓后必须跟标点/空格(避免"工程""经理部"等词中误报)
        let next = chars.get(pos + 1);
        if next.map(|n| matches!(n, '，' | ',' | '。' | '.' | '：' | ':' | '、' | ' ' | '！' | '!'))
            .unwrap_or(false)
        {
            return Some(chars[..=pos].iter().collect());
        }
    }
    None
}

/// 对已按时间排序、已标注 speaker 的段落跑三条规则;
/// 返回需要落库的改动与称呼约束(不直接写库,便于测试与调用方控制)。
pub fn refine(segs: &[Segment]) -> RefineReport {
    let mut inherited = 0;
    let mut flipped = 0;
    let mut vocatives = Vec::new();
    let mut changes: Vec<(i64, String)> = Vec::new();

    for (i, seg) in segs.iter().enumerate() {
        let Some(cur) = seg.speaker.clone() else { continue };
        let dur = seg.end_ms - seg.start_ms;

        // 规则 1:短应答继承
        if dur <= 1500 && seg.text.chars().count() <= 4 && is_ack(&seg.text) {
            // 最近的前一发言(dur>=2s):优先同来源,找不到再跨来源
            let mut prev_same: Option<&Segment> = None;
            let mut prev_any: Option<&Segment> = None;
            for p in segs[..i].iter().rev() {
                if p.end_ms - p.start_ms >= 2000 && p.speaker.is_some() {
                    if prev_any.is_none() {
                        prev_any = Some(p);
                    }
                    if p.source == seg.source {
                        prev_same = Some(p);
                        break;
                    }
                }
            }
            if let Some(target) = prev_same.or(prev_any) {
                let target_spk = target.speaker.clone().unwrap_or_default();
                if target_spk != cur {
                    changes.push((seg.idx, target_spk.clone()));
                    inherited += 1;
                    continue; // 已改簇,不再做规则 2
                }
            }
        }

        // 规则 2:回合约束(同来源前后同簇、中间孤立短 run)
        if dur < 1000 && i > 0 && i + 1 < segs.len() {
            let prev = &segs[i - 1];
            let next = &segs[i + 1];
            if let (Some(ps), Some(ns)) = (&prev.speaker, &next.speaker) {
                if prev.source == seg.source
                    && next.source == seg.source
                    && ps == ns
                    && ps != &cur
                {
                    changes.push((seg.idx, ps.clone()));
                    flipped += 1;
                    continue;
                }
            }
        }

        // 规则 3:称呼反推
        if let Some(name) = parse_vocative(&seg.text) {
            vocatives.push((name, cur));
        }
    }
    RefineReport { changes, inherited, flipped, vocatives }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Segment;

    fn seg(idx: i64, start: i64, dur: i64, spk: &str, source: &str, text: &str) -> Segment {
        Segment {
            idx,
            start_ms: start,
            end_ms: start + dur,
            speaker: Some(spk.into()),
            source: Some(source.into()),
            text: text.into(),
            translation: String::new(),
        }
    }

    #[test]
    fn short_ack_inherits_prev_speaker() {
        // A 长发言,B 短应答"嗯"(被标成 B 自己的簇)→ 应答并入 A
        let segs = vec![
            seg(1, 0, 5000, "SPEAKER_00", "mic", "我先说一下整体进度。"),
            seg(2, 5200, 600, "SPEAKER_01", "mic", "嗯"),
            seg(3, 6500, 5000, "SPEAKER_00", "mic", "然后是风险部分。"),
        ];
        let r = refine(&segs);
        assert_eq!(r.inherited, 1);
        let (_, spk) = r.changes.iter().find(|(i, _)| *i == 2).unwrap();
        assert_eq!(spk, "SPEAKER_00");
    }

    #[test]
    fn ack_prefers_same_source() {
        // 远端 B 长发言、现场 A 长发言;远端"对"继承远端 B(同来源优先)
        let segs = vec![
            seg(1, 0, 4000, "SPEAKER_00", "remote", "远端的意见是继续推进。"),
            seg(2, 4200, 3000, "SPEAKER_01", "mic", "现场我补充一点。"),
            seg(3, 7300, 500, "SPEAKER_02", "remote", "对"),
        ];
        let r = refine(&segs);
        let (_, spk) = r.changes.iter().find(|(i, _)| *i == 3).unwrap();
        assert_eq!(spk, "SPEAKER_00");
    }

    #[test]
    fn turn_constraint_flips_isolated_run() {
        // A B A 且 B 段 <1s → B 并入 A
        let segs = vec![
            seg(1, 0, 3000, "SPEAKER_00", "mic", "第一部分讲完了。"),
            seg(2, 3100, 400, "SPEAKER_01", "mic", "然后呢"),
            seg(3, 3600, 3000, "SPEAKER_00", "mic", "第二部分是成本。"),
        ];
        let r = refine(&segs);
        assert_eq!(r.flipped, 1);
        assert!(r.changes.iter().any(|(idx, spk)| *idx == 2 && spk == "SPEAKER_00"));
    }

    #[test]
    fn vocative_excludes_speaker_name() {
        // 喊"张总，"的段在 SPEAKER_01 → 约束 (张总, SPEAKER_01)
        let segs = vec![seg(1, 0, 3000, "SPEAKER_01", "mic", "张总，这个方案您看行吗")];
        let r = refine(&segs);
        assert_eq!(r.vocatives, vec![("张总".to_string(), "SPEAKER_01".to_string())]);
    }

    #[test]
    fn vocative_requires_punctuation() {
        // "工程师来了" 不是称呼
        let segs = vec![seg(1, 0, 3000, "SPEAKER_00", "mic", "工程师来了吗")];
        let r = refine(&segs);
        assert!(r.vocatives.is_empty());
    }

    #[test]
    fn non_ack_short_segment_untouched() {
        // 短但不是应答词 → 不动
        let segs = vec![
            seg(1, 0, 5000, "SPEAKER_00", "mic", "开头我先说结论。"),
            seg(2, 5200, 800, "SPEAKER_01", "mic", "缺陷"),
        ];
        let r = refine(&segs);
        assert_eq!(r.inherited, 0);
        assert!(r.changes.is_empty());
    }
}
