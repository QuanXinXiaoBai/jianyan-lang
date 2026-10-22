//! 导入预处理模块：实现 `导入 "文件名.jy"` 的编译期文本拼接。
//!
//! 第一版策略与 C 的 #include 完全一致：读取目标文件的文本，替换 导入 语句
//! 所在的整行，不做模块隔离。因此跨文件出现同名函数/变量时，会被语义分析
//! 报【符号重复定义】中文错误。
//!
//! 展开的同时维护一张“行来源表”（虚拟行 → 真实文件与真实行号），保证
//! 拼接之后所有词法/语法/语义报错仍然指向真实的文件与行号。

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::error::{级别, 行来源, 错误类别, 诊断箱};

/// 展开结果：拼接后的完整源码文本 + 行来源表。
pub struct 展开结果 {
    pub 文本: String,
    pub 行来源表: Vec<行来源>,
}

/// 最大导入深度（防止“文件导入自身”这类无限递归把内存吃光）。
const 最大深度: usize = 64;

/// 从入口文件出发，递归展开全部 导入。
pub fn 展开导入(入口: &Path, 诊断: &mut 诊断箱) -> Result<展开结果, ()> {
    let mut 文本 = String::new();
    let mut 行表 = Vec::new();
    let mut 导入链: Vec<PathBuf> = Vec::new();
    let 入口显示名 = Rc::new(入口.display().to_string());
    展开文件(入口, &入口显示名, &mut 文本, &mut 行表, &mut 导入链, 0, 诊断)?;
    Ok(展开结果 {
        文本,
        行来源表: 行表,
    })
}

/// 把单个文件的内容（递归展开其中的 导入）追加进 `文本`。
#[allow(clippy::too_many_arguments)]
fn 展开文件(
    路径: &Path,
    显示名: &Rc<String>,
    文本: &mut String,
    行表: &mut Vec<行来源>,
    导入链: &mut Vec<PathBuf>,
    深度: usize,
    诊断: &mut 诊断箱,
) -> Result<(), ()> {
    // ---- 循环导入检测：规范化路径后若已在导入链中 → 报【循环导入】 ----
    let 规范化 = 规范化路径(路径);
    if let Some(位置) = 导入链.iter().position(|已导入| 规范化路径(已导入) == 规范化) {
        let 循环链 = 导入链[位置..]
            .iter()
            .map(|路径| 路径.display().to_string())
            .chain(std::iter::once(显示名.to_string()))
            .collect::<Vec<_>>()
            .join(" → ");
        诊断.记录(
            级别::错误,
            错误类别::语义,
            format!("【循环导入】检测到循环导入：{}", 循环链),
            0,
            0,
        );
        return Err(());
    }
    if 深度 > 最大深度 {
        诊断.记录(
            级别::错误,
            错误类别::语义,
            format!("导入嵌套超过 {} 层，请检查是否存在深层递归导入", 最大深度),
            0,
            0,
        );
        return Err(());
    }
    // ---- 读取文件（UTF-8，容忍开头 BOM） ----
    let 内容 = match fs::read_to_string(路径) {
        Ok(内容) => 内容,
        Err(原因) => {
            诊断.记录(
                级别::错误,
                错误类别::输入输出,
                format!("无法读取源文件『{}』：{}", 显示名, 原因),
                0,
                0,
            );
            return Err(());
        }
    };
    let 内容 = 内容.strip_prefix('\u{feff}').unwrap_or(&内容);

    导入链.push(路径.to_path_buf());
    for (行号从零, 行) in 内容.lines().enumerate() {
        if 是导入行(行) {
            match 解析导入行(行) {
                Some(文件名) => {
                    let 相对路径 = 路径
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(&文件名);
                    let 相对显示名 = Rc::new(相对路径.display().to_string());
                    展开文件(&相对路径, &相对显示名, 文本, 行表, 导入链, 深度 + 1, 诊断)?;
                }
                None => {
                    // 行首是“导入”但格式不对：给出专门的中文提示
                    诊断.记录于文件(
                        级别::错误,
                        错误类别::语法,
                        "『导入』语句格式应为：导入 \"文件名.jy\"（引号内是被导入文件的路径）",
                        显示名.to_string(),
                        行号从零 + 1,
                        1,
                    );
                }
            }
        } else {
            文本.push_str(行);
            文本.push('\n');
            行表.push(行来源 {
                文件: 显示名.clone(),
                行: 行号从零 + 1,
            });
        }
    }
    导入链.pop();
    Ok(())
}

/// 判断一行是否为 导入 语句（“导入”是关键字，后面必须紧跟空白或引号，
/// 避免把 导入xx 之类的标识符误判成导入语句）。
pub fn 是导入行(行: &str) -> bool {
    let 剩余 = 行.trim_start();
    match 剩余.strip_prefix("导入") {
        Some(尾) => 尾.chars().next().map_or(false, |字符| 字符.is_whitespace() || 字符 == '"'),
        None => false,
    }
}

/// 解析一行 导入 语句，返回被导入的文件名；格式不对返回 None。
fn 解析导入行(行: &str) -> Option<String> {
    let 剩余 = 行.trim_start().strip_prefix("导入")?;
    let 剩余 = 剩余.trim_start().strip_prefix('"')?;
    let 结束 = 剩余.find('"')?;
    let 文件名 = &剩余[..结束];
    let 尾部 = 剩余[结束 + 1..].trim_start();
    // 引号之后只允许空白或 // 注释
    if !尾部.is_empty() && !尾部.starts_with("//") {
        return None;
    }
    Some(文件名.to_string())
}

/// 路径规范化（用于循环导入检测）：优先 canonicalize，失败时拼绝对路径。
fn 规范化路径(路径: &Path) -> PathBuf {
    fs::canonicalize(路径).unwrap_or_else(|_| {
        if 路径.is_absolute() {
            路径.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|目录| 目录.join(路径))
                .unwrap_or_else(|_| 路径.to_path_buf())
        }
    })
}
