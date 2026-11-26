//! 打印本机 DXGI 适配器与本机推荐档(调试用):cargo run -p mrc-core --example gpu_probe

fn main() {
    for g in mrc_core::gpu::gpus() {
        println!(
            "{:?} dedicated={}MB shared={}MB",
            g.name, g.dedicated_mb, g.shared_mb
        );
    }
    match mrc_core::gpu::discrete() {
        Some(g) => println!("discrete: {} ({} MB)", g.name, g.dedicated_mb),
        None => println!("discrete: none"),
    }
    let rec = mrc_core::llm::recommend_for_machine();
    println!("recommend: {} ctx={}", rec.label, rec.ctx);
}
