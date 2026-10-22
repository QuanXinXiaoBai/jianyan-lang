//! 汇编生成模块：把 AST 翻译成 NASM 语法的 x86-64 Windows 汇编源码。
//!
//! 设计要点（学习向，可读性优先，不做性能优化）：
//! - 调用约定遵循 Windows x64（win64）：整型/字符串指针参数走 rcx、rdx、r8、r9，
//!   浮点参数走 xmm0～xmm3，超出部分按参数声明顺序压栈；每次 call 前预留
//!   32 字节影子空间，并保证 rsp 始终 16 字节对齐；
//! - 值约定（编译器内部）：整数/布尔/字符串指针放在 rax，浮点数放在 xmm0；
//! - 栈帧：每个变量一个 8 字节槽位（浮点以位模式存放），参数槽在前、局部变量在后，
//!   表达式求值用的临时区在帧的最底部；帧大小在函数体生成完后统一计算；
//! - 中文标识符会映射成 `_jy_fn0`、`_jy_fn1`…… 这样的 NASM 符号（NASM 符号名
//!   对中文支持有限），同时用注释保留原始函数名，方便对照阅读；
//! - 字符串与浮点常量统一放进 .rdata；打印走 msvcrt.dll 的 printf，
//!   由 GoLink 直接从 msvcrt.dll 自动导入，不需要任何 .lib 导入库。

use std::collections::HashMap;

use crate::ast::{表达式, 表达式种类, 程序, 语句, 语句种类, 二元运算符, 类型};
use crate::semantic::{作用域, 语义结果};

/// 整型参数在 win64 下依次使用的寄存器。
const 整数参数寄存器: [&str; 4] = ["rcx", "rdx", "r8", "r9"];
/// 浮点参数依次使用的寄存器。
const 浮点参数寄存器: [&str; 4] = ["xmm0", "xmm1", "xmm2", "xmm3"];

/// 参数在调用约定中的最终位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum 参数位置 {
    整数寄存器(usize),
    浮点寄存器(usize),
    栈(usize), // 栈参数序号（从 0 起，按参数声明顺序数）
}

/// 把形参列表翻译成调用约定位置（调用方与函数方共用同一规则）。
fn 分配参数位置(参数: &[(类型, String)]) -> Vec<参数位置> {
    let mut 整数计数 = 0usize;
    let mut 浮点计数 = 0usize;
    let mut 栈计数 = 0usize;
    let mut 位置 = Vec::with_capacity(参数.len());
    for (参数类型, _) in 参数 {
        let 位置项 = if *参数类型 == 类型::浮点数64 {
            if 浮点计数 < 4 {
                参数位置::浮点寄存器(浮点计数)
            } else {
                参数位置::栈(栈计数)
            }
        } else {
            if 整数计数 < 4 {
                参数位置::整数寄存器(整数计数)
            } else {
                参数位置::栈(栈计数)
            }
        };
        if *参数类型 == 类型::浮点数64 {
            浮点计数 += 1;
        } else {
            整数计数 += 1;
        }
        if matches!(位置项, 参数位置::栈(_)) {
            栈计数 += 1;
        }
        位置.push(位置项);
    }
    位置
}

/// 比较的“机器类别”：整数用 cmp/jl/jg……，浮点用 ucomisd/jb/ja……，
/// 字符串用 strcmp + test（仅支持 == / !=）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum 比较类别 {
    整数,
    浮点,
    字符串,
}

/// 当前函数生成期间的帧信息。
struct 帧信息 {
    槽位偏移: HashMap<String, i32>, // 变量名 → 相对 rbp 的偏移（负数）
    类型表: HashMap<String, 类型>,
    槽位总数: usize, // 作用域里变量的总数（参数 + 局部变量）
    隐藏槽位数: usize, // 重复 语句占用的合成槽位数
    当前偏移: i32, // rsp 当前相对 rbp 的偏移（负数；临时区在槽位之下）
    最深偏移: i32, // 临时区到过的最低点（用于计算帧大小）
    返回类型: 类型,
    返回标号: String,
}

/// 代码生成器。
struct 代码生成器<'a> {
    语义: &'a 语义结果,
    /// 简言函数名 → NASM 符号名。
    函数符号: HashMap<String, String>,
    函数编号: usize,
    标号编号: usize,
    /// 字符串常量表：内容字节 → 标签。
    字符串常量: HashMap<Vec<u8>, String>,
    字符串列表: Vec<(String, Vec<u8>)>,
    /// 浮点常量表：位模式 → 标签。
    浮点常量: HashMap<u64, String>,
    浮点列表: Vec<(String, u64)>,
    /// 已生成的各函数体（中文注释头 + 完整汇编文本）。
    函数体列表: Vec<(String, String)>,
    帧: 帧信息,
    代码缓冲: Vec<String>,
    /// 循环上下文栈：(循环标号, 结束标号)，供 跳出/跳过 生成跳转。
    循环栈: Vec<(String, String)>,
    /// 是否用到了字符串拼接（决定是否生成运行时辅助函数与 extern）。
    用了拼接: bool,
    /// 是否用到了 数值 → 字符串 转换。
    用了数字转字符串: bool,
    /// 是否用到了字符串 == / != 比较。
    用了字符串比较: bool,
}

/// 对齐到 16 字节。
fn 对齐16(数值: i32) -> i32 {
    (数值 + 15) & !15
}

/// 生成整个程序的 NASM 汇编文本。
pub fn 生成(程序: &程序, 语义: &语义结果) -> String {
    let mut 生成器 = 代码生成器::新建(语义);
    生成器.生成全部(程序)
}

impl<'a> 代码生成器<'a> {
    fn 新建(语义: &'a 语义结果) -> Self {
        代码生成器 {
            语义,
            函数符号: HashMap::new(),
            函数编号: 0,
            标号编号: 0,
            字符串常量: HashMap::new(),
            字符串列表: Vec::new(),
            浮点常量: HashMap::new(),
            浮点列表: Vec::new(),
            函数体列表: Vec::new(),
            帧: 帧信息 {
                槽位偏移: HashMap::new(),
                类型表: HashMap::new(),
                槽位总数: 0,
                隐藏槽位数: 0,
                当前偏移: 0,
                最深偏移: 0,
                返回类型: 类型::空,
                返回标号: String::new(),
            },
            代码缓冲: Vec::new(),
            循环栈: Vec::new(),
            用了拼接: false,
            用了数字转字符串: false,
            用了字符串比较: false,
        }
    }

