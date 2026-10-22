//! 集成测试：通过命令行完整走一遍编译闭环。
//!
//! 需要 ./tools/nasm.exe 与 ./tools/GoLink.exe 才能跑“完整构建”类用例；
//! 工具缺失时这类用例会打印提示并跳过（不判失败）。
//! “仅生成汇编”与“错误诊断”类用例不依赖工具，始终执行。
//!
//! 注意：cargo test 会并行执行多个用例，因此每个用例都在自己独立的
//! 临时目录里编译（输出文件与源文件同目录），避免互相踩踏产物。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 编译器可执行文件路径（cargo 自动注入）。
fn 编译器() -> &'static str {
    env!("CARGO_BIN_EXE_jianyan")
}

/// 项目根目录。
fn 项目根() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 工具链是否齐全。
fn 工具齐全() -> bool {
    项目根().join("tools/nasm.exe").is_file() && 项目根().join("tools/GoLink.exe").is_file()
}

/// 为本用例建立一个独立的临时工作目录（每个进程唯一的目录名）。
fn 临时工作目录(用例名: &str) -> PathBuf {
    let 目录 = std::env::temp_dir().join(format!(
        "jianyan_test_{}_{}",
        用例名,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&目录);
    std::fs::create_dir_all(&目录).unwrap();
    目录
}

/// 把源文件复制到指定目录（连同相对路径解析所需的伴生文件）。
fn 复制源文件(来源: &Path, 目标目录: &Path) {
    std::fs::copy(来源, 目标目录.join(来源.file_name().unwrap())).unwrap();
}

/// 把输出里的 \r\n 统一成 \n（msvcrt 走管道时是文本模式，会做 CRLF 转换）。
fn 标准化换行(字节: &[u8]) -> String {
    String::from_utf8_lossy(字节).replace("\r\n", "\n")
}

#[test]
fn 模式a_只生成汇编不调用工具链() {
    let 根 = 项目根();
    let _ = std::fs::remove_file(根.join("demo.asm"));
    let 输出 = Command::new(编译器())
        .arg("-S")
        .arg(根.join("demo.jy"))
        .output()
        .unwrap();
    assert!(
        输出.status.success(),
        "编译失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    let asm = 根.join("demo.asm");
    assert!(asm.is_file(), "应当生成 demo.asm");
    let 文本 = std::fs::read_to_string(&asm).unwrap();
    assert!(文本.contains("bits 64"));
    assert!(文本.contains("section .rdata"));
    assert!(文本.contains("section .text"));
    assert!(文本.contains("global main"));
    assert!(文本.contains("extern printf"));
    assert!(文本.contains("_jy_fn0"), "函数 阶乘 应映射为 _jy_fn0");
    assert!(!根.join("demo.obj").exists(), "-S 模式不应生成 obj");
    assert!(!根.join("demo.exe").exists(), "-S 模式不应生成 exe");
    let _ = std::fs::remove_file(&asm);
}

#[test]
fn 完整构建_阶乘演示输出120() {
    if !工具齐全() {
        eprintln!("跳过：缺少 tools/nasm.exe 或 tools/GoLink.exe");
        return;
    }
    let 目录 = 临时工作目录("demo");
    复制源文件(&项目根().join("demo.jy"), &目录);
    let 源 = 目录.join("demo.jy");
    let 输出 = Command::new(编译器()).arg(&源).output().unwrap();
    assert!(
        输出.status.success(),
        "构建失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    let exe = 源.with_extension("exe");
    assert!(exe.is_file(), "应当生成 demo.exe");
    assert!(!源.with_extension("asm").exists(), "默认应清理中间产物 asm");
    assert!(!源.with_extension("obj").exists(), "默认应清理中间产物 obj");
    let 运行 = Command::new(&exe).output().unwrap();
    assert_eq!(标准化换行(&运行.stdout), "120\n");
    let _ = std::fs::remove_dir_all(&目录);
}

#[test]
fn 保留中间产物参数生效() {
    if !工具齐全() {
        eprintln!("跳过：缺少 tools/nasm.exe 或 tools/GoLink.exe");
        return;
    }
    let 目录 = 临时工作目录("keep_temp");
    复制源文件(&项目根().join("demo.jy"), &目录);
    let 源 = 目录.join("demo.jy");
    let 输出 = Command::new(编译器())
        .arg("--keep-temp")
        .arg(&源)
        .output()
        .unwrap();
    assert!(
        输出.status.success(),
        "构建失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    assert!(源.with_extension("asm").is_file(), "--keep-temp 应保留 asm");
    assert!(源.with_extension("obj").is_file(), "--keep-temp 应保留 obj");
    let _ = std::fs::remove_dir_all(&目录);
}

#[test]
fn 导入示例完整构建() {
    if !工具齐全() {
        eprintln!("跳过：缺少 tools/nasm.exe 或 tools/GoLink.exe");
        return;
    }
    let 目录 = 临时工作目录("import");
    复制源文件(&项目根().join("examples/ex4_main.jy"), &目录);
    复制源文件(&项目根().join("examples/ex4_工具.jy"), &目录);
    let 源 = 目录.join("ex4_main.jy");
    let 输出 = Command::new(编译器()).arg(&源).output().unwrap();
    assert!(
        输出.status.success(),
        "构建失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    let 运行 = Command::new(源.with_extension("exe")).output().unwrap();
    assert_eq!(标准化换行(&运行.stdout), "42\n导入成功\n");
    let _ = std::fs::remove_dir_all(&目录);
}

#[test]
fn 错误诊断_带中文与行号() {
    // 这些用例只用 -S 模式（不需要工具链）
    let 用例: &[(&str, &str)] = &[
        ("未定义变量", "变量未定义"),
        ("缩进错误", "缩进"),
        ("参数不匹配", "参数数量不匹配"),
        ("符号重复", "符号重复定义"),
        ("缺少冒号", "『：』"),
        ("字符串运算", "字符串只支持"),
        ("跳出循环外", "跳出"),
    ];
    for (文件名, 关键词) in 用例 {
        let 源 = 项目根().join(format!("tests/fixtures/{}.jy", 文件名));
        let 输出 = Command::new(编译器()).arg("-S").arg(&源).output().unwrap();
        assert!(!输出.status.success(), "用例『{}』应当编译失败", 文件名);
        let 标准错误 = String::from_utf8_lossy(&输出.stderr);
        assert!(
            标准错误.contains(关键词),
            "用例『{}』的报错应包含『{}』，实际输出：\n{}",
            文件名,
            关键词,
            标准错误
        );
        assert!(
            标准错误.contains(&format!("{}.jy", 文件名)),
            "报错应带文件名，实际输出：\n{}",
            标准错误
        );
        // 清理可能残留的 asm
        let _ = std::fs::remove_file(源.with_extension("asm"));
    }
}

#[test]
fn 中文引号与类型别名() {
    // 只用 -S 模式（不需要工具链）：中文弯引号 “ ” 应归一化为字符串定界符
    let 源 = 项目根().join("tests/fixtures/中文引号.jy");
    let 输出 = Command::new(编译器()).arg("-S").arg(&源).output().unwrap();
    assert!(
        输出.status.success(),
        "编译失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    let 汇编 = std::fs::read_to_string(源.with_extension("asm")).unwrap();
    assert!(汇编.contains("__str0"), "应生成字符串常量 __str0");
    assert!(汇编.contains("0xE4"), "中文字符应以 UTF-8 字节形式出现在汇编里");
    assert!(汇编.contains("__str1"), "混合配对的两个字符串都应生成");
    let _ = std::fs::remove_file(源.with_extension("asm"));
}

#[test]
fn 数学插值示例完整构建() {
    if !工具齐全() {
        eprintln!("跳过：缺少 tools/nasm.exe 或 tools/GoLink.exe");
        return;
    }
    let 目录 = 临时工作目录("math");
    复制源文件(&项目根().join("examples/ex5_数学插值.jy"), &目录);
    let 源 = 目录.join("ex5_数学插值.jy");
    let 输出 = Command::new(编译器()).arg(&源).output().unwrap();
    assert!(
        输出.status.success(),
        "构建失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    let 运行 = Command::new(源.with_extension("exe")).output().unwrap();
    assert_eq!(标准化换行(&运行.stdout), "16\n3\n你好，简言！\n");
    let _ = std::fs::remove_dir_all(&目录);
}

#[test]
fn 逻辑与字符串示例完整构建() {
    if !工具齐全() {
        eprintln!("跳过：缺少 tools/nasm.exe 或 tools/GoLink.exe");
        return;
    }
    let 目录 = 临时工作目录("logic");
    复制源文件(&项目根().join("examples/ex6_逻辑与字符串.jy"), &目录);
    let 源 = 目录.join("ex6_逻辑与字符串.jy");
    let 输出 = Command::new(编译器()).arg(&源).output().unwrap();
    assert!(
        输出.status.success(),
        "构建失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    let 运行 = Command::new(源.with_extension("exe")).output().unwrap();
    let 期望 = "结果是 120！\n\
               拼接：你好世界\n\
               及格\n\
               甲 > 0 且 乙 > 1\n\
               甲 < 0 或 乙 > 1\n\
               1\n\
               字符串比较相等\n\
               圆周率 = 3.14\n";
    assert_eq!(标准化换行(&运行.stdout), 期望);
    let _ = std::fs::remove_dir_all(&目录);
}

#[test]
fn 循环与运算符示例完整构建() {
    if !工具齐全() {
        eprintln!("跳过：缺少 tools/nasm.exe 或 tools/GoLink.exe");
        return;
    }
    let 目录 = 临时工作目录("loops");
    复制源文件(&项目根().join("examples/ex7_循环与运算符.jy"), &目录);
    let 源 = 目录.join("ex7_循环与运算符.jy");
    let 输出 = Command::new(编译器()).arg(&源).output().unwrap();
    assert!(
        输出.status.success(),
        "构建失败：{}",
        String::from_utf8_lossy(&输出.stderr)
    );
    let 运行 = Command::new(源.with_extension("exe")).output().unwrap();
    let 期望 = "1 到 100 的和 = 5050\n\
               重复\n\
               重复\n\
               重复\n\
               2\n\
               4\n\
               5\n\
               取余：1\n\
               复合赋值：6\n\
               1\n\
               布尔与取反正常\n\
               字典序：1\n\
               不换行：12 结束\n";
    assert_eq!(标准化换行(&运行.stdout), 期望);
    let _ = std::fs::remove_dir_all(&目录);
}
