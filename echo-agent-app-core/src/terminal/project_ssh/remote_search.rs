//! SSH 项目的内容搜索提供者：在目标机器上完成匹配，只回传展示记录。
//!
//! 背景：`grep` 工具的默认实现枚举目录、逐文件读取正文再在本机匹配。对 SSH
//! 项目来说，这意味着把整棵候选树的正文经 SFTP 拉回本机。本提供者把“枚举与
//! 匹配”下发到远端，只回传已经排版好的结果记录。
//!
//! 为什么不用 `git grep`：本机遍历（`grep.rs::walk_and_search`）根本不读
//! `.gitignore`，而 `git grep --untracked` 仍只覆盖“已跟踪 + 未被忽略的未跟踪
//! 文件”，被忽略目录里的命中会静默消失；`git grep` 也没有单文件 10MB 上限，
//! 且非 git 目录完全不可用。为了与回退路径逐行一致，这里统一用
//! `find` + `grep`：`find` 复现忽略项、大小上限和文件名 glob，`grep` 只在
//! 筛选后的文件上做正则匹配。
//!
//! 传输格式：每个候选行输出 `path\0line<sep>content`，`<sep>` 为 `:` 表示
//! 命中行、`-` 表示上下文行（与 `grep --null` 的默认分隔一致）。路径用 NUL
//! 与行号分隔，避免路径里的冒号/连字符破坏解析；上下文分组的分隔行 `--`
//! 本地直接忽略。远端不发明新的截断协议：输出体积只受 `SandboxExecutor`
//! 既有输出上限约束，解析侧按 `output_truncated` 丢弃可能不完整的最后一组。

use echo_agent::error::{ReactError, Result, ToolError};
use echo_agent::sandbox::{ExecutionResult, SandboxCommand, SandboxExecutor};
use echo_agent::tools::search::{
    ContentSearchOutcome, ContentSearchProvider, ContentSearchRequest,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 与 `grep.rs::walk_and_search` 完全一致的忽略目录/文件名。
///
/// 这里刻意不引入 `.gitignore`、`.ignore` 等新规则：提供者的结果必须能被本机
/// 回退路径复现，多一条规则就会让两条路径给出不同命中集合。
const IGNORED_ENTRY_NAMES: &[&str] = &[
    "target",
    "node_modules",
    "__pycache__",
    ".git",
    "vendor",
    "build",
    "dist",
];

/// 单文件大小上限，与 `GrepTool` 的 `10 * 1024 * 1024` 一致；`>` 上限才跳过。
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// 通过环境变量把正则传给内层 `sh`，避免把它拼进命令字符串而受引号影响。
const PATTERN_ENV: &str = "EKO_REMOTE_CONTENT_SEARCH_PATTERN";

/// 远端根不存在或不是普通文件/目录。刻意避开 125-127：SSH 执行器把这三个
/// 退出码视为“远程包装/程序启动失败”，会在协议层直接报错并保留 unknown 负债。
const ROOT_UNAVAILABLE_EXIT: i32 = 4;

/// 匹配阶段失败（grep 自身报错、xargs 批处理失败等）。
const REMOTE_FAILURE_EXIT: i32 = 2;

/// 错误信息里保留的远端 stderr 字符数（按字符截断，避免切坏中文/emoji）。
const STDERR_PREVIEW_CHARS: usize = 240;

/// 在 SSH 项目上执行内容搜索的提供者。
///
/// 只依赖既有 `SandboxExecutor`，不拥有连接生命周期：executor 由调用方的
/// 项目连接持有，提供者随 agent 一起释放。
pub struct RemoteContentSearch {
    executor: Arc<dyn SandboxExecutor>,
    /// `FileSystem`/`ContentSearchProvider` 都是同步契约，而 `execute` 是
    /// async；在构造时（应用运行时内）捕获句柄，搜索时用 `block_on` 桥接。
    /// 调用方必须像 `GrepTool` 那样在阻塞线程上调用 `search`，否则
    /// `block_on` 会在异步执行上下文里 panic。
    runtime: Option<tokio::runtime::Handle>,
}

impl RemoteContentSearch {
    pub fn new(executor: Arc<dyn SandboxExecutor>) -> Self {
        Self {
            executor,
            runtime: tokio::runtime::Handle::try_current().ok(),
        }
    }
}

impl ContentSearchProvider for RemoteContentSearch {
    fn search(&self, request: &ContentSearchRequest<'_>) -> Result<Option<ContentSearchOutcome>> {
        // 正则兼容性先判定：远端只有 POSIX ERE，Rust regex 的 `\d`/`\w`/`\p{}`/
        // 内联标志/非贪婪等写法在 ERE 里没有等价语义。这里不冒险翻译（Rust 的
        // `\w` 是 Unicode 语义，翻译成 ASCII 会静默改变结果），而是返回 `None`
        // 让工具落回本机遍历：结果正确、只是慢。
        if !ere_compatible_request(request) {
            tracing::debug!(
                pattern = request.pattern,
                case_insensitive = request.case_insensitive,
                "remote content search declined a pattern without an ERE equivalent"
            );
            return Ok(None);
        }
        // 先做纯函数校验：相对路径在这里就失败，不会触碰远端或运行时。
        let script = build_remote_search_command(request)?;
        let working_directory = remote_working_directory(request.root);
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            protocol_error(
                "remote content search requires the original application runtime; \
                 call it from a blocking tool worker",
            )
        })?;
        let command = SandboxCommand::shell(script).with_working_dir(&working_directory);
        let result = runtime.block_on(self.executor.execute(command))?;
        Ok(Some(outcome_from_execution(&result, request)?))
    }
}