    fn 新标号(&mut self) -> String {
        let 标号 = format!(".L{}", self.标号编号);
        self.标号编号 += 1;
        标号
    }

    fn 行(&mut self, 文本: &str) {
        self.代码缓冲.push(文本.to_string());
    }

    /// 分配临时区，返回其相对 rbp 的起始偏移。
    fn 分配临时(&mut self, 字节: i32) -> i32 {
        self.帧.当前偏移 -= 字节;
        self.帧.最深偏移 = self.帧.最深偏移.min(self.帧.当前偏移);
        self.帧.当前偏移
    }

    /// 为 重复 语句分配一个合成槽位（剩余次数计数器），返回相对 rbp 的偏移。
    fn 分配隐藏槽(&mut self) -> i32 {
        let 序号 = self.帧.槽位总数 + self.帧.隐藏槽位数;
        self.帧.隐藏槽位数 += 1;
        -8 - 8 * 序号 as i32
    }

    fn 释放临时(&mut self, 字节: i32) {
        self.帧.当前偏移 += 字节;
    }

    fn 字符串常量标签(&mut self, 内容: &str) -> String {
        let 字节 = 内容.as_bytes().to_vec();
        if let Some(标签) = self.字符串常量.get(&字节) {
            return 标签.clone();
        }
        let 标签 = format!("__str{}", self.字符串列表.len());
        self.字符串常量.insert(字节.clone(), 标签.clone());
        self.字符串列表.push((标签.clone(), 字节));
        标签
    }

    fn 浮点常量标签(&mut self, 位模式: u64) -> String {
        if let Some(标签) = self.浮点常量.get(&位模式) {
            return 标签.clone();
        }
        let 标签 = format!("__flt{}", self.浮点列表.len());
        self.浮点常量.insert(位模式, 标签.clone());
        self.浮点列表.push((标签.clone(), 位模式));
        标签
    }

    // ---------- 顶层拼装 ----------

    fn 生成全部(&mut self, 程序: &程序) -> String {
        // ---- 0. 建立 简言函数名 → NASM 符号 的映射 ----
        for 函数 in &程序.函数列表 {
            if !self.函数符号.contains_key(&函数.函数名) {
                let 符号 = format!("_jy_fn{}", self.函数编号);
                self.函数编号 += 1;
                self.函数符号.insert(函数.函数名.clone(), 符号);
            }
        }
        // ---- 1. 生成入口 main（顶层语句自动包裹）----
        // 先把共享引用“提出”，避免与 &mut self 冲突
        let 语义: &'a 语义结果 = self.语义;
        self.生成函数体(
            "main",
            "入口 main（由编译器自动生成，包裹所有顶层语句）",
            类型::整数64,
            &[],
            &程序.顶层语句,
            &语义.入口作用域,
            true,
        );
        // ---- 2. 逐个生成普通函数 ----
        for 函数 in &程序.函数列表 {
            let 符号 = self.函数符号[&函数.函数名].clone();
            let 参数文本 = 函数
                .参数
                .iter()
                .map(|(类型, 名)| format!("{} {}", 类型.名字(), 名))
                .collect::<Vec<_>>()
                .join(", ");
            let 注释 = format!(
                "函数：{}({}) -> {}",
                函数.函数名,
                参数文本,
                函数.返回类型.名字()
            );
            let 作用域 = &语义.函数作用域表[&函数.函数名];
            self.生成函数体(&符号, &注释, 函数.返回类型, &函数.参数, &函数.函数体, 作用域, false);
        }
        // ---- 3. 拼装完整汇编文本 ----
        let mut 输出 = String::new();
        输出.push_str("; ============================================================\n");
        输出.push_str("; 由【简言】编译器自动生成的 NASM x86-64 Windows 汇编\n");
        输出.push_str("; 调用约定：Windows x64（win64），入口 main，链接器 GoLink\n");
        输出.push_str("; 中文函数名被映射为 _jy_fn0、_jy_fn1……，见各函数注释\n");
        输出.push_str("; ============================================================\n");
        输出.push_str("bits 64\n");
        输出.push_str("default rel\n\n");
        // ---- 数据段 ----
        输出.push_str("section .rdata\n");
        输出.push_str("    ; ---- printf 格式串：带换行的用于 打印 的最后一个参数 ----\n");
        输出.push_str("    __fmt_int:   db \"%lld\", 10, 0\n");
        输出.push_str("    __fmt_float: db \"%g\", 10, 0\n");
        输出.push_str("    __fmt_str:   db \"%s\", 10, 0\n");
        输出.push_str("    ; ---- 不带换行的格式串：用于 打印 的非末尾参数（同一行拼接输出）----\n");
        输出.push_str("    __fmt_int_plain:   db \"%lld\", 0\n");
        输出.push_str("    __fmt_float_plain: db \"%g\", 0\n");
        输出.push_str("    __fmt_str_plain:   db \"%s\", 0\n");
        输出.push_str("    ; ---- 字符串运行时辅助的格式串（不带换行）----\n");
        输出.push_str("    __fmt_ll: db \"%lld\", 0\n");
        输出.push_str("    __fmt_g:  db \"%g\", 0\n");
        输出.push_str("    ; ---- 常用浮点常量 ----\n");
        输出.push_str("    __f_zero: dq 0.0\n");
        输出.push_str("    __f_neg1: dq -1.0\n");
        输出.push_str("    ; ---- 字符串常量 ----\n");
        for (标签, 字节) in &self.字符串列表 {
            let 原文 = String::from_utf8_lossy(字节);
            输出.push_str(&format!("    ; “{}”\n", 转义显示(&原文)));
            输出.push_str(&format!("    {}: {}\n", 标签, 序列化字节(字节)));
        }
        for (标签, 位模式) in &self.浮点列表 {
            let 值 = f64::from_bits(*位模式);
            输出.push_str("    align 8            ; 浮点常量需 8 字节对齐\n");
            输出.push_str(&format!("    {}: dq 0x{:016X}    ; {}\n", 标签, 位模式, 值));
        }
        // ---- 代码段 ----
        输出.push_str("\nsection .text\n");
        输出.push_str("    extern printf    ; 由 GoLink 从 msvcrt.dll 自动导入\n");
        if self.用了拼接 || self.用了数字转字符串 {
            输出.push_str("    extern malloc, strlen, strcpy, strcat    ; 字符串运行时支持\n");
        }
        if self.用了数字转字符串 {
            输出.push_str("    extern sprintf    ; 数值 → 字符串\n");
        }
        if self.用了字符串比较 {
            输出.push_str("    extern strcmp    ; 字符串比较\n");
        }
        输出.push_str("    global main\n\n");
        for (注释, 文本) in &self.函数体列表 {
            输出.push_str(&格式函数头(注释));
            输出.push_str(文本);
            输出.push('\n');
        }
        // ---- 运行时辅助函数（按需生成）----
        if self.用了拼接 {
            输出.push_str(&运行时_字符串拼接());
        }
        if self.用了数字转字符串 {
            输出.push_str(&运行时_整数转字符串());
            输出.push_str(&运行时_浮点转字符串());
        }
        输出
    }

