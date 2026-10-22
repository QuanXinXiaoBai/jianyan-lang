//! 错误处理模块：统一的中文诊断系统。
//!
//! 所有面向用户的错误（词法、语法、语义、工具链、输入输出）都汇总到这里：
//! - 统一输出格式：`错误[类别]: 文件:行:列: 原因`，并附上出错行原文与下划线指示；
//! - 词法/语法/语义阶段的位置先是“虚拟行号”，打印前会按导入展开时记录的
//!   行来源表翻译回真实的文件与行号；
//! - 绝不把 Rust 原生 panic 抛给用户（main 中另有 catch_unwind 兜底）。

use std::rc::Rc;

/// 错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum 错误类别 {
    词法,
    语法,
    语义,
    工具链,
    输入输出,
}

impl 错误类别 {
    /// 类别的中文名。
    pub fn 名字(self) -> &'static str {
        match self {
            错误类别::词法 => "词法错误",
            错误类别::语法 => "语法错误",
            错误类别::语义 => "语义错误",
            错误类别::工具链 => "工具链错误",
            错误类别::输入输出 => "输入输出错误",
        }
    }
}

/// 诊断级别：错误会阻止继续编译，警告不影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum 级别 {
    错误,
    警告,
}

/// 一条诊断消息。
#[derive(Debug, Clone)]
pub struct 诊断 {
    pub 级别: 级别,
    pub 类别: 错误类别,
    pub 消息: String,
    pub 文件: String,
    pub 行: usize, // 1 起；0 表示没有位置信息
    pub 列: usize, // 1 起
    /// 出错那一行的原文（用于输出下划线提示）。
    pub 源码行: Option<String>,
}

/// 行来源表的一项：导入展开后的“虚拟行” → 真实文件与真实行号。
#[derive(Debug, Clone)]
pub struct 行来源 {
    pub 文件: Rc<String>,
    pub 行: usize,
}

/// 诊断收集器：收集全部错误/警告，最后统一打印。
/// 收集数量有硬上限：即使解析器出现意外循环，也只会留下有限条诊断，
/// 绝不允许错误本身把内存吃光。
pub struct 诊断箱 {
    pub 列表: Vec<诊断>,
    行来源表: Vec<行来源>,   // 下标 = 虚拟行 - 1
    虚拟源码行: Vec<String>, // 下标 = 虚拟行 - 1
    打印上限: usize,
    收集上限: usize,
    已截断: bool, // 是否因超过收集上限而停止收集
}

impl 诊断箱 {
    pub fn 新建() -> Self {
        诊断箱 {
            列表: Vec::new(),
            行来源表: Vec::new(),
            虚拟源码行: Vec::new(),
            打印上限: 30,
            收集上限: 200,
            已截断: false,
        }
    }

    /// 设置导入展开产生的行来源表与展开后的源码行（词法分析之前调用）。
    pub fn 设置行映射(&mut self, 行来源表: Vec<行来源>, 虚拟源码行: Vec<String>) {
        self.行来源表 = 行来源表;
        self.虚拟源码行 = 虚拟源码行;
    }

    /// 记录一条诊断。`虚拟行` 会被行来源表翻译成真实文件与行号；
    /// 虚拟行为 0 表示没有位置信息（如工具链错误）。
    pub fn 记录(&mut self, 级别: 级别, 类别: 错误类别, 消息: impl Into<String>, 虚拟行: usize, 列: usize) {
        if self.列表.len() >= self.收集上限 {
            self.已截断 = true;
            return;
        }
        let (文件, 行, 源码行) = if 虚拟行 == 0 {
            ("<编译器>".to_string(), 0usize, None)
        } else {
            match self.行来源表.get(虚拟行 - 1) {
                Some(来源) => (
                    (*来源.文件).clone(),
                    来源.行,
                    self.虚拟源码行.get(虚拟行 - 1).cloned(),
                ),
                None => ("<未知位置>".to_string(), 虚拟行, self.虚拟源码行.get(虚拟行 - 1).cloned()),
            }
        };
        self.列表.push(诊断 {
            级别,
            类别,
            消息: 消息.into(),
            文件,
            行,
            列,
            源码行,
        });
    }

    /// 记录一条已经知道真实文件与行号的诊断（导入展开阶段使用）。
    pub fn 记录于文件(
        &mut self,
        级别: 级别,
        类别: 错误类别,
        消息: impl Into<String>,
        文件: String,
        行: usize,
        列: usize,
    ) {
        if self.列表.len() >= self.收集上限 {
            self.已截断 = true;
            return;
        }
        self.列表.push(诊断 {
            级别,
            类别,
            消息: 消息.into(),
            文件,
            行,
            列,
            源码行: None,
        });
    }

    /// 是否已存在错误级别的诊断。
    pub fn 有错误(&self) -> bool {
        self.列表.iter().any(|诊断| 诊断.级别 == 级别::错误)
    }

    /// 把全部诊断统一打印到标准错误输出。
    pub fn 全部打印(&self) {
        let 错误数 = self.列表.iter().filter(|诊断| 诊断.级别 == 级别::错误).count();
        let 警告数 = self.列表.len() - 错误数;
        for 诊断 in self.列表.iter().take(self.打印上限) {
            eprintln!("{}", 格式化诊断(诊断));
        }
        if self.已截断 {
            eprintln!(
                "…… 诊断数量超过 {} 条，已停止收集（通常是某个语法错误引发的连锁反应）",
                self.收集上限
            );
        } else if self.列表.len() > self.打印上限 {
            eprintln!("…… 其余 {} 条诊断未显示", self.列表.len() - self.打印上限);
        }
        if 错误数 + 警告数 > 0 {
            eprintln!();
            eprintln!("编译结果：{} 个错误，{} 个警告", 错误数, 警告数);
        }
    }
}

/// 把一条诊断格式化成多行文本（含出错行与下划线提示）。
fn 格式化诊断(诊断: &诊断) -> String {
    let 级别文本 = match 诊断.级别 {
        级别::错误 => "错误",
        级别::警告 => "警告",
    };
    let mut 文本 = if 诊断.行 == 0 {
        format!("{}[{}]: {}", 级别文本, 诊断.类别.名字(), 诊断.消息)
    } else {
        format!(
            "{}[{}]: {}:{}:{}: {}",
            级别文本,
            诊断.类别.名字(),
            诊断.文件,
            诊断.行,
            诊断.列,
            诊断.消息
        )
    };
    if let Some(源码行) = &诊断.源码行 {
        文本.push_str(&format!("\n    {}", 源码行));
        if 诊断.列 > 0 {
            文本.push_str(&format!("\n    {}^", " ".repeat(诊断.列.saturating_sub(1))));
        }
    }
    文本
}