/// 该请求能否用 POSIX ERE 忠实表达。
///
/// 判定是保守的：只要出现 ERE 无法等价表达的构造就放弃远端、落回本机遍历。
/// 宁可慢也不能静默给出与本机不同的命中集合。
fn ere_compatible_request(request: &ContentSearchRequest<'_>) -> bool {
    if !ere_compatible_pattern(request.pattern) {
        return false;
    }
    // `-i` 在远端按 locale 折叠，Rust 按 Unicode 折叠；非 ASCII 大小写不敏感
    // 搜索可能得到不同命中，回退本机。
    if request.case_insensitive && !request.pattern.is_ascii() {
        return false;
    }
    true
}

/// 只接受两个引擎语义一致的转义：标点类“去掉特殊含义”的转义。
///
/// `\d`/`\w`/`\s`/`\b`/`\p{}`/`\n`/`\t` 等在 ERE 里要么不支持、要么是
/// 字面字符，与 Rust regex 的 Unicode 语义不同，一律视为不兼容。
fn ere_compatible_pattern(pattern: &str) -> bool {
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => match characters.next() {
                Some(escaped) if ".*+?()[]{}|\\^$-/".contains(escaped) => {}
                _ => return false,
            },
            // 内联标志、非捕获/命名分组、前后瞻都以 `(?` 开头，ERE 不支持。
            '(' if pattern_starts_group_extension(&mut characters) => return false,
            // 惰性量词：ERE 没有非贪婪语义。
            '*' | '+' | '?' if matches!(characters.clone().next(), Some('?')) => return false,
            _ => {}
        }
    }
    true
}

fn pattern_starts_group_extension(characters: &mut std::str::Chars<'_>) -> bool {
    matches!(characters.clone().next(), Some('?'))
}

/// 构造远端 `sh -c` 脚本。纯函数，便于在没有 SSH 的环境里断言命令形态。
///
/// `request.root` 必须是绝对路径：远端命令只能使用目标机器自己的路径命名空间，
/// 本机路径（包括当前工作目录）不会被拼进去。
pub(crate) fn build_remote_search_command(request: &ContentSearchRequest<'_>) -> Result<String> {
    let root = absolute_root_text(request.root)?;
    let root = trim_trailing_separator(root);
    let quoted_root = quote(root)?;
    let quoted_pattern = quote(request.pattern)?;
    let glob_clause = filename_glob_clause(request.glob);
    let grep_flags = grep_flags(request);
    // `find -size -Nc` 是“小于 N 字节”，要复现“只有大于 10MB 才跳过”，
    // 这里取上限 + 1，从而保留恰好 10MB 的文件。
    let size_limit = MAX_FILE_BYTES.saturating_add(1);

    let mut script = String::new();
    script.push_str("root=");
    script.push_str(&quoted_root);
    script.push('\n');
    script.push_str("export ");
    script.push_str(PATTERN_ENV);
    script.push('=');
    script.push_str(&quoted_pattern);
    script.push('\n');

    // 目录根：先按既有忽略项剪枝，再用大小/glob 选出候选文件。根目录本身用
    // `-path "$root"` 放行，因为忽略项只作用于根之下的条目（与 read_dir 的
    // 遍历位置一致）。
    script.push_str("if [ -d \"$root\" ]; then\n");
    script.push_str("eko_search_candidates() {\nfind -H \"$root\" \\( -path \"$root\" -o ");
    script.push_str(&ignored_directory_clause());
    script.push_str(" -o -type f ");
    script.push_str(&ignored_file_clause());
    script.push_str(" -size -");
    script.push_str(&size_limit.to_string());
    script.push('c');
    script.push_str(&glob_clause);
    script.push_str(" -print0 \\)\n}\n");
    // 文件根：本地路径只对文件名做 glob、只跳过 >10MB，不套用隐藏/忽略项，
    // 因此这里也不套用。
    script.push_str("elif [ -f \"$root\" ]; then\n");
    script.push_str("eko_search_candidates() {\nfind -H \"$root\" -maxdepth 0 -type f -size -");
    script.push_str(&size_limit.to_string());
    script.push('c');
    script.push_str(&glob_clause);
    script.push_str(" -print0\n}\n");
    script.push_str("else\nprintf '%s\\n' 'eko remote content search root is not a file or directory' >&2\nexit ");
    script.push_str(&ROOT_UNAVAILABLE_EXIT.to_string());
    script.push_str("\nfi\n");

    // 候选文件按批交给 grep（xargs 只在参数过多时拆批，单个文件不会被拆开）。
    // 内层 `sh -c` 把 grep 的 0/1 归一为成功、其它归一为失败：这样 xargs 的
    // 123 才专指“某个批次真的报错”，而“没有匹配”不会被当成故障。
    script.push_str("eko_search_candidates | xargs -0 sh -c 'grep -Hn --null -I");
    script.push_str(&grep_flags);
    script.push_str(" -E -e \"$");
    script.push_str(PATTERN_ENV);
    script.push_str("\" -- \"$@\"; status=$?; [ \"$status\" -le 1 ] && exit 0; exit ");
    script.push_str(&REMOTE_FAILURE_EXIT.to_string());
    script.push_str("' sh\n");

    script.push_str("status=$?\n");
    script.push_str("case \"$status\" in\n0) exit 0 ;;\n123) printf '%s\\n' 'eko remote content search: grep reported an error' >&2; exit ");
    script.push_str(&REMOTE_FAILURE_EXIT.to_string());
    script.push_str(
        " ;;\n*) printf '%s\\n' 'eko remote content search: command pipeline failed' >&2; exit ",
    );
    script.push_str(&REMOTE_FAILURE_EXIT.to_string());
    script.push_str(" ;;\nesac\n");
    Ok(script)
}

