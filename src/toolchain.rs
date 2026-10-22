//! 工具链调用模块：定位并调用 nasm.exe（汇编器）与 GoLink.exe（链接器）。
//!
//! 查找顺序（严格遵守项目约定）：
//! 1. 当前工作目录下的 `./tools/nasm.exe`、`./tools/GoLink.exe`；
//! 2. 编译器程序自身所在目录旁的 `tools/`（便于把整个目录打包带走）；
//! 3. 系统 PATH。
//!
//! 找不到工具时给出中文提示与下载地址，绝不静默失败。
//!
//! 兼容性细节：nasm.exe 是传统 ANSI 程序，无法打开中文（非 ASCII）路径，
//! 因此输入/输出路径含非 ASCII 字符时，会先复制到临时目录用 ASCII 文件名
//! 完成汇编，再把目标文件搬回原路径（GoLink 同样处理）。
//!
//! 之所以选 GoLink 而不是 mingw 的 ld：GoLink 是单文件 exe、无 dll 依赖、
//! 免费，适合便携打包，并且能直接从命令行列出的 msvcrt.dll 自动导入
//! printf 等外部符号，不需要任何 .lib 导入库。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{级别, 错误类别, 诊断箱};

/// 按约定顺序寻找工具，返回其完整路径。
pub fn 寻找工具(名称: &str) -> Option<PathBuf> {
    // 1) 当前工作目录 ./tools/
    if let Ok(当前目录) = env::current_dir() {
        let 候选 = 当前目录.join("tools").join(名称);
        if 候选.is_file() {
            return Some(候选);
        }
    }
    // 2) 程序自身所在目录旁的 tools/
    if let Ok(程序路径) = env::current_exe() {
        if let Some(程序目录) = 程序路径.parent() {
            let 候选 = 程序目录.join("tools").join(名称);
            if 候选.is_file() {
                return Some(候选);
            }
        }
    }
    // 3) 系统 PATH
    if let Some(path) = env::var_os("PATH") {
        for 目录 in env::split_paths(&path) {
            let 候选 = 目录.join(名称);
            if 候选.is_file() {
                return Some(候选);
            }
        }
    }
    None
}

/// 调用 NASM：把 x86-64 Windows 汇编源码汇编成 win64 COFF 目标文件。
pub fn 调用nasm(nasm路径: &Path, 输入: &Path, 输出: &Path, 诊断: &mut 诊断箱) -> Result<(), ()> {
    if 是纯ASCII路径(输入) && 是纯ASCII路径(输出) {
        return 运行nasm(nasm路径, 输入, 输出, 诊断);
    }
    // NASM 不支持非 ASCII 路径：经临时目录中转（ASCII 文件名）
    经临时目录汇编(nasm路径, 输入, 输出, 诊断)
}

/// 调用 GoLink：把 win64 COFF 目标文件链接成 exe（入口 main，控制台程序）。
/// printf 等外部符号由 GoLink 直接从命令行给出的 msvcrt.dll 自动导入。
pub fn 调用golink(golink路径: &Path, 输入: &Path, 输出: &Path, 诊断: &mut 诊断箱) -> Result<(), ()> {
    if 是纯ASCII路径(输入) && 是纯ASCII路径(输出) {
        return 运行golink(golink路径, 输入, 输出, 诊断);
    }
    // GoLink 同样可能不支持非 ASCII 路径：经临时目录中转
    经临时目录链接(golink路径, 输入, 输出, 诊断)
}

/// 路径是否全部由 ASCII 字符组成（传统工具的兼容性判断）。
#[allow(non_snake_case)]
fn 是纯ASCII路径(路径: &Path) -> bool {
    路径.to_string_lossy().is_ascii()
}

/// 直接运行 nasm。
fn 运行nasm(nasm路径: &Path, 输入: &Path, 输出: &Path, 诊断: &mut 诊断箱) -> Result<(), ()> {
    let 结果 = Command::new(nasm路径)
        .arg("-f")
        .arg("win64")
        .arg(输入)
        .arg("-o")
        .arg(输出)
        .output();
    match 结果 {
        Ok(结果) if 结果.status.success() => Ok(()),
        Ok(结果) => {
            let 错误文本 = String::from_utf8_lossy(&结果.stderr);
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                format!("nasm 汇编失败：\n{}", 错误文本.trim()),
                0,
                0,
            );
            Err(())
        }
        Err(原因) => {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                format!("无法启动 nasm.exe：{}", 原因),
                0,
                0,
            );
            Err(())
        }
    }
}

