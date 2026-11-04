//! mrc-core — 会议记录核心库。
//!
//! 分层:实时链路(capture → transcribe → db)与后处理链路
//! (diarize → naming → chunk → shots → llm → package → rag),
//! 两链路只通过 meetings.db 交汇;recorder(常驻托盘)与 postprocess(CLI)
//! 都是薄壳,业务全在这里。

pub mod capture;
pub mod chunk;
pub mod config;
pub mod db;
pub mod diarize;
pub mod llm;
pub mod package;
pub mod pipeline;
pub mod rag;
pub mod shots;
pub mod tokens;
pub mod tools;
pub mod transcribe;

pub use config::Config;