    // ---------- 函数级生成 ----------

    #[allow(clippy::too_many_arguments)]
    fn 生成函数体(
        &mut self,
        符号: &str,
        注释: &str,
        返回类型: 类型,
        参数: &[(类型, String)],
        体: &[语句],
        作用域: &作用域,
        是入口: bool,
    ) {
        // 槽位：参数在前、局部变量在后，每个 8 字节
        let mut 槽位偏移 = HashMap::new();
        for (序号, 变量名) in 作用域.顺序.iter().enumerate() {
            槽位偏移.insert(变量名.clone(), -8 - 8 * 序号 as i32);
        }
        let 槽位总数 = 作用域.顺序.len();
        let 槽位区大小 = 8 * 槽位总数 as i32;
        let 返回标号 = self.新标号();
        self.帧 = 帧信息 {
            槽位偏移,
            类型表: 作用域.类型表.clone(),
            槽位总数,
            隐藏槽位数: 0,
            当前偏移: -槽位区大小,
            最深偏移: -槽位区大小,
            返回类型,
            返回标号: 返回标号.clone(),
        };
        self.代码缓冲.clear();

        // ---- 函数体语句 ----
        for 语句 in 体 {
            self.生成语句(语句);
        }
        if 是入口 {
            self.行("    xor eax, eax          ; 入口默认返回 0");
        }
        // ---- 计算帧大小 ----
        let 总槽位数 = (self.帧.槽位总数 + self.帧.隐藏槽位数) as i32;
        let 临时区大小 = (-self.帧.最深偏移) - 总槽位数 * 8;
        let 帧大小 = 对齐16(总槽位数 * 8 + 临时区大小);

        // ---- 组装：序言 + 保存参数 + 体 + 尾声 ----
        let mut 文本 = String::new();
        文本.push_str(&format!("{}:\n", 符号));
        文本.push_str("    push rbp\n");
        文本.push_str("    mov rbp, rsp\n");
        if 帧大小 > 0 {
            文本.push_str(&format!(
                "    sub rsp, {}          ; 局部变量 + 表达式临时区（{} 字节）\n",
                帧大小, 帧大小
            ));
        }
        if !参数.is_empty() {
            文本.push_str("    ; ---- 保存参数到栈槽位 ----\n");
            let 位置表 = 分配参数位置(参数);
            for (序号, ((参数类型, 参数名), 位置)) in 参数.iter().zip(位置表.iter()).enumerate() {
                let 槽 = -8 - 8 * 序号 as i32;
                match 位置 {
                    参数位置::整数寄存器(寄存器序号) => 文本.push_str(&format!(
                        "    mov [rbp{}], {}    ; 参数 {}\n",
                        槽,
                        整数参数寄存器[*寄存器序号],
                        参数名
                    )),
                    参数位置::浮点寄存器(寄存器序号) => 文本.push_str(&format!(
                        "    movsd [rbp{}], {}  ; 参数 {}（浮点）\n",
                        槽,
                        浮点参数寄存器[*寄存器序号],
                        参数名
                    )),
                    参数位置::栈(栈序号) => {
                        // rbp+16 起是影子空间之后的第一块栈参数
                        let 来源 = 16 + 8 * *栈序号 as i32;
                        if *参数类型 == 类型::浮点数64 {
                            文本.push_str(&format!(
                                "    movsd xmm0, [rbp+{}]  ; 参数 {}（栈传递，浮点）\n",
                                来源, 参数名
                            ));
                            文本.push_str(&format!("    movsd [rbp{}], xmm0\n", 槽));
                        } else {
                            文本.push_str(&format!(
                                "    mov rax, [rbp+{}]    ; 参数 {}（栈传递）\n",
                                来源, 参数名
                            ));
                            文本.push_str(&format!("    mov [rbp{}], rax\n", 槽));
                        }
                    }
                }
            }
        }
        for 行 in &self.代码缓冲 {
            文本.push_str(行);
            文本.push('\n');
        }
        文本.push_str(&format!("{}:\n", 返回标号));
        文本.push_str("    mov rsp, rbp\n");
        文本.push_str("    pop rbp\n");
        文本.push_str("    ret\n");
        self.函数体列表.push((注释.to_string(), 文本));
    }

    // ---------- 语句级生成 ----------

