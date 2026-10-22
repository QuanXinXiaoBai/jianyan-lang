//! 【简言】编译器入口：命令行参数解析与三种编译模式的分派。
//!
//! 用法：
//!   模式A（仅生成汇编）：jianyan -S 源文件.jy
//!   模式B（完整构建）：  jianyan 源文件.jy
//!   模式C（保留中间物）：jianyan --keep-temp 源文件.jy
//!
//! 完整构建分三步：
//!   ① 简言源码 → demo.asm  ② nasm.exe → demo.obj（win64 COFF）  ③ GoLink.exe → demo.exe
//!
//! 所有报错统一走 error.rs 的中文诊断系统（带 文件:行:列 与出错行提示）；
//! 即使编译器内部意外 panic，也会被这里兜底成一句中文提示，
//! 而不是把 Rust 的原生 panic 信息抛给用户。

mod ast;
mod codegen;
mod error;
mod import;
mod lexer;
mod parser;
mod semantic;
mod toolchain;

use std::env;
use std::fs;
use std::panic;
use std::path::PathBuf;
use std::process::ExitCode;

use error::{级别, 错误类别, 诊断箱};

/// 命令行参数。
struct 参数 {
    仅生成汇编: bool,
    保留中间产物: bool,
    源文件: PathBuf,
}

/// 参数解析结果：显示帮助 / 运行编译。
enum 参数解析结果 {
    帮助,
    运行(参数),
}

const 帮助文本: &str = "\
【简言】编译器原型 v0.3 —— 中文缩进式教学语言，后端输出 NASM x86-64 Windows 汇编。

用法：
  jianyan [选项] 源文件.jy

选项：
  -S            模式A：只生成 源文件.asm，不调用 nasm / GoLink（用于查看汇编）
  --keep-temp   模式C：完整构建成功后保留 .asm 与 .obj 中间产物（默认清理）
  -h, --help    显示本帮助

不带 -S 时执行完整构建（模式B）：
  ① 简言源码 → demo.asm   ② nasm.exe → demo.obj（win64 COFF）  ③ GoLink.exe → demo.exe
工具查找顺序：先找 ./tools/nasm.exe 与 ./tools/GoLink.exe，找不到再回退系统 PATH。
";

fn main() -> ExitCode {
    // Windows 控制台默认代码页可能不是 UTF-8，先切成 UTF-8 保证中文正常显示。
    #[cfg(target_os = "windows")]
    设置控制台为UTF8();
    match panic::catch_unwind(运行) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(())) => ExitCode::FAILURE,
        Err(_) => {
            eprintln!("【内部错误】编译器内部发生了未预期的错误（这是一个 bug，欢迎反馈）。");
            ExitCode::FAILURE
        }
    }
}

/// 把控制台输入输出代码页切换为 UTF-8（Windows 专有，失败也无关紧要）。
#[cfg(target_os = "windows")]
#[allow(non_snake_case)]
fn 设置控制台为UTF8() {
    unsafe extern "system" {
        fn SetConsoleOutputCP(代码页: u32) -> i32;
        fn SetConsoleCP(代码页: u32) -> i32;
    }
    const 代码页_UTF8: u32 = 65001;
    unsafe {
        SetConsoleOutputCP(代码页_UTF8);
        SetConsoleCP(代码页_UTF8);
    }
}