/// 把一次执行结果映射成结果记录。纯函数，退出码语义在这里集中说明：
/// - 0 = 有匹配；1 = 无匹配（`grep` 语义，空结果而不是失败）；
/// - 其它 = 远端命令失败，必须上抛，不能降级成空结果。
pub(crate) fn outcome_from_execution(
    result: &ExecutionResult,
    request: &ContentSearchRequest<'_>,
) -> Result<ContentSearchOutcome> {
    if result.timed_out || result.cancelled {
        return Err(protocol_error(
            "remote content search did not complete (timeout or cancellation); partial output is not a result set",
        ));
    }
    match result.exit_code {
        0 | 1 => parse_remote_search_output(
            &result.stdout,
            request.context_lines,
            request.max_results,
            result.output_truncated,
        ),
        code => Err(protocol_error(format!(
            "remote content search failed with exit {code}: {}",
            stderr_preview(&result.stderr)
        ))),
    }
}

/// 解析远端回传的 `path\0line<sep>content` 行，并按本机遍历顺序重建记录。
///
/// 本机顺序是“目录项按路径排序后的深度优先”，等价于按 `Path` 分量排序；
/// 因此这里用 `BTreeMap<PathBuf, _>` 收集后统一重排，而不依赖 `find` 的
/// 文件系统顺序。
///
/// 传输上限截断时，最后一批文件可能只回传了半个上下文组；丢弃最后出现的
/// 那个文件的记录，而不是输出一条缺行的多行记录。
pub(crate) fn parse_remote_search_output(
    stdout: &str,
    context_lines: usize,
    max_results: usize,
    output_truncated: bool,
) -> Result<ContentSearchOutcome> {
    let mut files: BTreeMap<PathBuf, BTreeMap<usize, ParsedLine>> = BTreeMap::new();
    let mut last_file: Option<PathBuf> = None;

    for raw in stdout.lines() {
        // `--` 是 grep 的分组分隔行，不携带内容。
        if raw.is_empty() || raw == "--" {
            continue;
        }
        let Some((path, remainder)) = raw.split_once('\0') else {
            // 传输截断可能留下没有 NUL 的最后半行；丢弃而不是猜。
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let Some((line_number, matched, content)) = split_match_line(remainder) else {
            continue;
        };
        let path = PathBuf::from(path);
        files.entry(path.clone()).or_default().insert(
            line_number,
            ParsedLine {
                content: content.to_string(),
                matched,
            },
        );
        last_file = Some(path);
    }

    let mut records: Vec<String> = Vec::new();
    for (path, lines) in &files {
        if output_truncated && last_file.as_ref() == Some(path) {
            continue;
        }
        let display = path.display().to_string();
        for (line_number, line) in lines {
            if !line.matched {
                continue;
            }
            records.push(render_record(&display, *line_number, lines, context_lines));
        }
    }

    let mut truncated = output_truncated;
    if records.len() > max_results {
        // `truncate` 只保留完整记录，不做字节截断，避免切坏多行记录。
        records.truncate(max_results);
        truncated = true;
    }
    Ok(ContentSearchOutcome { records, truncated })
}

#[derive(Debug)]
struct ParsedLine {
    content: String,
    matched: bool,
}

/// 把一条命中行渲染成本机 `GrepTool` 的最终展示文本。
///
/// 无上下文：`"{path}:{line}:  {content}"`。
/// 有上下文：窗口内每行加前缀（命中 `>`、其余空格），每行仍是
/// `"{prefix}{path}:{line}:  {content}"`，多行用换行连接。窗口大小的裁剪
/// 复现 `search_file` 的 `line_idx - ctx ..= line_idx + ctx`（越界由可用行自然
/// 截断），并且同一文件的两个命中各自生成一条记录，不做合并。
fn render_record(
    path: &str,
    line_number: usize,
    lines: &BTreeMap<usize, ParsedLine>,
    context_lines: usize,
) -> String {
    if context_lines == 0 {
        let content = lines
            .get(&line_number)
            .map(|line| line.content.as_str())
            .unwrap_or("");
        return format!("{path}:{line_number}:  {content}");
    }

    let start = line_number.saturating_sub(context_lines);
    let end = line_number.saturating_add(context_lines);
    let mut record_lines: Vec<String> = Vec::new();
    for (context_number, context) in lines.range(start..=end) {
        let prefix = if *context_number == line_number {
            ">"
        } else {
            " "
        };
        record_lines.push(format!(
            "{prefix}{path}:{context_number}:  {}",
            context.content
        ));
    }
    record_lines.join("\n")
}

/// 拆出 `line<sep>content`。路径已由 NUL 隔开，所以这里不会与路径里的
/// 冒号/连字符混淆；`content` 内部的同形文本也不会被误读。
fn split_match_line(remainder: &str) -> Option<(usize, bool, &str)> {
    let separator_index = remainder.find(|character: char| !character.is_ascii_digit())?;
    if separator_index == 0 {
        return None;
    }
    let line_number = remainder.get(..separator_index)?.parse::<usize>().ok()?;
    let separator = remainder.get(separator_index..)?.chars().next()?;
    let matched = match separator {
        ':' => true,
        '-' => false,
        _ => return None,
    };
    let content = remainder.get(separator_index.saturating_add(separator.len_utf8())..)?;
    Some((line_number, matched, content))
}

/// 远端脚本的工作目录：`SandboxExecutor` 会用 `cd` 进入它，因此必须是
/// 根所在的父目录（文件根的父目录才是目录）。根为 `/` 时退回 `/`。
fn remote_working_directory(root: &Path) -> PathBuf {
    match root.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => root.to_path_buf(),
    }
}