    fn 生成语句(&mut self, 语句: &语句) {
        match &语句.种类 {
            语句种类::设 { 变量名, 初值 } => {
                let 初值类型 = self.推断类型(初值);
                self.生成表达式(初值);
                self.存变量(变量名, 初值类型);
            }
            语句种类::带类型声明 {
                类型,
                变量名,
                初值,
            } => {
                let 初值类型 = self.推断类型(初值);
                self.生成表达式(初值);
                self.转换到(初值类型, *类型);
                self.存变量(变量名, *类型);
            }
            语句种类::赋值 {
                变量名,
                值,
                运算符,
            } => {
                let 目标类型 = *self.帧.类型表.get(变量名).unwrap_or(&类型::整数64);
                if let Some(运算符) = 运算符 {
                    // 复合赋值：等价于 变量名 运算符 值
                    let 左 = 表达式 {
                        种类: 表达式种类::变量(变量名.clone()),
                        行: 语句.行,
                        列: 1,
                    };
                    let 值类型 = self.推断类型(值);
                    self.生成二元(运算符, &左, 值);
                    self.转换到(值类型, 目标类型);
                    self.存变量(变量名, 目标类型);
                } else {
                    let 值类型 = self.推断类型(值);
                    self.生成表达式(值);
                    self.转换到(值类型, 目标类型);
                    self.存变量(变量名, 目标类型);
                }
            }
            语句种类::打印 { 参数, 换行 } => self.生成打印(参数, *换行),
            语句种类::返回(值) => match 值 {
                Some(表达式) => {
                    let 值类型 = self.推断类型(表达式);
                    self.生成表达式(表达式);
                    self.转换到(值类型, self.帧.返回类型);
                    self.行(&format!("    jmp {}          ; 返回", self.帧.返回标号));
                }
                None => {
                    self.行(&format!("    jmp {}", self.帧.返回标号));
                }
            },
            语句种类::如果 {
                条件,
                分支,
                否则分支,
            } => {
                let 否则标号 = self.新标号();
                let 结束标号 = self.新标号();
                let (类别, 运算符) = self.生成条件(条件);
                let 跳转 = 跳转指令(运算符, 类别, true); // 取反：条件不成立时跳走
                self.行(&format!("    {} {}       ; 条件不成立 → 否则", 跳转, 否则标号));
                for 语句 in 分支 {
                    self.生成语句(语句);
                }
                self.行(&format!("    jmp {}", 结束标号));
                self.行(&format!("    {}:", 否则标号));
                for 语句 in 否则分支 {
                    self.生成语句(语句);
                }
                self.行(&format!("    {}:", 结束标号));
            }
            语句种类::当 { 条件, 循环体 } => {
                let 循环标号 = self.新标号();
                let 结束标号 = self.新标号();
                self.行(&format!("    {}:", 循环标号));
                let (类别, 运算符) = self.生成条件(条件);
                let 跳转 = 跳转指令(运算符, 类别, true);
                self.行(&format!("    {} {}       ; 条件不成立 → 退出循环", 跳转, 结束标号));
                self.循环栈.push((循环标号.clone(), 结束标号.clone()));
                for 语句 in 循环体 {
                    self.生成语句(语句);
                }
                self.循环栈.pop();
                self.行(&format!("    jmp {}", 循环标号));
                self.行(&format!("    {}:", 结束标号));
            }
            语句种类::重复 { 次数, 循环体 } => {
                // 计数循环：先求值次数放入合成槽位，每次迭代减一，减到 0 退出
                let 槽 = self.分配隐藏槽();
                let 循环标号 = self.新标号();
                let 结束标号 = self.新标号();
                self.生成表达式(次数);
                self.行(&format!("    mov [rbp{}], rax      ; 剩余次数", 槽));
                self.行(&format!("    {}:", 循环标号));
                self.行(&format!("    cmp qword [rbp{}], 0  ; 次数用完？", 槽));
                self.行(&format!("    jle {}", 结束标号));
                self.循环栈.push((循环标号.clone(), 结束标号.clone()));
                for 语句 in 循环体 {
                    self.生成语句(语句);
                }
                self.循环栈.pop();
                self.行(&format!("    dec qword [rbp{}]      ; 剩余次数减一", 槽));
                self.行(&format!("    jmp {}", 循环标号));
                self.行(&format!("    {}:", 结束标号));
            }
            语句种类::跳出 => {
                if let Some((_, 结束标号)) = self.循环栈.last() {
                    self.行(&format!("    jmp {}          ; 跳出循环", 结束标号));
                }
            }
            语句种类::跳过 => {
                if let Some((循环标号, _)) = self.循环栈.last() {
                    self.行(&format!("    jmp {}          ; 跳过本次循环的剩余部分", 循环标号));
                }
            }
            语句种类::表达式语句(表达式) => {
                let _ = self.生成表达式(表达式);
            }
        }
    }

    /// 打印语句：多个参数同一行依次输出（无分隔符）；换行=true 时末尾换行。
    fn 生成打印(&mut self, 参数: &[表达式], 换行: bool) {
        let 数量 = 参数.len();
        if 数量 == 0 {
            if 换行 {
                let 空串 = self.字符串常量标签("");
                self.分配临时(32);
                self.行("    lea rcx, [__fmt_str]");
                self.行(&format!("    lea rdx, [{}]       ; 空串", 空串));
                self.行("    xor eax, eax          ; printf 变参：没有浮点参数");
                self.行("    call printf");
                self.释放临时(32);
            }
            return;
        }
        for (序号, 表达式) in 参数.iter().enumerate() {
            let 类型 = self.推断类型(表达式);
            self.生成表达式(表达式);
            let 末尾 = 序号 + 1 == 数量 && 换行;
            match 类型 {
                类型::浮点数64 => {
                    self.分配临时(32);
                    self.行(if 末尾 {
                        "    lea rcx, [__fmt_float]"
                    } else {
                        "    lea rcx, [__fmt_float_plain]"
                    });
                    self.行("    movq rdx, xmm0        ; 变参约定：浮点实参须在对应 GPR 中留位副本");
                    self.行("    mov al, 1            ; printf 变参：1 个浮点参数（xmm0）");
                    self.行("    call printf");
                    self.释放临时(32);
                }
                _ => {
                    self.行("    mov rdx, rax          ; 要打印的值");
                    self.分配临时(32);
                    let 格式 = match (类型 == 类型::字符串, 末尾) {
                        (true, true) => "__fmt_str",
                        (true, false) => "__fmt_str_plain",
                        (false, true) => "__fmt_int",
                        (false, false) => "__fmt_int_plain",
                    };
                    self.行(&format!("    lea rcx, [{}]", 格式));
                    self.行("    xor eax, eax          ; printf 变参：没有浮点参数");
                    self.行("    call printf");
                    self.释放临时(32);
                }
            }
        }
    }

    /// 存变量：浮点存 xmm0，其余存 rax。
    fn 存变量(&mut self, 变量名: &str, 类型: 类型) {
        let 槽 = self.帧.槽位偏移[变量名];
        if 类型 == 类型::浮点数64 {
            self.行(&format!("    movsd [rbp{}], xmm0  ; 变量 {}", 槽, 变量名));
        } else {
            self.行(&format!("    mov [rbp{}], rax      ; 变量 {}", 槽, 变量名));
        }
    }

