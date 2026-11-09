//! `wxlocated`: the headless, agent-facing CLI. Business commands write JSON
//! to stdout; help prints the command usage.

mod commands;
mod dump;
mod tools;

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use sha2::{Digest, Sha256};
use wxlocated_engine::archive::Archive;

#[derive(Serialize)]
struct DumpOutput {
    packages: Vec<DumpPackage>,
}

#[derive(Serialize)]
struct DumpPackage {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
    entries: Vec<DumpEntry>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct DumpEntry {
    path: String,
    offset: u64,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

/// Hidden: every entry of the given packages with offset, size and SHA-256,
/// diffed against the Swift oracle by `scripts/parity-archive.py`.
fn dump_entries(paths: &[String]) -> DumpOutput {
    let packages = paths
        .iter()
        .filter(|path| !path.starts_with('-'))
        .map(|path| match Archive::open(Path::new(path)) {
            Err(error) => DumpPackage {
                path: path.clone(),
                format: None,
                failure: Some(error.code()),
                entries: Vec::new(),
                notes: Vec::new(),
            },
            Ok(archive) => DumpPackage {
                path: path.clone(),
                format: Some(archive.format.as_str()),
                failure: None,
                entries: archive
                    .entries
                    .iter()
                    .map(|entry| {
                        let (sha256, failure) = match archive.read(entry) {
                            Ok(bytes) => (Some(hex::encode(Sha256::digest(&bytes))), None),
                            Err(error) => (None, Some(error.code())),
                        };
                        DumpEntry { path: entry.path.clone(), offset: entry.offset, size: entry.size, sha256, failure }
                    })
                    .collect(),
                notes: archive.notes.clone(),
            },
        })
        .collect();
    DumpOutput { packages }
}

fn emit_failure(failure: &commands::Failure, pretty: bool) -> ExitCode {
    let mut text = wxlocated_engine::swift_json::encode(&failure.output(), pretty);
    text.push('\n');
    print!("{text}");
    ExitCode::from(failure.exit)
}

const USAGE: &str = "wxlocated — 为 Agent 提供本机微信小程序审计材料（输出 JSON）

用法：
  wxlocated list                                   列出本机缓存的小程序
  wxlocated info <appid|名称>                      材料完整度与包信息（material-check schema）
  wxlocated export <appid|名称> --out <目录>        导出 raw/、source/、material.json、AGENT.md
            [--force] [--reuse] [--redact] [--no-source] [--no-raw]
  wxlocated trace <材料目录> <路径>[:行号] [--context 字节]
                                                   把重建代码映射回 raw/ 中的原始字节
  wxlocated watch <appid|名称> [--timeout 秒] [--interval 秒]
                                                   等待缺失分包与插件下载（JSON Lines）
  wxlocated overlay init <材料目录> <工作目录> [--name 名称] [--agent 名称] [--model 模型]
  wxlocated overlay import <工作目录>             Agent 语义化重建层：校验基线后保存改动的文件
  wxlocated overlay list [appid] · overlay remove <appid> <名称>
  wxlocated doctor                                 检查磁盘权限、缓存目录、微信与命令行安装
  wxlocated version

选项：
  --compact   单行 JSON

退出码：0 成功 · 64 用法错误 · 66 未找到 · 70 执行失败 · 75 watch 超时
stdout 只输出 JSON；进度写入 stderr。
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let pretty = !args.iter().any(|arg| arg == "--compact");
    args.retain(|arg| arg != "--compact");
    let Some(command) = args.first().cloned() else {
        eprintln!("usage: wxlocated <command>");
        return ExitCode::from(64);
    };
    let rest = &args[1..];
    let output = match command.as_str() {
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "list" => commands::list(),
        "export" => match commands::export(rest) {
            Ok(value) => value,
            Err(failure) => return emit_failure(&failure, pretty),
        },
        "overlay" => match tools::overlay(rest) {
            Ok(value) => value,
            Err(failure) => return emit_failure(&failure, pretty),
        },
        "watch" => match tools::watch(rest) {
            Ok(code) => return ExitCode::from(code),
            Err(failure) => return emit_failure(&failure, pretty),
        },
        "doctor" => {
            let (report, code) = tools::doctor();
            let mut text = wxlocated_engine::swift_json::encode(&report, pretty);
            text.push('\n');
            print!("{text}");
            return ExitCode::from(code);
        }
        "trace" => match commands::trace(rest) {
            Ok(value) => value,
            Err(failure) => return emit_failure(&failure, pretty),
        },
        "info" => match commands::info(rest) {
            Ok(value) => value,
            Err(failure) => return emit_failure(&failure, pretty),
        },
        "version" | "--version" => commands::version(),
        "__dump-entries" => serde_json::to_value(dump_entries(rest)).expect("serializable output"),
        "__dump-decompilation" => match dump::dump_decompilation(rest) {
            Ok(value) => value,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(70);
            }
        },
        _ => {
            eprintln!("unknown command: {command}");
            return ExitCode::from(64);
        }
    };
    let mut text = wxlocated_engine::swift_json::encode(&output, pretty);
    text.push('\n');
    print!("{text}");
    ExitCode::SUCCESS
}