fn absolute_root_text(root: &Path) -> Result<&str> {
    if !root.is_absolute() {
        return Err(ReactError::Tool(Box::new(ToolError::InvalidParameter {
            name: "path".to_string(),
            message: format!(
                "Remote content search root must be an absolute path: {}",
                root.display()
            ),
        })));
    }
    super::remote_path(root).map_err(protocol_error)
}

/// 去掉结尾分隔符，使远端 `find` 打印的路径与本机 `PathBuf` 拼接结果一致。
fn trim_trailing_separator(root: &str) -> &str {
    let trimmed = root.trim_end_matches('/');
    if trimmed.is_empty() { "/" } else { trimmed }
}

fn quote(value: &str) -> Result<String> {
    super::shell_quote(value).map_err(protocol_error)
}

/// 目录剪枝子句：隐藏目录 + 既有忽略目录。`-prune` 不进入匹配目录。
fn ignored_directory_clause() -> String {
    let mut names: Vec<&str> = vec![".*"];
    names.extend(IGNORED_ENTRY_NAMES.iter().copied());
    let mut clause = String::from("-type d \\( ");
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            clause.push_str(" -o ");
        }
        clause.push_str("-name ");
        clause.push_str(&quote_constant(name));
    }
    clause.push_str(" \\) -prune");
    clause
}

/// 普通文件过滤子句：隐藏文件 + 同名忽略项（本机遍历对文件和目录一视同仁）。
fn ignored_file_clause() -> String {
    let mut clause = String::from("! -name '.*'");
    for name in IGNORED_ENTRY_NAMES {
        clause.push_str(" ! -name ");
        clause.push_str(&quote_constant(name));
    }
    clause
}

/// 文件名 glob 过滤。`find -name` 与 `GrepTool::glob_matches` 一样只匹配
/// basename；`*.{ts,tsx}` 这类花括号形式按本机的展开规则拆成多个 `-name`。
fn filename_glob_clause(glob: Option<&str>) -> String {
    let Some(glob) = glob else {
        return String::new();
    };
    let patterns = expand_braces(glob);
    let mut clause = String::from(" \\( ");
    for (index, pattern) in patterns.iter().enumerate() {
        if index > 0 {
            clause.push_str(" -o ");
        }
        clause.push_str("-name ");
        clause.push_str(&quote_constant(pattern));
    }
    clause.push_str(" \\)");
    clause
}

/// 复现 `grep.rs::glob_matches` 的花括号展开：只处理第一对 `{}`，每个候选
/// 去除首尾空白后与前后缀拼接；没有花括号时原样保留。
fn expand_braces(pattern: &str) -> Vec<String> {
    if let Some((prefix, remainder)) = pattern.split_once('{')
        && let Some((alternatives, suffix)) = remainder.split_once('}')
    {
        return alternatives
            .split(',')
            .map(|alternative| format!("{}{}{}", prefix, alternative.trim(), suffix))
            .collect();
    }
    vec![pattern.to_string()]
}