    // ---------- 表达式级生成 ----------

    /// 推断表达式的类型（语义分析已保证没有错误，这里放心查表）。
    fn 推断类型(&self, 表达式: &表达式) -> 类型 {
        match &表达式.种类 {
            表达式种类::整数字面量(_) => 类型::整数64,
            表达式种类::浮点字面量(_) => 类型::浮点数64,
            表达式种类::字符串字面量(_) => 类型::字符串,
            表达式种类::真 | 表达式种类::假 => 类型::布尔,
            表达式种类::变量(名) => *self.帧.类型表.get(名).unwrap_or(&类型::整数64),
            表达式种类::取负(内) => self.推断类型(内),
            表达式种类::二元 { 运算符, 左, 右 } => {
                if 运算符.是比较() {
                    return 类型::布尔;
                }
                let 左类型 = self.推断类型(左);
                let 右类型 = self.推断类型(右);
                if 左类型 == 类型::字符串 || 右类型 == 类型::字符串 {
                    return 类型::字符串; // 仅 + 拼接会走到这里（语义层已排除其他运算）
                }
                if 左类型 == 类型::浮点数64 || 右类型 == 类型::浮点数64 {
                    类型::浮点数64
                } else {
                    类型::整数64
                }
            }
            表达式种类::逻辑且 { .. } | 表达式种类::逻辑或 { .. } | 表达式种类::逻辑非(_) => {
                类型::布尔
            }
            表达式种类::调用 { 函数名, .. } => self
                .语义
                .函数表
                .get(函数名)
                .map(|信息| 信息.返回类型)
                .unwrap_or(类型::整数64),
        }
    }

    fn 生成表达式(&mut self, 表达式: &表达式) {
        match &表达式.种类 {
            表达式种类::整数字面量(值) => {
                self.行(&format!("    mov rax, {}          ; 整数 {}", 值, 值))
            }
            表达式种类::浮点字面量(值) => {
                let 标签 = self.浮点常量标签(值.to_bits());
                self.行(&format!("    movsd xmm0, [{}]    ; 浮点数 {}", 标签, 值));
            }
            表达式种类::字符串字面量(内容) => {
                let 标签 = self.字符串常量标签(内容);
                self.行(&format!("    lea rax, [{}]       ; 字符串“{}”", 标签, 转义显示(内容)));
            }
            表达式种类::真 => self.行("    mov rax, 1            ; 真"),
            表达式种类::假 => self.行("    mov rax, 0            ; 假"),
            表达式种类::变量(名) => {
                let 槽 = self.帧.槽位偏移[名];
                let 类型 = self.帧.类型表[名];
                if 类型 == 类型::浮点数64 {
                    self.行(&format!("    movsd xmm0, [rbp{}]  ; 变量 {}", 槽, 名));
                } else {
                    self.行(&format!("    mov rax, [rbp{}]      ; 变量 {}", 槽, 名));
                }
            }
            表达式种类::取负(内) => {
                self.生成表达式(内);
                if self.推断类型(内) == 类型::浮点数64 {
                    self.行("    mulsd xmm0, [__f_neg1] ; 取负");
                } else {
                    self.行("    neg rax              ; 取负");
                }
            }
            表达式种类::逻辑且 { 左, 右 } => self.生成逻辑且(左, 右),
            表达式种类::逻辑或 { 左, 右 } => self.生成逻辑或(左, 右),
            表达式种类::逻辑非(内) => {
                self.生成表达式(内);
                self.行("    test rax, rax");
                self.行("    setz al");
                self.行("    movzx eax, al         ; 非 的结果 1/0");
            }
            表达式种类::二元 { 运算符, 左, 右 } => self.生成二元(运算符, 左, 右),
            表达式种类::调用 { 函数名, 参数 } => self.生成调用(函数名, 参数),
        }
    }

    /// 值类型转换（整数 ↔ 浮点；字符串参与转换的写法已在语义层拒绝）。
    fn 转换到(&mut self, 来源: 类型, 目标: 类型) {
        if 来源 == 目标 {
            return;
        }
        match (来源 == 类型::浮点数64, 目标 == 类型::浮点数64) {
            (true, false) => self.行("    cvttsd2si rax, xmm0   ; 浮点 → 整数（截断）"),
            (false, true) => self.行("    cvtsi2sd xmm0, rax    ; 整数 → 浮点"),
            _ => {} // 整数家族（整数64/整数32/布尔）之间无需转换
        }
    }

    /// 把当前值（整数/布尔在 rax、浮点在 xmm0、字符串指针在 rax）转换成字符串指针。
    fn 转换到字符串(&mut self, 来源: 类型) {
        if 来源 == 类型::字符串 {
            return;
        }
        self.分配临时(32);
        if 来源 == 类型::浮点数64 {
            self.行("    call _jy_float_to_str  ; 浮点 → 字符串");
        } else {
            self.行("    mov rcx, rax");
            self.行("    call _jy_int_to_str    ; 整数 → 字符串");
        }
        self.释放临时(32);
        self.用了数字转字符串 = true;
    }

    /// 逻辑与：短路求值（左为假时不再求值右），结果 1/0。
    fn 生成逻辑且(&mut self, 左: &表达式, 右: &表达式) {
        let 假标号 = self.新标号();
        let 结束标号 = self.新标号();
        self.生成表达式(左);
        self.行("    test rax, rax");
        self.行(&format!("    jz {}           ; 左为假 → 短路，结果 0", 假标号));
        self.生成表达式(右);
        self.行("    test rax, rax");
        self.行(&format!("    jz {}", 假标号));
        self.行("    mov rax, 1");
        self.行(&format!("    jmp {}", 结束标号));
        self.行(&format!("    {}:", 假标号));
        self.行("    xor eax, eax");
        self.行(&format!("    {}:", 结束标号));
    }

