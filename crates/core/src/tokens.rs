//! 近似 token 估算:中文按 1 字≈1 token,非 CJK 按每 4 字符≈1 token。
//! 只用于 chunk 切块阈值与上下文降级判断,不需要精确。

pub fn approx_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else if !c.is_whitespace() {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF   // CJK 统一表意
        | 0x3400..=0x4DBF // 扩展 A
        | 0x3000..=0x303F // CJK 标点
        | 0xFF00..=0xFFEF // 全角
        | 0x20000..=0x2A6DF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_cjk_and_ascii() {
        assert_eq!(approx_tokens("增加偷胶结构"), 6);
        // 空白不计;update(6)+DFM(3)+file(4)=13 字符 → ceil(13/4)=4
        assert_eq!(approx_tokens("update DFM file"), 4);
        // 6 个汉字 + 7 个 ASCII → 6+2=8
        assert_eq!(approx_tokens("更新DFM并同步到wiki"), 8);
    }
}