/// 直接运行 GoLink。
fn 运行golink(golink路径: &Path, 输入: &Path, 输出: &Path, 诊断: &mut 诊断箱) -> Result<(), ()> {
    let 结果 = Command::new(golink路径)
        .arg("/entry")
        .arg("main")
        .arg("/console")
        .arg(输入)
        .arg("msvcrt.dll")
        .arg("kernel32.dll")
        .arg("/fo")
        .arg(输出)
        .output();
    match 结果 {
        Ok(结果) if 结果.status.success() => Ok(()),
        Ok(结果) => {
            // GoLink 的提示信息主要写在 stdout，错误也可能出现在那里，两路都收集
            let 输出文本 = String::from_utf8_lossy(&结果.stdout);
            let 错误文本 = String::from_utf8_lossy(&结果.stderr);
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                format!(
                    "GoLink 链接失败：\n{}{}",
                    输出文本.trim(),
                    错误文本.trim()
                ),
                0,
                0,
            );
            Err(())
        }
        Err(原因) => {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                format!("无法启动 GoLink.exe：{}", 原因),
                0,
                0,
            );
            Err(())
        }
    }
}

/// 建立本次编译专用的临时目录（ASCII 路径）。
fn 新建临时目录() -> Result<PathBuf, ()> {
    let 目录 = env::temp_dir().join(format!("jianyan_{}", std::process::id()));
    fs::create_dir_all(&目录).map_err(|原因| {
        eprintln!("无法创建临时目录『{}』：{}", 目录.display(), 原因);
    })?;
    Ok(目录)
}

/// 把文件从临时目录搬回目标路径。
/// Windows 的 rename 不支持跨磁盘，因此先试 rename，失败则复制后删除源文件。
fn 搬移文件(源: &Path, 目标: &Path) -> std::io::Result<()> {
    match fs::rename(源, 目标) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(源, 目标)?;
            fs::remove_file(源)
        }
    }
}

/// 输入/输出含中文等非 ASCII 字符时，复制到临时目录用 ASCII 文件名完成汇编，
/// 再把目标文件搬回原路径。
fn 经临时目录汇编(nasm路径: &Path, 输入: &Path, 输出: &Path, 诊断: &mut 诊断箱) -> Result<(), ()> {
    let 临时目录 = match 新建临时目录() {
        Ok(目录) => 目录,
        Err(()) => {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                "nasm 不支持中文路径，但创建临时目录失败，请把源文件放到纯英文路径下",
                0,
                0,
            );
            return Err(());
        }
    };
    let 临时输入 = 临时目录.join("input.asm");
    let 临时输出 = 临时目录.join("input.obj");
    // 复制输入到 ASCII 路径
    if fs::copy(输入, &临时输入).is_err() {
        诊断.记录(
            级别::错误,
            错误类别::工具链,
            format!("无法把汇编文件复制到临时目录『{}』", 临时目录.display()),
            0,
            0,
        );
        let _ = fs::remove_dir_all(&临时目录);
        return Err(());
    }
    let 结果 = 运行nasm(nasm路径, &临时输入, &临时输出, 诊断);
    if 结果.is_ok() {
        // 搬回目标路径（Rust 的文件操作支持中文路径）
        if 搬移文件(&临时输出, 输出).is_err() {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                format!("无法把目标文件移回『{}』", 输出.display()),
                0,
                0,
            );
        }
    }
    let _ = fs::remove_dir_all(&临时目录);
    结果
}

/// GoLink 的非 ASCII 路径中转（与 nasm 相同思路）。
fn 经临时目录链接(golink路径: &Path, 输入: &Path, 输出: &Path, 诊断: &mut 诊断箱) -> Result<(), ()> {
    let 临时目录 = match 新建临时目录() {
        Ok(目录) => 目录,
        Err(()) => {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                "GoLink 不支持中文路径，但创建临时目录失败，请把源文件放到纯英文路径下",
                0,
                0,
            );
            return Err(());
        }
    };
    let 临时输入 = 临时目录.join("input.obj");
    let 临时输出 = 临时目录.join("output.exe");
    if fs::copy(输入, &临时输入).is_err() {
        诊断.记录(
            级别::错误,
            错误类别::工具链,
            format!("无法把目标文件复制到临时目录『{}』", 临时目录.display()),
            0,
            0,
        );
        let _ = fs::remove_dir_all(&临时目录);
        return Err(());
    }
    let 结果 = 运行golink(golink路径, &临时输入, &临时输出, 诊断);
    if 结果.is_ok() {
        if 搬移文件(&临时输出, 输出).is_err() {
            诊断.记录(
                级别::错误,
                错误类别::工具链,
                format!("无法把可执行文件移回『{}』", 输出.display()),
                0,
                0,
            );
        }
    }
    let _ = fs::remove_dir_all(&临时目录);
    结果
}