    /// 逻辑或：短路求值（左为真时不再求值右），结果 1/0。
    fn 生成逻辑或(&mut self, 左: &表达式, 右: &表达式) {
        let 真标号 = self.新标号();
        let 结束标号 = self.新标号();
        self.生成表达式(左);
        self.行("    test rax, rax");
        self.行(&format!("    jnz {}          ; 左为真 → 短路，结果 1", 真标号));
        self.生成表达式(右);
        self.行("    test rax, rax");
        self.行(&format!("    jnz {}", 真标号));
        self.行("    xor eax, eax");
        self.行(&format!("    jmp {}", 结束标号));
        self.行(&format!("    {}:", 真标号));
        self.行("    mov rax, 1");
        self.行(&format!("    {}:", 结束标号));
    }

    fn 生成二元(&mut self, 运算符: &二元运算符, 左: &表达式, 右: &表达式) {
        // 比较运算：结果为布尔值 1/0（字符串 ==/!= 走 strcmp）
        if 运算符.是比较() {
            let 类别 = self.生成比较(左, 右);
            let 后缀 = 比较后缀(*运算符, 类别);
            self.行(&format!("    set{} al", 后缀));
            self.行(&format!("    movzx eax, al         ; {} 比较结果 1/0", 运算符.符号()));
            return;
        }
        // 字符串拼接：任意一侧是字符串时，另一侧自动转成字符串再拼接
        let 左字符串 = self.推断类型(左) == 类型::字符串;
        let 右字符串 = self.推断类型(右) == 类型::字符串;
        if 左字符串 || 右字符串 {
            let 临时 = self.分配临时(16);
            self.生成表达式(左);
            self.转换到字符串(self.推断类型(左));
            self.行(&format!("    mov [rbp{}], rax      ; 暂存左串", 临时));
            self.生成表达式(右);
            self.转换到字符串(self.推断类型(右));
            self.行("    mov rdx, rax          ; 右串");
            self.行(&format!("    mov rcx, [rbp{}]    ; 左串", 临时));
            self.释放临时(16);
            self.分配临时(32);
            self.行("    call _jy_str_concat");
            self.释放临时(32);
            self.用了拼接 = true;
            return;
        }
        let 浮点运算 =
            self.推断类型(左) == 类型::浮点数64 || self.推断类型(右) == 类型::浮点数64;
        let 临时 = self.分配临时(16);
        self.生成表达式(左);
        if 浮点运算 {
            if self.推断类型(左) != 类型::浮点数64 {
                self.行("    cvtsi2sd xmm0, rax");
            }
            self.行(&format!("    movsd [rbp{}], xmm0  ; 暂存左操作数", 临时));
            self.生成表达式(右);
            if self.推断类型(右) != 类型::浮点数64 {
                self.行("    cvtsi2sd xmm0, rax");
            }
            self.行("    movsd xmm1, xmm0      ; 右操作数");
            self.行(&format!("    movsd xmm0, [rbp{}]  ; 左操作数", 临时));
            self.释放临时(16);
            match 运算符 {
                二元运算符::加 => self.行("    addsd xmm0, xmm1"),
                二元运算符::减 => self.行("    subsd xmm0, xmm1"),
                二元运算符::乘 => self.行("    mulsd xmm0, xmm1"),
                二元运算符::除 => self.行("    divsd xmm0, xmm1"),
                _ => {}
            }
            return;
        }
        // 整数运算
        self.行(&format!("    mov [rbp{}], rax      ; 暂存左操作数", 临时));
        self.生成表达式(右);
        match 运算符 {
            二元运算符::除 | 二元运算符::取余 => {
                self.行("    mov rcx, rax          ; 除数");
                self.行(&format!("    mov rax, [rbp{}]    ; 被除数", 临时));
                self.释放临时(16);
                self.行("    cqo                   ; 符号扩展 rdx:rax");
                self.行("    idiv rcx              ; 商 → rax，余数 → rdx");
                if matches!(运算符, 二元运算符::取余) {
                    self.行("    mov rax, rdx          ; 取余：余数 → rax");
                }
            }
            _ => {
                self.行(&format!("    mov rcx, [rbp{}]    ; 左操作数", 临时));
                self.释放临时(16);
                match 运算符 {
                    二元运算符::加 => {
                        self.行("    add rcx, rax");
                        self.行("    mov rax, rcx");
                    }
                    二元运算符::减 => {
                        self.行("    sub rcx, rax");
                        self.行("    mov rax, rcx");
                    }
                    二元运算符::乘 => {
                        self.行("    imul rcx, rax");
                        self.行("    mov rax, rcx");
                    }
                    _ => {}
                }
            }
        }
    }

    /// 生成比较：把左、右操作数比较后把标志位留下（cmp / ucomisd / strcmp+test）。
    fn 生成比较(&mut self, 左: &表达式, 右: &表达式) -> 比较类别 {
        // 字符串比较（语义层保证两侧都是字符串、运算符为 == / !=）
        if self.推断类型(左) == 类型::字符串 {
            let 临时 = self.分配临时(16);
            self.生成表达式(左);
            self.行(&format!("    mov [rbp{}], rax      ; 暂存左串", 临时));
            self.生成表达式(右);
            self.行("    mov rdx, rax          ; 右串");
            self.行(&format!("    mov rcx, [rbp{}]    ; 左串", 临时));
            self.释放临时(16);
            self.分配临时(32);
            self.行("    call strcmp");
            self.释放临时(32);
            self.行("    test eax, eax          ; 两串相等则 ZF=1");
            self.用了字符串比较 = true;
            return 比较类别::字符串;
        }
        let 浮点比较 =
            self.推断类型(左) == 类型::浮点数64 || self.推断类型(右) == 类型::浮点数64;
        let 临时 = self.分配临时(16);
        self.生成表达式(左);
        if 浮点比较 {
            if self.推断类型(左) != 类型::浮点数64 {
                self.行("    cvtsi2sd xmm0, rax");
            }
            self.行(&format!("    movsd [rbp{}], xmm0", 临时));
            self.生成表达式(右);
            if self.推断类型(右) != 类型::浮点数64 {
                self.行("    cvtsi2sd xmm0, rax");
            }
            self.行("    movsd xmm1, xmm0");
            self.行(&format!("    movsd xmm0, [rbp{}]", 临时));
            self.释放临时(16);
            self.行("    ucomisd xmm0, xmm1    ; 比较 左 ? 右");
            比较类别::浮点
        } else {
            self.行(&format!("    mov [rbp{}], rax", 临时));
            self.生成表达式(右);
            self.行(&format!("    mov rcx, [rbp{}]", 临时));
            self.释放临时(16);
            self.行("    cmp rcx, rax          ; 比较 左 ? 右");
            比较类别::整数
        }
    }

