fn main() {
    let cfg = mrc_core::config::TranslateCfg {
        provider: "wanai".into(),
        from_lang: "zh".into(),
        to_lang: "en".into(),
        ..Default::default()
    };
    let llm = mrc_core::config::LlmCfg::default();
    let tr = mrc_core::translate::Translator::new(&cfg, &llm).expect("build");
    let out = tr.translate_batch(&["hello world".into(), "update the DFM".into()]);
    println!("result: {:?}", out);
}