fn grep_flags(request: &ContentSearchRequest<'_>) -> String {
    let mut flags = String::new();
    if request.case_insensitive {
        flags.push_str(" -i");
    }
    if request.context_lines > 0 {
        flags.push_str(" -C ");
        flags.push_str(&request.context_lines.to_string());
    }
    flags
}

/// 编译期常量（忽略项名字）没有引号/NUL，直接套用与 `shell_quote` 相同的
/// 单引号规则即可，不需要再走可能失败的校验路径。
fn quote_constant(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn stderr_preview(stderr: &str) -> String {
    stderr.trim().chars().take(STDERR_PREVIEW_CHARS).collect()
}

fn protocol_error(message: impl Into<String>) -> ReactError {
    ReactError::Tool(Box::new(ToolError::ExecutionFailed {
        tool: "grep".to_string(),
        message: message.into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tools::Tool;
    use echo_agent::tools::ToolContext;
    use echo_agent::tools::ToolParameters;
    use echo_agent::tools::files::grep::GrepTool;
    use serde_json::{Value, json};
    use std::fs;
    use std::process::Command;

    fn request<'a>(
        root: &'a Path,
        pattern: &'a str,
        glob: Option<&'a str>,
        context_lines: usize,
        case_insensitive: bool,
    ) -> ContentSearchRequest<'a> {
        ContentSearchRequest {
            root,
            pattern,
            glob,
            context_lines,
            case_insensitive,
            max_results: 1_000,
        }
    }

    #[test]
    fn ere_incompatible_patterns_decline_remote_search() -> Result<()> {
        let root = Path::new("/srv/project");
        // Rust regex 有、POSIX ERE 没有等价语义的写法：一律拒绝远端，落回本机。
        for pattern in [
            r"\d+",
            r"\w+\.rs",
            r"\bword\b",
            r"\p{Greek}",
            r"a\tb",
            r"(?i)foo",
            r"(?:x)",
            r"foo.*?bar",
            r"a+?b",
        ] {
            let request = request(root, pattern, None, 0, false);
            assert!(
                !ere_compatible_request(&request),
                "pattern {pattern} must not be sent to a POSIX ERE engine"
            );
        }
        // 两个引擎语义一致的写法仍然走远端。
        for pattern in [r"\.rs$", "[0-9]+", "foo|bar", r"a\.b", "^start"] {
            let request = request(root, pattern, None, 0, false);
            assert!(
                ere_compatible_request(&request),
                "pattern {pattern} is expressible in ERE"
            );
        }
        // 非 ASCII + 大小写不敏感：远端按 locale 折叠，可能与 Unicode 折叠不同。
        assert!(!ere_compatible_request(&request(
            root, "Ärger", None, 0, true
        )));
        assert!(ere_compatible_request(&request(root, "abc", None, 0, true)));
        Ok(())
    }

    fn script_for(
        root: &Path,
        pattern: &str,
        glob: Option<&str>,
        context_lines: usize,
        case_insensitive: bool,
    ) -> Result<String> {
        build_remote_search_command(&request(
            root,
            pattern,
            glob,
            context_lines,
            case_insensitive,
        ))
    }

    fn execution_result(exit_code: i32, stdout: &str, stderr: &str) -> ExecutionResult {
        ExecutionResult {
            exit_code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            duration: std::time::Duration::from_millis(1),
            sandbox_type: "test".to_string(),
            timed_out: false,
            cancelled: false,
            output_truncated: false,
            stdout_bytes: u64::try_from(stdout.len()).unwrap_or(0),
            stderr_bytes: u64::try_from(stderr.len()).unwrap_or(0),
        }
    }

    // ── 命令构造（纯函数） ──────────────────────────────────────────────

    #[test]
    fn command_reproduces_local_grep_filters() -> Result<()> {
        let script = script_for(
            Path::new("/srv/project"),
            "fn\\s+main",
            Some("*.{rs,tsx}"),
            2,
            true,
        )?;
        // 根只用远端绝对路径，不拼本机路径或当前目录。
        assert!(script.contains("root='/srv/project'"));
        assert!(script.contains("export EKO_REMOTE_CONTENT_SEARCH_PATTERN='fn\\s+main'"));
        // 忽略项与 10MB 上限来自本地遍历的同一套规则；边界保留恰好 10MB。
        assert!(script.contains("-name 'node_modules'"));
        assert!(script.contains("-name '__pycache__'"));
        assert!(script.contains("-name 'vendor'"));
        assert!(script.contains("-name 'build'"));
        assert!(script.contains("-name 'dist'"));
        assert!(script.contains("! -name '.*'"));
        assert!(script.contains("-size -10485761c"));
        // glob 是文件名匹配，花括号展开成多个 -name。
        assert!(script.contains("-name '*.rs'"));
        assert!(script.contains("-name '*.tsx'"));
        // 大小写与上下文。
        assert!(script.contains(" -i"));
        assert!(script.contains("-C 2"));
        // 只回传结果行：NUL 分隔路径、只按批调用 grep。
        assert!(script.contains("--null"));
        assert!(script.contains("xargs -0"));
        Ok(())
    }

    #[test]
    fn command_without_glob_or_case_keeps_defaults() -> Result<()> {
        let script = script_for(Path::new("/srv/project"), "secret", None, 0, false)?;
        assert!(!script.contains(" -i"));
        assert!(!script.contains("-C "));
        assert!(!script.contains("-name '*.rs'"));
        Ok(())
    }

    #[test]
    fn command_quotes_patterns_and_root() -> Result<()> {
        let script = script_for(Path::new("/srv/it's project"), "it's", None, 0, false)?;
        assert!(script.contains(r#"root='/srv/it'\''s project'"#));
        assert!(script.contains(r#"export EKO_REMOTE_CONTENT_SEARCH_PATTERN='it'\''s'"#));
        Ok(())
    }

    #[test]
    fn relative_root_is_rejected_before_any_remote_work() {
        let request = request(Path::new("relative/dir"), "x", None, 0, false);
        assert!(build_remote_search_command(&request).is_err());

        let executor: Arc<dyn SandboxExecutor> =
            Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox());
        let provider = RemoteContentSearch::new(executor);
        // 没有运行时应返回错误而不是 panic；相对路径在更早的纯函数里就被拒绝。
        let outcome = provider.search(&request);
        assert!(outcome.is_err());
    }

    #[test]
    fn file_root_is_detected_without_applying_ignore_names() -> Result<()> {
        let script = script_for(
            Path::new("/srv/project/target/notes.txt"),
            "x",
            None,
            0,
            false,
        )?;
        // 文件根分支不套用隐藏/忽略项（本地 search_file 也不套用），只保留大小与 glob。
        assert!(script.contains("elif [ -f \"$root\" ]; then"));
        assert!(script.contains("-maxdepth 0 -type f -size -10485761c"));
        Ok(())
    }

    // ── 输出解析（纯函数） ──────────────────────────────────────────────

    #[test]
    fn parse_returns_empty_for_no_matches() -> Result<()> {
        let outcome = parse_remote_search_output("", 0, 1_000, false)?;
        assert!(outcome.records.is_empty());
        assert!(!outcome.truncated);
        Ok(())
    }

    #[test]
    fn parse_renders_final_display_text() -> Result<()> {
        let stdout = "/w/a.rs\x003:secret\n/w/a.rs\x007:other\n";
        let outcome = parse_remote_search_output(stdout, 0, 1_000, false)?;
        assert_eq!(
            outcome.records,
            vec![
                "/w/a.rs:3:  secret".to_string(),
                "/w/a.rs:7:  other".to_string(),
            ]
        );
        assert!(!outcome.truncated);
        Ok(())
    }

    #[test]
    fn parse_keeps_colons_in_paths_and_content() -> Result<()> {
        let stdout = "/w/colon:name.txt\x003:see issue:42: detail\n";
        let outcome = parse_remote_search_output(stdout, 0, 1_000, false)?;
        assert_eq!(
            outcome.records,
            vec!["/w/colon:name.txt:3:  see issue:42: detail".to_string()]
        );
        Ok(())
    }

    #[test]
    fn parse_rebuilds_context_records_per_match() -> Result<()> {
        let stdout =
            "/w/a.rs\x001-a\n/w/a.rs\x002:x\n/w/a.rs\x003-b\n/w/a.rs\x004:x\n/w/a.rs\x005-c\n";
        let outcome = parse_remote_search_output(stdout, 1, 1_000, false)?;
        // 两个命中各自一条窗口记录，既不复用也不合并（与 search_file 一致）。
        assert_eq!(
            outcome.records,
            vec![
                " /w/a.rs:1:  a\n>/w/a.rs:2:  x\n /w/a.rs:3:  b".to_string(),
                " /w/a.rs:3:  b\n>/w/a.rs:4:  x\n /w/a.rs:5:  c".to_string(),
            ]
        );
        Ok(())
    }

    #[test]
    fn parse_orders_records_by_path_then_line() -> Result<()> {
        let stdout = "/w/b.txt\x001:bee\n/w/a/deep.txt\x002:aye\n/w/a.txt\x003:aye\n";
        let outcome = parse_remote_search_output(stdout, 0, 1_000, false)?;
        // 与本地“目录项排序后的深度优先”一致：目录 `a` 在 `a.txt` 之前。
        assert_eq!(
            outcome.records,
            vec![
                "/w/a/deep.txt:2:  aye".to_string(),
                "/w/a.txt:3:  aye".to_string(),
                "/w/b.txt:1:  bee".to_string(),
            ]
        );
        Ok(())
    }

    #[test]
    fn parse_strips_crlf_like_rust_lines() -> Result<()> {
        let stdout = "/w/a.txt\x002:secret crlf\r\n";
        let outcome = parse_remote_search_output(stdout, 0, 1_000, false)?;
        assert_eq!(
            outcome.records,
            vec!["/w/a.txt:2:  secret crlf".to_string()]
        );
        Ok(())
    }

    #[test]
    fn parse_truncates_to_max_results() -> Result<()> {
        let stdout = "/w/a.txt\x001:x\n/w/a.txt\x002:y\n";
        let outcome = parse_remote_search_output(stdout, 0, 1, false)?;
        assert_eq!(outcome.records, vec!["/w/a.txt:1:  x".to_string()]);
        assert!(outcome.truncated);
        Ok(())
    }

    #[test]
    fn parse_drops_the_partial_file_when_transport_truncated() -> Result<()> {
        let stdout = "/w/a.txt\x001:first\n/w/b.txt\x001:second\n";
        let outcome = parse_remote_search_output(stdout, 0, 1_000, true)?;
        // 最后一个文件可能只回传了半个上下文组；宁可少一条，也不能给出缺行记录。
        assert_eq!(outcome.records, vec!["/w/a.txt:1:  first".to_string()]);
        assert!(outcome.truncated);
        Ok(())
    }

    // ── 退出码映射 ──────────────────────────────────────────────────────

    #[test]
    fn exit_codes_map_to_success_empty_or_failure() -> Result<()> {
        let request = request(Path::new("/w"), "x", None, 0, false);
        let matched = outcome_from_execution(&execution_result(0, "/w/a\x001:x\n", ""), &request)?;
        assert_eq!(matched.records, vec!["/w/a:1:  x".to_string()]);

        // 退出码 1 是 grep 的“没有匹配”，不是失败。
        let empty = outcome_from_execution(&execution_result(1, "", ""), &request)?;
        assert!(empty.records.is_empty());
        assert!(!empty.truncated);

        // 其它退出码（含远端根不存在）必须上抛，不能降级成空结果。
        assert!(outcome_from_execution(&execution_result(2, "", "boom"), &request).is_err());
        assert!(outcome_from_execution(&execution_result(4, "", "missing"), &request).is_err());

        let mut timed_out = execution_result(0, "", "");
        timed_out.timed_out = true;
        assert!(outcome_from_execution(&timed_out, &request).is_err());
        Ok(())
    }

    // ── 与本地遍历的等价性 ──────────────────────────────────────────────

    fn write_fixture(root: &Path, relative: &str, content: &[u8]) -> std::io::Result<()> {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)
    }

    fn build_fixture() -> Result<tempfile::TempDir> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        write_fixture(
            root,
            "a.txt",
            b"alpha\nsecret one\nbeta\nsecret two\ngamma\n",
        )?;
        write_fixture(root, "app.rs", b"fn main() {\n    // secret\n}\n")?;
        write_fixture(root, "app.ts", b"export const secret = 1;\n")?;
        write_fixture(root, "app.tsx", b"export const secret = 2;\n")?;
        write_fixture(root, "app.py", b"secret = 3\n")?;
        write_fixture(root, "sub.txt", b"secret at sub.txt\n")?;
        write_fixture(
            root,
            "sub/deep.txt",
            b"nested secret\n\xe4\xb8\xad\xe6\x96\x87 secret \xe5\x86\x85\xe5\xae\xb9\n",
        )?;
        write_fixture(
            root,
            "sub/nested/deeper.md",
            b"\xe7\xa7\x98\xe5\xaf\x86 in markdown\nsecret line\n",
        )?;
        write_fixture(root, "colon:name.txt", b"secret colon name\n")?;
        write_fixture(root, "crlf.txt", b"alpha\r\nsecret crlf\r\ngamma\r\n")?;
        write_fixture(root, "uppercase.txt", b"SECRET upper\n")?;
        // 非 UTF-8 且含 NUL：本地 read_to_string 失败、grep -I 也判为二进制，
        // 两边都必须跳过它。
        write_fixture(root, "bin.dat", b"\x00\xff\xfe secret binary\n")?;
        // 隐藏项与既有忽略项。
        write_fixture(root, ".dotfile", b"secret dot\n")?;
        write_fixture(root, ".hidden/skip.txt", b"secret hidden\n")?;
        write_fixture(root, "node_modules/pkg/index.js", b"secret module\n")?;
        write_fixture(root, "target/out.txt", b"secret target\n")?;
        write_fixture(root, "build/out.txt", b"secret build\n")?;
        write_fixture(root, "vendor/lib.txt", b"secret vendor\n")?;
        write_fixture(root, "__pycache__/x.txt", b"secret cache\n")?;
        write_fixture(root, "dist/x.txt", b"secret dist\n")?;
        // 超过 10MB：本地按 metadata.len() 跳过，远端按 find -size 跳过。
        let big = root.join("big").join("huge.txt");
        if let Some(parent) = big.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&big, b"secret big\n")?;
        let file = fs::OpenOptions::new().write(true).open(&big)?;
        file.set_len(MAX_FILE_BYTES.saturating_add(1024))?;
        Ok(temp)
    }

    /// 用真实 `GrepTool` 取基线。逐条分页（limit=1）是为了拿到未合并的
    /// 多行记录边界，而不是把整页文本再拆一遍。
    async fn local_grep_records(
        root: &Path,
        pattern: &str,
        glob: Option<&str>,
        context_lines: usize,
        case_insensitive: bool,
    ) -> Result<Vec<String>> {
        let tool = GrepTool::with_base_dir(root.to_path_buf());
        let ctx = ToolContext::default();
        let mut records: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut parameters: ToolParameters = ToolParameters::new();
            parameters.insert("pattern".to_string(), Value::String(pattern.to_string()));
            parameters.insert(
                "path".to_string(),
                Value::String(root.display().to_string()),
            );
            parameters.insert("context".to_string(), json!(context_lines));
            parameters.insert(
                "case_insensitive".to_string(),
                Value::Bool(case_insensitive),
            );
            parameters.insert("limit".to_string(), json!(1));
            if let Some(glob) = glob {
                parameters.insert("glob".to_string(), Value::String(glob.to_string()));
            }
            if let Some(cursor) = cursor.as_ref() {
                parameters.insert("cursor".to_string(), Value::String(cursor.clone()));
            }
            let result = tool.execute_with_context(parameters, &ctx).await?;
            let total = result
                .metadata
                .get("page.total")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if total > 0
                && let Some((record, _)) = result.output.rsplit_once("\n\n")
            {
                records.push(record.to_string());
            }
            cursor = result.metadata.get("page.next_cursor").cloned();
            if cursor.is_none() {
                break;
            }
        }
        Ok(records)
    }

    /// 本机用 `sh` 跑同一条远端命令，再用同一解析函数得到记录。
    fn remote_grep_records(
        root: &Path,
        pattern: &str,
        glob: Option<&str>,
        context_lines: usize,
        case_insensitive: bool,
    ) -> Result<Vec<String>> {
        let script = script_for(root, pattern, glob, context_lines, case_insensitive)?;
        let output = Command::new("sh").arg("-c").arg(&script).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            matches!(output.status.code(), Some(0)),
            "remote command exited with {:?}: {}",
            output.status.code(),
            stderr
        );
        Ok(parse_remote_search_output(&stdout, context_lines, 1_000, false)?.records)
    }

    #[tokio::test]
    async fn remote_command_matches_local_grep_on_fixture() -> Result<()> {
        let temp = build_fixture()?;
        let root = temp.path();
        let cases: [(&str, Option<&str>, usize, bool); 7] = [
            ("secret", None, 0, false),
            ("SECRET", None, 0, true),
            ("secret", Some("*.rs"), 0, false),
            ("secret", Some("*.{ts,tsx}"), 0, false),
            ("secret", None, 2, false),
            ("^fn ", Some("*.rs"), 1, false),
            ("秘密", None, 1, false),
        ];
        let mut compared = 0_usize;
        for (pattern, glob, context_lines, case_insensitive) in cases {
            let expected =
                local_grep_records(root, pattern, glob, context_lines, case_insensitive).await?;
            let actual = remote_grep_records(root, pattern, glob, context_lines, case_insensitive)?;
            assert_eq!(
                actual, expected,
                "pattern {pattern:?} glob {glob:?} context {context_lines} case_insensitive {case_insensitive}"
            );
            compared = compared.saturating_add(actual.len());
        }

        // 文件根也必须与本地一致（不套用隐藏/忽略项，只保留大小与 glob）。
        let file_root = root.join("sub").join("deep.txt");
        let expected = local_grep_records(&file_root, "secret", None, 1, false).await?;
        let actual = remote_grep_records(&file_root, "secret", None, 1, false)?;
        assert_eq!(actual, expected, "file root");
        compared = compared.saturating_add(actual.len());

        // >10MB 的文件根在两条路径上都必须被跳过。
        let big_root = root.join("big").join("huge.txt");
        assert!(
            local_grep_records(&big_root, "secret", None, 0, false)
                .await?
                .is_empty()
        );
        assert!(
            remote_grep_records(&big_root, "secret", None, 0, false)?.is_empty(),
            "oversized file root must be skipped remotely"
        );

        assert!(
            compared > 0,
            "fixture produced no records; the comparison would be vacuous"
        );
        Ok(())
    }

    #[test]
    fn missing_root_is_a_failure_not_an_empty_result() -> Result<()> {
        let root = Path::new("/nonexistent-eko-remote-search-root");
        let request = request(root, "secret", None, 0, false);
        let script = build_remote_search_command(&request)?;
        let output = Command::new("sh").arg("-c").arg(&script).output()?;
        assert_eq!(output.status.code(), Some(ROOT_UNAVAILABLE_EXIT));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let result = execution_result(
            output.status.code().unwrap_or(REMOTE_FAILURE_EXIT),
            &stdout,
            &stderr,
        );
        assert!(outcome_from_execution(&result, &request).is_err());
        Ok(())
    }
}