    /// 生成 如果/当 的条件：直接比较（或与 0 比较），把标志位留给跳转指令。
    /// 返回（比较类别, 语义上的比较运算符）供跳转/取反使用。
    fn 生成条件(&mut self, 条件: &表达式) -> (比较类别, 二元运算符) {
        if let 表达式种类::二元 { 运算符, 左, 右 } = &条件.种类 {
            if 运算符.是比较() {
                let 类别 = self.生成比较(左, 右);
                return (类别, *运算符);
            }
        }
        // 普通表达式作为条件：非零为真
        let 类型 = self.推断类型(条件);
        self.生成表达式(条件);
        if 类型 == 类型::浮点数64 {
            self.行("    ucomisd xmm0, [__f_zero] ; 与 0.0 比较");
            (比较类别::浮点, 二元运算符::不等于)
        } else {
            self.行("    cmp rax, 0             ; 与 0 比较");
            (比较类别::整数, 二元运算符::不等于)
        }
    }

    /// 生成函数调用（win64 调用约定）。
    fn 生成调用(&mut self, 函数名: &str, 实参: &[表达式]) {
        // 先把共享引用提出，避免与 &mut self 冲突
        let 语义: &'a 语义结果 = self.语义;
        let 信息 = &语义.函数表[函数名];
        let 符号 = self.函数符号[函数名].clone();
        // 构造形参列表（只需类型，用于分配参数位置）
        let 形参: Vec<(类型, String)> = 信息
            .参数类型
            .iter()
            .enumerate()
            .map(|(序号, &类型)| (类型, format!("arg{}", 序号)))
            .collect();
        let 位置表 = 分配参数位置(&形参);
        // 临时区：32 字节影子空间 + 每个实参 8 字节槽，整体对齐 16
        let 总字节 = 对齐16(32 + 8 * 实参.len() as i32);
        let 基址 = self.分配临时(总字节);
        // 逐个求值实参并放入槽（求值顺序从左到右）
        for (序号, 实参) in 实参.iter().enumerate() {
            let 目标类型 = 信息.参数类型[序号];
            let 来源类型 = self.推断类型(实参);
            self.生成表达式(实参);
            self.转换到(来源类型, 目标类型);
            let 槽 = 基址 + 32 + 8 * 序号 as i32;
            if 目标类型 == 类型::浮点数64 {
                self.行(&format!("    movsd [rbp{}], xmm0  ; 实参 {}", 槽, 序号 + 1));
            } else {
                self.行(&format!("    mov [rbp{}], rax    ; 实参 {}", 槽, 序号 + 1));
            }
        }
        // 寄存器参数：从槽装入寄存器（必须在重排栈参数之前，重排可能覆盖槽）
        for (序号, 位置) in 位置表.iter().enumerate() {
            let 槽 = 基址 + 32 + 8 * 序号 as i32;
            match 位置 {
                参数位置::整数寄存器(寄存器序号) => self.行(&format!(
                    "    mov {}, [rbp{}]      ; 实参 {}",
                    整数参数寄存器[*寄存器序号],
                    槽,
                    序号 + 1
                )),
                参数位置::浮点寄存器(寄存器序号) => self.行(&format!(
                    "    movsd {}, [rbp{}]    ; 实参 {}（浮点）",
                    浮点参数寄存器[*寄存器序号],
                    槽,
                    序号 + 1
                )),
                参数位置::栈(_) => {}
            }
        }
        // 栈参数：把槽重排到影子空间之后。目标槽永远 ≤ 源槽，且按序号从小到大
        // 处理（先读后写、只向下搬），保证不互相覆盖。
        for (序号, 位置) in 位置表.iter().enumerate() {
            if let 参数位置::栈(栈序号) = 位置 {
                let 源 = 基址 + 32 + 8 * 序号 as i32;
                let 目的 = 基址 + 32 + 8 * *栈序号 as i32;
                if 源 != 目的 {
                    if 信息.参数类型[序号] == 类型::浮点数64 {
                        self.行(&format!("    movsd xmm7, [rbp{}]", 源));
                        self.行(&format!("    movsd [rbp{}], xmm7", 目的));
                    } else {
                        self.行(&format!("    mov rax, [rbp{}]", 源));
                        self.行(&format!("    mov [rbp{}], rax", 目的));
                    }
                }
            }
        }
        self.行(&format!("    call {}", 符号));
        self.释放临时(总字节);
    }
}

// ---------- 杂项辅助 ----------

/// 比较运算符 → 汇编条件码后缀（整数用带符号比较，浮点/字符串用无符号/符号位比较）。
fn 比较后缀(运算符: 二元运算符, 类别: 比较类别) -> &'static str {
    match (运算符, 类别) {
        (二元运算符::等于, _) => "e",
        (二元运算符::不等于, _) => "ne",
        (二元运算符::小于, 比较类别::整数) => "l",
        (二元运算符::小于, 比较类别::浮点) => "b",
        (二元运算符::小于, 比较类别::字符串) => "l",
        (二元运算符::大于, 比较类别::整数) => "g",
        (二元运算符::大于, 比较类别::浮点) => "a",
        (二元运算符::大于, 比较类别::字符串) => "g",
        (二元运算符::小于等于, 比较类别::整数) => "le",
        (二元运算符::小于等于, 比较类别::浮点) => "be",
        (二元运算符::小于等于, 比较类别::字符串) => "le",
        (二元运算符::大于等于, 比较类别::整数) => "ge",
        (二元运算符::大于等于, 比较类别::浮点) => "ae",
        (二元运算符::大于等于, 比较类别::字符串) => "ge",
        _ => "e", // 不可达（算术运算符不会走到这里）
    }
}

/// 条件取反（用于“条件不成立就跳走”的分支）。
fn 取反比较(运算符: 二元运算符) -> 二元运算符 {
    match 运算符 {
        二元运算符::等于 => 二元运算符::不等于,
        二元运算符::不等于 => 二元运算符::等于,
        二元运算符::小于 => 二元运算符::大于等于,
        二元运算符::大于 => 二元运算符::小于等于,
        二元运算符::小于等于 => 二元运算符::大于,
        二元运算符::大于等于 => 二元运算符::小于,
        _ => 运算符,
    }
}

