//! Kiro 反代内核：把 Kiro（AWS Kiro IDE）私有 API 翻译成两套公开协议。
//!
//! - `auth`：Kiro IDE SSO 凭据读取 + OIDC token 刷新 + 账户模型/额度查询
//! - `eventstream`：AWS Event Stream 二进制帧解析
//! - `models`：模型名映射 / effort 归一化 / token 估算 / 模型能力 schema
//! - `transform`：Anthropic ↔ Kiro 请求响应转换
//! - `openai`：OpenAI Chat Completions 兼容层（入参归一化 + 出参翻译）
//! - `server`：本地 HTTP 代理，同一端口按路径分流两套协议
//! - `service`：应用层集成（Tauri 命令、配置持久化、凭据定时刷新）
//!
//! 协议内核移植自 https://github.com/aristra/kiro2cli（MIT License），
//! 与 Kiro 上游事实源（`runtime.us-east-1.kiro.dev`）的对接细节保持一致。

pub mod auth;
pub mod eventstream;
pub mod models;
pub mod openai;
pub mod server;
pub mod service;
pub mod transform;

pub use service::*;