fn 运行() -> Result<(), ()> {
    let 参数 = match 解析参数() {
        Ok(参数解析结果::帮助) => {
            print!("{}", 帮助文本);
            return Ok(());
        }
        Ok(参数解析结果::运行(参数)) => 参数,
        Err(消息) => {
            eprintln!("错误[命令行]: {}", 消息);
            eprintln!();
            eprintln!("{}", 帮助文本);
            return Err(());
        }
    };

    let mut 诊断 = 诊断箱::新建();

    // ---- ① 展开 导入（编译期文本拼接，类似 C 的 #include）----
    let 展开 = match import::展开导入(&参数.源文件, &mut 诊断) {
        Ok(展开) => 展开,
        Err(()) => {
            诊断.全部打印();
            return Err(());
        }
    };
    诊断.设置行映射(展开.行来源表, 展开.文本.lines().map(str::to_string).collect());

    // ---- ② 词法分析 ----
    let 记号流 = match lexer::切分(&展开.文本, &mut 诊断) {
        Ok(记号流) => 记号流,
        Err(()) => {
            诊断.全部打印();
            return Err(());
        }
    };
    if 诊断.有错误() {
        诊断.全部打印();
        return Err(());
    }

    // ---- ③ 语法分析 ----
    let 程序 = match parser::解析(记号流, &mut 诊断) {
        Ok(程序) => 程序,
        Err(()) => {
            诊断.全部打印();
            return Err(());
        }
    };
    if 诊断.有错误() {
        诊断.全部打印();
        return Err(());
    }

    // ---- ④ 语义分析 ----
    let 语义 = match semantic::分析(&程序, &mut 诊断) {
        Ok(语义) => 语义,
        Err(()) => {
            诊断.全部打印();
            return Err(());
        }
    };
    if 诊断.有错误() {
        诊断.全部打印();
        return Err(());
    }

    // ---- ⑤ 汇编生成 ----
    let 汇编 = codegen::生成(&程序, &语义);
    let asm路径 = 参数.源文件.with_extension("asm");
    if let Err(原因) = fs::write(&asm路径, &汇编) {
        诊断.记录(
            级别::错误,
            错误类别::输入输出,
            format!("无法写出汇编文件『{}』：{}", asm路径.display(), 原因),
            0,
            0,
        );
        诊断.全部打印();
        return Err(());
    }
    println!("✅ 汇编代码已生成：{}", asm路径.display());
    if 参数.仅生成汇编 {
        // 模式A到此为止：不调用 nasm、不链接
        return Ok(());
    }

    // ---- ⑥ 定位工具链 ----
    let nasm路径 = match toolchain::寻找工具("nasm.exe") {
        Some(路径) => 路径,
        None => {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                "找不到 nasm.exe。\n请把 nasm.exe 放到项目 ./tools/ 目录（或加入系统 PATH）。\n下载地址：https://www.nasm.us/pub/nasm/releasebuilds/",
                0,
                0,
            );
            诊断.全部打印();
            return Err(());
        }
    };
    let golink路径 = match toolchain::寻找工具("GoLink.exe") {
        Some(路径) => 路径,
        None => {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                "找不到 GoLink.exe。\n请把 GoLink.exe 放到项目 ./tools/ 目录（或加入系统 PATH）。\n下载地址：http://www.godevtool.com/ （GoLink.zip，解压得到 GoLink.exe）",
                0,
                0,
            );
            诊断.全部打印();
            return Err(());
        }
    };

    // ---- ⑦ nasm 汇编 ----
    let obj路径 = 参数.源文件.with_extension("obj");
    if toolchain::调用nasm(&nasm路径, &asm路径, &obj路径, &mut 诊断).is_err() {
        诊断.全部打印();
        println!("（中间文件 {} 已保留，便于排查）", asm路径.display());
        return Err(());
    }
    println!("✅ 目标文件已生成：{}", obj路径.display());

    // ---- ⑧ GoLink 链接 ----
    let exe路径 = 参数.源文件.with_extension("exe");
    if toolchain::调用golink(&golink路径, &obj路径, &exe路径, &mut 诊断).is_err() {
        诊断.全部打印();
        println!(
            "（中间文件 {}、{} 已保留，便于排查）",
            asm路径.display(),
            obj路径.display()
        );
        return Err(());
    }
    println!("✅ 构建完成：{}", exe路径.display());

    // ---- 模式C：清理中间产物 ----
    if 参数.保留中间产物 {
        println!(
            "（--keep-temp：已保留 {} 与 {}）",
            asm路径.display(),
            obj路径.display()
        );
    } else {
        let _ = fs::remove_file(&asm路径);
        let _ = fs::remove_file(&obj路径);
        println!(
            "🗑 已清理中间产物（{}、{}），加 --keep-temp 可保留",
            asm路径.display(),
            obj路径.display()
        );
    }
    println!("运行：.\\{}", exe路径.file_name().unwrap_or_default().to_string_lossy());
    Ok(())
}

/// 解析命令行参数。
fn 解析参数() -> Result<参数解析结果, String> {
    let mut 仅生成汇编 = false;
    let mut 保留中间产物 = false;
    let mut 源文件: Option<PathBuf> = None;
    for 项 in env::args().skip(1) {
        match 项.as_str() {
            "-S" => 仅生成汇编 = true,
            "--keep-temp" => 保留中间产物 = true,
            "-h" | "--help" => return Ok(参数解析结果::帮助),
            _ if 项.starts_with('-') => {
                return Err(format!(
                    "未知的命令行参数『{}』（可用：-S / --keep-temp / -h）",
                    项
                ));
            }
            _ => {
                if 源文件.is_some() {
                    return Err("一次只能编译一个源文件（多文件请使用 导入 \"文件名.jy\"）".to_string());
                }
                源文件 = Some(PathBuf::from(项));
            }
        }
    }
    match 源文件 {
        Some(源文件) => Ok(参数解析结果::运行(参数 {
            仅生成汇编,
            保留中间产物,
            源文件,
        })),
        None => Err("缺少源文件（用法：jianyan [选项] 源文件.jy）".to_string()),
    }
}