/// 生成跳转指令文本：`取反` 为真时跳“条件不成立”的分支。
fn 跳转指令(运算符: 二元运算符, 类别: 比较类别, 取反: bool) -> String {
    let 实际 = if 取反 { 取反比较(运算符) } else { 运算符 };
    format!("j{}", 比较后缀(实际, 类别))
}

/// 把字符串字节序列化成可读的 NASM db 行（可打印 ASCII 用引号，其余用 0xHH）。
fn 序列化字节(字节: &[u8]) -> String {
    let mut 片段: Vec<String> = Vec::new();
    let mut 当前文本 = String::new();
    for &b in 字节 {
        if b.is_ascii_graphic() && b != b'"' && b != b'`' {
            当前文本.push(b as char);
        } else {
            if !当前文本.is_empty() {
                片段.push(format!("\"{}\"", 当前文本));
                当前文本.clear();
            }
            片段.push(format!("0x{:02X}", b));
        }
    }
    if !当前文本.is_empty() {
        片段.push(format!("\"{}\"", 当前文本));
    }
    片段.push("0".to_string());
    format!("db {}", 片段.join(", "))
}

/// 转义显示（用于注释里的字符串原文）。
fn 转义显示(内容: &str) -> String {
    内容
        .chars()
        .map(|字符| match 字符 {
            '\n' => "\\n".to_string(),
            '\t' => "\\t".to_string(),
            '\r' => "\\r".to_string(),
            '\\' => "\\\\".to_string(),
            '"' => "\\\"".to_string(),
            _ => 字符.to_string(),
        })
        .collect()
}

/// 函数注释头（中文函数名 + 分隔线）。
fn 格式函数头(注释: &str) -> String {
    let 横线 = format!("; {}", "-".repeat(72));
    format!("{}\n; {}\n{}\n", 横线, 注释, 横线)
}

// ---------- 运行时辅助函数（字符串拼接 / 数值转字符串，按需生成）----------
//
// 这些是编译器生成进 .text 的小型运行时：全部遵循 win64 调用约定，
// 内部只用易失寄存器（rax/rcx/rdx/r8-r11/xmm），新串一律 malloc 分配。
// 教学编译器不做垃圾回收：拼接产生的临时串不会被释放（程序级内存泄漏）。

/// 运行时：字符串拼接 _jy_str_concat(rcx=左串, rdx=右串) → rax=新串。
fn 运行时_字符串拼接() -> String {
    r#"
; ------------------------------------------------------------------------
; 运行时辅助：字符串拼接（rcx=左串, rdx=右串 → rax=新串，malloc 分配不释放）
; ------------------------------------------------------------------------
_jy_str_concat:
    push rbp
    mov rbp, rsp
    sub rsp, 64
    mov [rbp-8], rcx
    mov [rbp-16], rdx
    ; 计算左串长度
    sub rsp, 32
    call strlen
    add rsp, 32
    mov [rbp-24], rax
    ; 计算右串长度
    mov rcx, [rbp-16]
    sub rsp, 32
    call strlen
    add rsp, 32
    ; 总长 = 左 + 右 + 1（结尾 0）
    add rax, [rbp-24]
    inc rax
    mov rcx, rax
    sub rsp, 32
    call malloc
    add rsp, 32
    mov [rbp-32], rax
    ; 复制左串
    mov rcx, rax
    mov rdx, [rbp-8]
    sub rsp, 32
    call strcpy
    add rsp, 32
    ; 追加右串
    mov rcx, [rbp-32]
    mov rdx, [rbp-16]
    sub rsp, 32
    call strcat
    add rsp, 32
    mov rax, [rbp-32]
    mov rsp, rbp
    pop rbp
    ret
"#
    .to_string()
}

/// 运行时：整数 → 十进制字符串 _jy_int_to_str(rcx=整数) → rax=新串。
fn 运行时_整数转字符串() -> String {
    r#"
; ------------------------------------------------------------------------
; 运行时辅助：整数 → 十进制字符串（rcx=整数 → rax=新串，malloc 分配不释放）
; ------------------------------------------------------------------------
_jy_int_to_str:
    push rbp
    mov rbp, rsp
    sub rsp, 96
    mov [rbp-8], rcx
    ; sprintf(栈上缓冲区, "%lld", 整数)
    lea rcx, [rbp-64]
    lea rdx, [__fmt_ll]
    mov r8, [rbp-8]
    xor eax, eax
    sub rsp, 32
    call sprintf
    add rsp, 32
    ; 复制到 malloc 区
    lea rcx, [rbp-64]
    sub rsp, 32
    call strlen
    add rsp, 32
    inc rax
    mov rcx, rax
    sub rsp, 32
    call malloc
    add rsp, 32
    mov rcx, rax
    lea rdx, [rbp-64]
    sub rsp, 32
    call strcpy
    add rsp, 32
    mov rsp, rbp
    pop rbp
    ret
"#
    .to_string()
}

/// 运行时：浮点数 → 字符串 _jy_float_to_str(xmm0=double) → rax=新串。
fn 运行时_浮点转字符串() -> String {
    r#"
; ------------------------------------------------------------------------
; 运行时辅助：浮点数 → 字符串（xmm0=double → rax=新串，malloc 分配不释放）
; ------------------------------------------------------------------------
_jy_float_to_str:
    push rbp
    mov rbp, rsp
    sub rsp, 96
    movsd [rbp-8], xmm0
    ; sprintf(栈上缓冲区, "%g", 浮点数)
    lea rcx, [rbp-64]
    lea rdx, [__fmt_g]
    movsd xmm0, [rbp-8]
    movq r8, xmm0           ; 变参约定：浮点实参须在对应 GPR 中留位副本
    mov al, 1
    sub rsp, 32
    call sprintf
    add rsp, 32
    ; 复制到 malloc 区
    lea rcx, [rbp-64]
    sub rsp, 32
    call strlen
    add rsp, 32
    inc rax
    mov rcx, rax
    sub rsp, 32
    call malloc
    add rsp, 32
    mov rcx, rax
    lea rdx, [rbp-64]
    sub rsp, 32
    call strcpy
    add rsp, 32
    mov rsp, rbp
    pop rbp
    ret
"#
    .to_string()
}
