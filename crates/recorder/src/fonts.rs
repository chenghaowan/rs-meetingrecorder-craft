//! CJK 字体加载(同 pointtofixcraft 方案:系统字体逐个回退)。

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};

pub fn load_cjk_font(ctx: &Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut defs = FontDefinitions::default();
            defs.font_data
                .insert("cjk".into(), std::sync::Arc::new(FontData::from_owned(bytes)));
            if let Some(fam) = defs.families.get_mut(&FontFamily::Proportional) {
                fam.push("cjk".into());
            }
            if let Some(fam) = defs.families.get_mut(&FontFamily::Monospace) {
                fam.push("cjk".into());
            }
            // 韩文兜底(msyh 韩文字形缺失,语言下拉的 한국어 会出豆腐块)
            if let Ok(kor) = std::fs::read(r"C:\Windows\Fonts\malgun.ttf") {
                defs.font_data
                    .insert("kor".into(), std::sync::Arc::new(FontData::from_owned(kor)));
                if let Some(fam) = defs.families.get_mut(&FontFamily::Proportional) {
                    fam.push("kor".into());
                }
                if let Some(fam) = defs.families.get_mut(&FontFamily::Monospace) {
                    fam.push("kor".into());
                }
            }
            ctx.set_fonts(defs);
            return;
        }
    }
    eprintln!("[warn] 未找到系统中文字体(尝试过 msyh/simhei/Deng),中文可能显示为方块");
}
