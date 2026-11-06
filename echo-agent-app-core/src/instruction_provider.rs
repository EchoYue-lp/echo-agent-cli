//! Layered instruction-file loader.
//!
//! Loads five tiers of instruction Markdown files and
//! concatenates them as a system-prompt suffix:
//! - `~/.eko/user.md`              — user-level (cross-project)
//! - `<root..cwd>/AGENTS[.override].md` — repository-standard chain
//! - `<project-root>/.eko/project.md` — project-level
//! - `<project-root>/.eko/learned-rules.md` — auto-promoted rules (evolution)
//! - `<cwd>/.eko/local.md`         — local directory
//!
//! Also loads hot-layer memory from `.eko/MEMORY.md`.
//!
//! Static, file-only loader: no DB, no embeddings, no recall. Query-dependent
//! dynamic memories are handled separately by the layered memory store.
//!
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use echo_agent::tools::filesystem::FileType;

/// File name for auto-promoted rules (evolution system output).
const LEARNED_RULES_FILE: &str = "learned-rules.md";

// ── 规则正文指纹缓存 ────────────────────────────────────────────────
//
// 背景：SSH 远程项目下每条消息的准备阶段都要重读 AGENTS/AGENTS.override 链与
// `.eko/*.md`，每次 `read` 都是一次网络往返。ADR 0063 明确拒绝"永久缓存规则
// 正文"——无法保证每条消息使用当前规则。这里采用"缓存正文 + 每次 stat 校验"：
// 只有本次 `symlink_metadata` 与缓存签名一致时才复用正文；指纹变化或无法证明
// 稳定就重读。
//
// 缓存是"内容来源"而不是"读取决策"：读哪些文件、按什么优先级、符号链接和
// 非普通文件如何拒绝，仍完全由加载器决定；缓存只替换"已经确定要读这个路径"
// 之后的那次 `read`。
//
// 只在 `cache_namespace() == Some(_)` 的提供者上生效。本地提供者返回 None：
// 本地读取便宜，缓存没有收益，也避免本地目录被远端缓存污染。

/// 缓存条目只存活在本进程内存中；容量上限只约束内存，不落盘、不引新依赖。
const INSTRUCTION_CACHE_CAPACITY: usize = 256;

/// SFTP v3 的 mtime 只有秒级精度：mtime 落在当前时间该窗口内的文件视为不稳定，
/// 即使指纹相同也强制重读。否则"同一秒内二次编辑"会一直命中旧正文。
const UNSTABLE_MODIFIED_WINDOW: Duration = Duration::from_secs(2);

/// 命中所需的 stat 指纹与正文。`file_type` 一定来自已通过拒绝规则的普通文件。
#[derive(Clone, Debug, Eq, PartialEq)]
struct CachedInstructionBody {
    modified: SystemTime,
    len: u64,
    file_type: FileType,
    content: String,
}

/// 键是 `(namespace, 绝对路径)`：namespace 隔离不同物理目标（不同主机/连接/
/// 项目根）上的同名路径，路径是本次准备阶段实际读取的那个 token。
static INSTRUCTION_BODY_CACHE: LazyLock<
    Mutex<HashMap<String, HashMap<PathBuf, CachedInstructionBody>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// 查表本身不做 I/O，也不改变缓存；比较全部在锁内完成，命中的正文克隆后再放锁。
fn instruction_cache_lookup(
    namespace: &str,
    path: &Path,
    modified: SystemTime,
    len: u64,
    file_type: FileType,
) -> Option<String> {
    let cache = INSTRUCTION_BODY_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cached = cache.get(namespace)?.get(path)?;
    (cached.modified == modified && cached.len == len && cached.file_type == file_type)
        .then(|| cached.content.clone())
}

fn instruction_cache_store(namespace: &str, path: &Path, body: CachedInstructionBody) {
    let mut cache = INSTRUCTION_BODY_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entries = cache.values().map(HashMap::len).sum::<usize>();
    if entries >= INSTRUCTION_CACHE_CAPACITY {
        evict_one_instruction_entry(&mut cache);
    }
    cache
        .entry(namespace.to_string())
        .or_default()
        .insert(path.to_path_buf(), body);
}

/// 简单淘汰：任意一条即可。被淘汰只会让下次读取多一次 `read`，不影响正确性，
/// 因此不维护真正的 LRU 顺序。
fn evict_one_instruction_entry(
    cache: &mut HashMap<String, HashMap<PathBuf, CachedInstructionBody>>,
) {
    let Some(victim_namespace) = cache
        .iter()
        .find(|(_, paths)| !paths.is_empty())
        .map(|(namespace, _)| namespace.clone())
    else {
        return;
    };
    let mut remove_namespace = false;
    if let Some(paths) = cache.get_mut(&victim_namespace) {
        if let Some(victim_path) = paths.keys().next().cloned() {
            paths.remove(&victim_path);
        }
        remove_namespace = paths.is_empty();
    }
    if remove_namespace {
        cache.remove(&victim_namespace);
    }
}

/// 失效必须同时清掉空的命名空间层，避免重连后留下无法命中的空壳。
fn instruction_cache_remove(namespace: &str, path: &Path) {
    let mut cache = INSTRUCTION_BODY_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let empty = match cache.get_mut(namespace) {
        Some(paths) => {
            paths.remove(path);
            paths.is_empty()
        }
        None => false,
    };
    if empty {
        cache.remove(namespace);
    }
}

/// mtime 能否充当稳定指纹。未来时间（时钟偏差或被改写）同样不可信。
fn instruction_fingerprint_is_stable(modified: SystemTime) -> bool {
    matches!(
        SystemTime::now().duration_since(modified),
        Ok(age) if age >= UNSTABLE_MODIFIED_WINDOW
    )
}

/// Layered instruction-file loader (user / repository / project / learned / local).
pub struct InstructionProvider {
    pub project_level: Option<String>,
    pub user_level: Option<String>,
    /// Standard `AGENTS.override.md` / `AGENTS.md` chain, root to working dir.
    pub repository_level: Option<String>,
    pub local_level: Option<String>,
    /// Auto-promoted rules and learned constraints (learned-rules.md body, frontmatter stripped).
    pub agents_level: Option<String>,
    /// Hot-layer memory content (MEMORY.md body, frontmatter stripped).
    pub hot_memory: Option<String>,
}

impl InstructionProvider {
    /// Read project tiers through one selected provider while user memory and
    /// EKO learned rules stay under their original local product-data owner.
    /// Run this synchronous phase on the existing owned blocking boundary.
    pub(crate) fn load_for_file_system(
        working_dir: Option<&Path>,
        state_dir: Option<&Path>,
        files: &dyn echo_agent::tools::filesystem::FileSystem,
    ) -> echo_agent::error::Result<Self> {
        let project_root = working_dir
            .map(|path| {
                crate::utils::find_project_root_with_file_system(path, files)
                    .map(|root| root.unwrap_or_else(|| path.to_path_buf()))
            })
            .transpose()?;
        Self::load_for_resolved_project(working_dir, project_root.as_deref(), state_dir, files)
    }

    /// 根目录已经由当前准备阶段解析；保留 cwd，不再重复远程发现。
    pub(crate) fn load_for_resolved_project(
        working_dir: Option<&Path>,
        project_root: Option<&Path>,
        state_dir: Option<&Path>,
        files: &dyn echo_agent::tools::filesystem::FileSystem,
    ) -> echo_agent::error::Result<Self> {
        let project_level = project_optional_text(
            files,
            project_root
                .map(|root| root.join(".eko/project.md"))
                .as_deref(),
        )?;
        let local_level = project_optional_text(
            files,
            working_dir
                .map(|root| root.join(".eko/local.md"))
                .as_deref(),
        )?;
        let repository_level =
            repository_instructions_with_reader(working_dir, project_root, |path| {
                project_optional_text(files, Some(path))
            })?;
        let user_level = strict_optional_text(Some(&crate::data_root::user_data_path("user.md")))?;
        let agents_level = match state_dir {
            Some(root) => strict_optional_text(Some(&root.join(LEARNED_RULES_FILE)))?,
            None => None,
        }
        .map(|raw| crate::utils::strip_yaml_frontmatter(&raw));
        let global_memory = crate::data_root::user_data_path("MEMORY.md");
        let local_memory = match state_dir {
            Some(root) => strict_optional_text(Some(&root.join("MEMORY.md")))?,
            None => None,
        };
        let hot_memory = match local_memory {
            Some(raw) => Some(raw),
            None => strict_optional_text(Some(&global_memory))?,
        }
        .map(|raw| crate::utils::strip_yaml_frontmatter(&raw));
        Ok(Self {
            project_level,
            user_level,
            repository_level,
            local_level,
            agents_level,
            hot_memory,
        })
    }

    /// Load every tier from disk.
    pub fn load() -> Self {
        let working_dir = std::env::current_dir().ok();
        Self::load_for(working_dir.as_deref())
    }

    /// Load every tier for one explicit working directory.
    ///
    /// `None` means global context only: user instructions plus user-level
    /// `MEMORY.md`. It intentionally does not consult process cwd, so exiting a
    /// workspace can remove project-local instructions deterministically.
    ///
    pub fn load_for(working_dir: Option<&Path>) -> Self {
        let project_root = working_dir.map(|path| {
            crate::utils::find_project_root(path).unwrap_or_else(|| path.to_path_buf())
        });
        let project_level = Self::load_project_instructions(project_root.as_deref());
        let user_level = Self::load_user_instructions();
        let repository_level =
            Self::load_repository_instructions(working_dir, project_root.as_deref());
        let local_level = Self::load_local_instructions(working_dir);
        let agents_level = Self::load_agents_instructions(project_root.as_deref());
        let hot_memory = Self::load_hot_memory(project_root.as_deref());

        Self {
            project_level,
            user_level,
            repository_level,
            local_level,
            agents_level,
            hot_memory,
        }
    }

    /// Load the complete instruction projection without silently dropping an
    /// existing but unreadable source.
    ///
    /// Promotion and workspace bootstrap transactions use this path before
    /// publishing a runtime generation. Missing optional files are valid;
    /// symlinks, non-regular entries, invalid UTF-8, and read failures are not.
    pub(crate) fn load_for_strict(working_dir: Option<&Path>) -> std::io::Result<Self> {
        Self::load_for_strict_with_state(working_dir, None)
    }

    /// Strictly load project/repository instructions from one path while
    /// optionally taking EKO-owned learned rules and hot memory from another
    /// state directory (used by linked workspaces).
    pub(crate) fn load_for_strict_with_state(
        working_dir: Option<&Path>,
        state_dir: Option<&Path>,
    ) -> std::io::Result<Self> {
        let project_root = working_dir.map(|path| {
            crate::utils::find_project_root(path).unwrap_or_else(|| path.to_path_buf())
        });
        let project_level = strict_optional_text(
            project_root
                .as_deref()
                .map(|root| root.join(".eko").join("project.md"))
                .as_deref(),
        )?;
        let user_path = crate::data_root::user_data_path("user.md");
        let user_level = strict_optional_text(Some(&user_path))?;
        let repository_level =
            strict_repository_instructions(working_dir, project_root.as_deref())?;
        let local_level = strict_optional_text(
            working_dir
                .map(|root| root.join(".eko").join("local.md"))
                .as_deref(),
        )?;
        let agents_level = match state_dir {
            Some(root) => strict_optional_text(Some(&root.join(LEARNED_RULES_FILE)))?,
            None => strict_learned_rules(project_root.as_deref())?,
        }
        .map(|raw| crate::utils::strip_yaml_frontmatter(&raw));
        let global_memory = crate::data_root::user_data_path("MEMORY.md");
        let hot_memory = if let Some(state_dir) = state_dir {
            match strict_optional_text(Some(&state_dir.join("MEMORY.md")))? {
                Some(raw) => Some(raw),
                None => strict_optional_text(Some(&global_memory))?,
            }
        } else {
            match project_root.as_deref() {
                Some(root) => strict_optional_text(Some(&root.join(".eko").join("MEMORY.md")))?,
                None => strict_optional_text(Some(&global_memory))?,
            }
        }
        .map(|raw| crate::utils::strip_yaml_frontmatter(&raw));

        Ok(Self {
            project_level,
            user_level,
            repository_level,
            local_level,
            agents_level,
            hot_memory,
        })
    }

    /// Concatenate the instruction tiers and hot-layer memory into a single
    /// system-prompt suffix.
    ///
    /// Compatibility aggregation for callers that need one string. Runtime
    /// context injection uses [`get_instruction_suffix`] and
    /// [`get_memory_suffix`] as two independently replaceable projections.
    pub fn get_system_prompt_suffix(&self) -> String {
        let mut parts = Vec::new();
        if let Some(suffix) = self.get_instruction_suffix() {
            parts.push(suffix);
        }
        if let Some(suffix) = self.get_memory_suffix() {
            parts.push(suffix);
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", parts.join("\n\n"))
        }
    }

    /// Concatenate only the instruction tiers
    /// (user / repository / project / learned-rules / local).
    ///
    /// Excludes hot-layer memory. Returns `None` when no instruction tier has
    /// content.
    pub fn get_instruction_suffix(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(ref user) = self.user_level {
            parts.push(format!("## User-level instructions\n{}", user));
        }
        if let Some(ref repository) = self.repository_level {
            parts.push(format!("## Repository instructions\n{}", repository));
        }
        if let Some(ref project) = self.project_level {
            parts.push(format!("## Project-level instructions\n{}", project));
        }
        if let Some(ref agents) = self.agents_level {
            parts.push(format!("## Auto-promoted rules\n{}", agents));
        }
        if let Some(ref local) = self.local_level {
            parts.push(format!("## Local directory instructions\n{}", local));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// Format only the hot-layer memory (MEMORY.md body).
    ///
    /// Returns `None` when no memory content is loaded. Separated from
    /// [`get_instruction_suffix`] because runtime context owns instruction and
    /// hot-memory projections independently.
    pub fn get_memory_suffix(&self) -> Option<String> {
        self.hot_memory
            .as_ref()
            .map(|hot| format!("## Active Memories (Hot Layer)\n{}", hot))
    }

    /// Load project-level instructions from `<project-root>/.eko/project.md`.
    fn load_project_instructions(project_root: Option<&Path>) -> Option<String> {
        project_root
            .map(|root| root.join(".eko").join("project.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Load user-level instructions from `~/.eko/user.md`.
    fn load_user_instructions() -> Option<String> {
        Some(crate::data_root::user_data_path("user.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Load the standard root-to-working-directory AGENTS chain.
    ///
    /// EKO deliberately requests the framework resolver's AGENTS-only mode:
    /// `.echo-agent/*` and `CLAUDE.md` are not part of EKO's file protocol.
    fn load_repository_instructions(
        working_dir: Option<&Path>,
        project_root: Option<&Path>,
    ) -> Option<String> {
        let working_dir = working_dir?;
        let resolver =
            echo_agent::project_rules::InstructionResolver::new(working_dir).agents_files_only();
        let resolved = match project_root {
            Some(root) => resolver.project_root(root).resolve(),
            None => resolver.resolve(),
        };
        resolved.annotated()
    }

    /// Load local-directory instructions from `<cwd>/.eko/local.md`.
    fn load_local_instructions(root: Option<&Path>) -> Option<String> {
        root.map(|path| path.join(".eko").join("local.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// Save project-level instructions to `<cwd>/.eko/project.md`.
    pub fn save_project_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::project_instructions_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    /// Save user-level instructions to `~/.eko/user.md`.
    pub fn save_user_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::user_instructions_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    /// Path to the project-level instructions file.
    fn project_instructions_path() -> PathBuf {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        crate::utils::find_project_root(&working_dir)
            .unwrap_or(working_dir)
            .join(".eko")
            .join("project.md")
    }

    /// Path to the user-level instructions file.
    fn user_instructions_path() -> PathBuf {
        crate::data_root::user_data_path("user.md")
    }

    /// Load hot-layer memory content from `.eko/MEMORY.md`.
    ///
    /// Returns the body (frontmatter stripped) so it can be included in the system prompt.
    fn load_hot_memory(project_root: Option<&Path>) -> Option<String> {
        let project_path = project_root.map(|root| root.join(".eko").join("MEMORY.md"));
        let path = project_path
            .filter(|path| path.exists())
            .unwrap_or_else(|| crate::data_root::user_data_path("MEMORY.md"));
        let raw = std::fs::read_to_string(path).ok()?;

        Some(crate::utils::strip_yaml_frontmatter(&raw))
    }

    /// Load auto-promoted rules from `<project-root>/.eko/learned-rules.md`.
    ///
    /// Contains rules that were automatically promoted from high-confidence
    /// memories by the evolution system's `RulePromoter`.
    fn load_agents_instructions(project_root: Option<&Path>) -> Option<String> {
        let root = project_root?;
        let path = root.join(".eko").join(LEARNED_RULES_FILE);
        let raw = std::fs::read_to_string(path).ok()?;
        Some(crate::utils::strip_yaml_frontmatter(&raw))
    }

    /// Path to the learned-rules file (auto-promoted rules written by `RulePromoter`).
    ///
    /// Resolves the project root via marker-based discovery. Returns a relative
    /// `.eko/learned-rules.md` path when no project root is found so callers get
    /// a deterministic write target.
    pub fn agents_instructions_path() -> PathBuf {
        std::env::current_dir()
            .ok()
            .and_then(|pwd| crate::utils::find_project_root(&pwd))
            .map(|root| root.join(".eko").join(LEARNED_RULES_FILE))
            .unwrap_or_else(|| std::path::PathBuf::from(".eko").join(LEARNED_RULES_FILE))
    }

    /// Save auto-promoted rules to the learned-rules file.
    pub fn save_agents_instructions(content: &str) -> std::io::Result<()> {
        let path = Self::agents_instructions_path();
        Self::save_agents_instructions_at(&path, content)
    }

    /// Save auto-promoted rules to one previously resolved authority path.
    pub fn save_agents_instructions_at(path: &Path, content: &str) -> std::io::Result<()> {
        echo_agent::utils::fs::atomic_write(path, content.as_bytes())
    }
}

pub(crate) fn strict_optional_text(path: Option<&Path>) -> std::io::Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "instruction source {} must be a regular non-symlink file",
                path.display()
            ),
        ));
    }
    std::fs::read_to_string(path).map(Some)
}

fn strict_learned_rules(project_root: Option<&Path>) -> std::io::Result<Option<String>> {
    let Some(root) = project_root else {
        return Ok(None);
    };
    let current = root.join(".eko").join(LEARNED_RULES_FILE);
    strict_optional_text(Some(&current))
}

fn strict_repository_instructions(
    working_dir: Option<&Path>,
    project_root: Option<&Path>,
) -> std::io::Result<Option<String>> {
    repository_instructions_with_reader(working_dir, project_root, |path| {
        strict_optional_text(Some(path))
    })
}

fn repository_instructions_with_reader<E: From<std::io::Error>>(
    working_dir: Option<&Path>,
    project_root: Option<&Path>,
    read: impl Fn(&Path) -> Result<Option<String>, E>,
) -> Result<Option<String>, E> {
    let Some(working_dir) = working_dir else {
        return Ok(None);
    };
    let scan_root = project_root.unwrap_or(working_dir);
    if !working_dir.starts_with(scan_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "instruction working directory {} escapes project root {}",
                working_dir.display(),
                scan_root.display()
            ),
        )
        .into());
    }
    let mut directories = working_dir
        .ancestors()
        .take_while(|directory| directory.starts_with(scan_root))
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    directories.reverse();
    let mut blocks = Vec::new();
    for directory in directories {
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let path = directory.join(name);
            let Some(content) = read(&path)? else {
                continue;
            };
            if content.trim().is_empty() {
                continue;
            }
            blocks.push(format!(
                "<!-- PROJECT INSTRUCTIONS: {} -->\n{}\n<!-- END PROJECT INSTRUCTIONS -->",
                path.display(),
                content.trim()
            ));
            break;
        }
    }
    Ok((!blocks.is_empty()).then(|| blocks.join("\n\n")))
}

/// 读一个"可选"投影文件：不存在返回 `None`，符号链接/非普通文件与读取、
/// UTF-8 失败仍按原顺序失败；只有正文可能来自指纹缓存。
fn project_optional_text(
    files: &dyn echo_agent::tools::filesystem::FileSystem,
    path: Option<&Path>,
) -> echo_agent::error::Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let namespace = files.cache_namespace();
    let metadata = match files.symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(echo_agent::error::ReactError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            // 文件已经消失：旧正文必须同时失效，不能继续返回上一条消息的规则。
            if let Some(namespace) = namespace.as_deref() {
                instruction_cache_remove(namespace, path);
            }
            return Ok(None);
        }
        Err(error) => {
            // stat 都不能完成的条目不可信；保留原来的错误类型与传播顺序。
            if let Some(namespace) = namespace.as_deref() {
                instruction_cache_remove(namespace, path);
            }
            return Err(error);
        }
    };
    if !metadata.is_file() || metadata.file_type.is_symlink() {
        if let Some(namespace) = namespace.as_deref() {
            instruction_cache_remove(namespace, path);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "instruction source {} must be a regular non-symlink file",
                path.display()
            ),
        )
        .into());
    }
    let Some(namespace) = namespace else {
        // 没有命名空间的提供者（默认/本地）逐字保持改动前的行为。
        return project_text(files, path).map(Some);
    };
    // 提供者不报告 mtime 时无法证明内容未变：既不读缓存也不写缓存。
    let Some(modified) = metadata.modified else {
        instruction_cache_remove(namespace.as_str(), path);
        return project_text(files, path).map(Some);
    };
    if instruction_fingerprint_is_stable(modified)
        && let Some(content) = instruction_cache_lookup(
            namespace.as_str(),
            path,
            modified,
            metadata.len,
            metadata.file_type,
        )
    {
        return Ok(Some(content));
    }
    let content = match project_text(files, path) {
        Ok(content) => content,
        Err(error) => {
            // 读取或 UTF-8 校验失败：旧正文不能再充当"当前规则"。
            instruction_cache_remove(namespace.as_str(), path);
            return Err(error);
        }
    };
    instruction_cache_store(
        namespace.as_str(),
        path,
        CachedInstructionBody {
            modified,
            len: metadata.len,
            file_type: metadata.file_type,
            content: content.clone(),
        },
    );
    Ok(Some(content))
}

fn project_text(
    files: &dyn echo_agent::tools::filesystem::FileSystem,
    path: &Path,
) -> echo_agent::error::Result<String> {
    String::from_utf8(files.read(path)?)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error).into())
}

impl Default for InstructionProvider {
    fn default() -> Self {
        Self::load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tools::files::LocalFileSystem;
    use echo_agent::tools::filesystem::{FileEntry, FileMetadata, FileSystem};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 夹具报告的 mtime 策略：真实值、固定值，或模拟不暴露 mtime 的提供者。
    #[derive(Clone, Copy)]
    enum ModifiedPolicy {
        Real,
        Fixed(SystemTime),
        Missing,
    }

    /// 计数/注入夹具：真实读取走本地实现，但命名空间和 mtime 由测试决定，
    /// 以便确定性验证缓存命中、秒级窗口与跨目标隔离。
    ///
    /// 不直接改宿主文件 mtime：测试不受文件系统时间精度和并发写入影响。
    struct FingerprintFiles {
        namespace: Option<String>,
        modified: Mutex<ModifiedPolicy>,
        stats: AtomicUsize,
        reads: AtomicUsize,
    }

    impl FingerprintFiles {
        fn new(namespace: Option<String>) -> Self {
            Self {
                namespace,
                modified: Mutex::new(ModifiedPolicy::Real),
                stats: AtomicUsize::new(0),
                reads: AtomicUsize::new(0),
            }
        }

        fn set_modified(&self, modified: Option<SystemTime>) {
            let policy = match modified {
                Some(modified) => ModifiedPolicy::Fixed(modified),
                None => ModifiedPolicy::Missing,
            };
            *self
                .modified
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
        }

        fn reset_counts(&self) {
            self.stats.store(0, Ordering::SeqCst);
            self.reads.store(0, Ordering::SeqCst);
        }

        fn stats(&self) -> usize {
            self.stats.load(Ordering::SeqCst)
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl FileSystem for FingerprintFiles {
        fn cache_namespace(&self) -> Option<String> {
            self.namespace.clone()
        }

        fn current_dir(&self) -> echo_agent::error::Result<PathBuf> {
            LocalFileSystem.current_dir()
        }

        fn canonicalize(&self, path: &Path) -> echo_agent::error::Result<PathBuf> {
            LocalFileSystem.canonicalize(path)
        }

        fn metadata(&self, path: &Path) -> echo_agent::error::Result<FileMetadata> {
            self.stats.fetch_add(1, Ordering::SeqCst);
            LocalFileSystem.metadata(path)
        }

        fn symlink_metadata(&self, path: &Path) -> echo_agent::error::Result<FileMetadata> {
            self.stats.fetch_add(1, Ordering::SeqCst);
            let mut metadata = LocalFileSystem.symlink_metadata(path)?;
            match *self
                .modified
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            {
                ModifiedPolicy::Real => {}
                ModifiedPolicy::Fixed(modified) => metadata.modified = Some(modified),
                ModifiedPolicy::Missing => metadata.modified = None,
            }
            Ok(metadata)
        }

        fn read(&self, path: &Path) -> echo_agent::error::Result<Vec<u8>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            LocalFileSystem.read(path)
        }

        fn read_dir(&self, path: &Path) -> echo_agent::error::Result<Vec<FileEntry>> {
            LocalFileSystem.read_dir(path)
        }

        fn create_dir_all(&self, path: &Path) -> echo_agent::error::Result<()> {
            LocalFileSystem.create_dir_all(path)
        }

        fn create_file(&self, path: &Path) -> echo_agent::error::Result<()> {
            LocalFileSystem.create_file(path)
        }

        fn write(&self, path: &Path, bytes: &[u8]) -> echo_agent::error::Result<()> {
            LocalFileSystem.write(path, bytes)
        }

        fn append(&self, path: &Path, bytes: &[u8]) -> echo_agent::error::Result<()> {
            LocalFileSystem.append(path, bytes)
        }

        fn rename(&self, from: &Path, to: &Path) -> echo_agent::error::Result<()> {
            LocalFileSystem.rename(from, to)
        }

        fn remove_file(&self, path: &Path) -> echo_agent::error::Result<()> {
            LocalFileSystem.remove_file(path)
        }

        fn remove_dir(&self, path: &Path) -> echo_agent::error::Result<()> {
            LocalFileSystem.remove_dir(path)
        }

        fn remove_dir_all(&self, path: &Path) -> echo_agent::error::Result<()> {
            LocalFileSystem.remove_dir_all(path)
        }

        fn set_mode(&self, path: &Path, mode: u32) -> echo_agent::error::Result<()> {
            LocalFileSystem.set_mode(path, mode)
        }

        fn atomic_write(&self, path: &Path, bytes: &[u8]) -> echo_agent::error::Result<()> {
            LocalFileSystem.atomic_write(path, bytes)
        }

        fn checkpoint(&self, path: &Path) -> echo_agent::error::Result<Option<String>> {
            LocalFileSystem.checkpoint(path)
        }
    }

    /// 每个夹具用独立命名空间，避免并行测试共享进程级缓存时互相命中。
    fn fixture_namespace(temp: &tempfile::TempDir) -> String {
        format!("test-namespace:{}", temp.path().display())
    }

    fn stable_modified() -> SystemTime {
        SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    #[test]
    fn fingerprint_cache_reuses_body_without_read_but_still_stats()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, "CACHED_RULE")?;
        let files = FingerprintFiles::new(Some(fixture_namespace(&temp)));
        files.set_modified(Some(stable_modified()));

        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("CACHED_RULE")
        );
        assert_eq!((files.reads(), files.stats()), (1, 1));

        files.reset_counts();
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("CACHED_RULE")
        );
        // 命中只证明"这次没有 read"，仍有一次 symlink_metadata 证明指纹没变。
        assert_eq!((files.reads(), files.stats()), (0, 1));
        Ok(())
    }

    #[test]
    fn fingerprint_cache_reads_new_body_after_change()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, "VERSION_ONE")?;
        let files = FingerprintFiles::new(Some(fixture_namespace(&temp)));
        let first_modified = stable_modified();
        files.set_modified(Some(first_modified));
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("VERSION_ONE")
        );

        std::fs::write(&path, "VERSION_TWO")?;
        files.set_modified(first_modified.checked_add(Duration::from_secs(10)));
        files.reset_counts();
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("VERSION_TWO")
        );
        assert_eq!(files.reads(), 1);
        Ok(())
    }

    #[test]
    fn fingerprint_cache_forces_reread_for_recent_modified_time()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("AGENTS.md");
        // 同一秒内、长度相同的两次编辑：mtime/len 指纹完全一致，
        // 只有"2 秒内强制重读"能防住旧正文。
        let recent = SystemTime::now()
            .checked_sub(Duration::from_millis(500))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        std::fs::write(&path, "SEC_ONE")?;
        let files = FingerprintFiles::new(Some(fixture_namespace(&temp)));
        files.set_modified(Some(recent));
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("SEC_ONE")
        );

        std::fs::write(&path, "SEC_TWO")?;
        files.reset_counts();
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("SEC_TWO")
        );
        assert_eq!(files.reads(), 1);
        Ok(())
    }

    #[test]
    fn fingerprint_cache_isolates_namespaces() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("AGENTS.md");
        let modified = stable_modified();
        std::fs::write(&path, "A_CONTENT")?;
        let left = FingerprintFiles::new(Some(format!("left:{}", temp.path().display())));
        left.set_modified(Some(modified));
        assert_eq!(
            project_optional_text(&left, Some(&path))?.as_deref(),
            Some("A_CONTENT")
        );

        // 同一绝对路径、同一 len、同一 mtime，只是物理目标不同：
        // B 必须真的读取，绝不能返回 A 的正文。
        std::fs::write(&path, "B_CONTENT")?;
        let right = FingerprintFiles::new(Some(format!("right:{}", temp.path().display())));
        right.set_modified(Some(modified));
        assert_eq!(
            project_optional_text(&right, Some(&path))?.as_deref(),
            Some("B_CONTENT")
        );
        assert_eq!(right.reads(), 1);

        // A 自己的缓存仍只属于 A：磁盘现在虽然是 B_CONTENT，A 仍返回它的旧正文。
        left.reset_counts();
        assert_eq!(
            project_optional_text(&left, Some(&path))?.as_deref(),
            Some("A_CONTENT")
        );
        assert_eq!(left.reads(), 0);
        Ok(())
    }

    #[test]
    fn fingerprint_cache_is_disabled_without_namespace()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, "LOCAL_RULE")?;
        let files = FingerprintFiles::new(None);
        files.set_modified(Some(stable_modified()));

        for _ in 0..2 {
            assert_eq!(
                project_optional_text(&files, Some(&path))?.as_deref(),
                Some("LOCAL_RULE")
            );
        }
        // 没有命名空间时与改动前逐字一致：每次都真的读。
        assert_eq!((files.reads(), files.stats()), (2, 2));
        Ok(())
    }

    #[test]
    fn fingerprint_cache_drops_body_after_deletion()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("AGENTS.md");
        let modified = stable_modified();
        std::fs::write(&path, "DELETED_V1")?;
        let files = FingerprintFiles::new(Some(fixture_namespace(&temp)));
        files.set_modified(Some(modified));
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("DELETED_V1")
        );

        std::fs::remove_file(&path)?;
        files.reset_counts();
        assert!(project_optional_text(&files, Some(&path))?.is_none());
        assert_eq!(files.reads(), 0);

        // 同长度、同 mtime 重建：若删除时没有清除缓存键，这里会错误命中旧正文。
        std::fs::write(&path, "DELETED_V2")?;
        files.reset_counts();
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("DELETED_V2")
        );
        assert_eq!(files.reads(), 1);
        Ok(())
    }

    #[test]
    fn fingerprint_cache_does_not_remember_missing_modified()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, "NO_MTIME")?;
        let files = FingerprintFiles::new(Some(fixture_namespace(&temp)));
        files.set_modified(Some(stable_modified()));
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("NO_MTIME")
        );

        // 提供者不再报告 mtime：不能继续复用之前缓存的正文。
        std::fs::write(&path, "NO_MTIME_V2")?;
        files.set_modified(None);
        files.reset_counts();
        assert_eq!(
            project_optional_text(&files, Some(&path))?.as_deref(),
            Some("NO_MTIME_V2")
        );
        assert_eq!(files.reads(), 1);
        Ok(())
    }

    #[test]
    fn repository_chain_keeps_override_priority_across_cached_loads()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".eko"))?;
        std::fs::write(root.join("AGENTS.md"), "ROOT_RULE")?;
        std::fs::write(root.join("AGENTS.override.md"), "OVERRIDE_RULE")?;
        let files = FingerprintFiles::new(Some(fixture_namespace(&temp)));
        let modified = stable_modified();
        files.set_modified(Some(modified));

        let first =
            InstructionProvider::load_for_resolved_project(Some(&root), Some(&root), None, &files)?;
        let repository = first.repository_level.unwrap_or_default();
        assert!(repository.contains("OVERRIDE_RULE"));
        assert!(!repository.contains("ROOT_RULE"));

        files.reset_counts();
        let second =
            InstructionProvider::load_for_resolved_project(Some(&root), Some(&root), None, &files)?;
        assert_eq!(second.repository_level, Some(repository));
        // 3 个候选各一次 symlink_metadata；正文全部命中，不产生 read。
        assert_eq!(files.stats(), 3);
        assert_eq!(files.reads(), 0);

        std::fs::write(root.join("AGENTS.override.md"), "OVERRIDE_RULE_V2")?;
        files.set_modified(modified.checked_add(Duration::from_secs(10)));
        files.reset_counts();
        let third =
            InstructionProvider::load_for_resolved_project(Some(&root), Some(&root), None, &files)?;
        let repository = third.repository_level.unwrap_or_default();
        assert!(repository.contains("OVERRIDE_RULE_V2"));
        assert!(!repository.contains("ROOT_RULE"));
        assert_eq!(files.reads(), 1);
        Ok(())
    }

    #[test]
    fn test_load_empty_instructions() {
        // This test assumes no instruction files exist
        let instructions = InstructionProvider {
            project_level: None,
            user_level: None,
            repository_level: None,
            local_level: None,
            agents_level: None,
            hot_memory: None,
        };
        assert!(instructions.get_system_prompt_suffix().is_empty());
    }

    #[test]
    fn load_agents_instructions_reads_new_name() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let eko = root.join(".eko");
        std::fs::create_dir_all(&eko)?;
        std::fs::create_dir(root.join(".git"))?;
        assert_eq!(crate::utils::find_project_root(root).as_deref(), Some(root));
        std::fs::write(eko.join(LEARNED_RULES_FILE), "RULE_BODY")?;

        let provider = InstructionProvider::load_for(Some(root));
        assert_eq!(provider.agents_level.as_deref(), Some("RULE_BODY"));
        Ok(())
    }

    #[test]
    fn learned_rules_do_not_interpret_eko_agents_file() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let eko = temp.path().join(".eko");
        std::fs::create_dir_all(&eko)?;
        std::fs::write(eko.join("AGENTS.md"), "NOT_LEARNED_RULES")?;

        let provider = InstructionProvider::load_for(Some(temp.path()));
        assert!(provider.agents_level.is_none());
        assert!(eko.join("AGENTS.md").exists());
        Ok(())
    }

    #[test]
    fn loads_agents_chain_without_echo_agent_namespace() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let child = root.join("src");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".echo-agent"))?;
        std::fs::create_dir_all(&child)?;
        std::fs::write(root.join("AGENTS.md"), "ROOT_RULE")?;
        std::fs::write(child.join("AGENTS.override.md"), "CHILD_RULE")?;
        std::fs::write(root.join(".echo-agent/AGENT.md"), "NOT_EKO_PROTOCOL")?;

        let provider = InstructionProvider::load_for(Some(&child));
        let repository = provider.repository_level.unwrap_or_default();
        assert!(repository.contains("ROOT_RULE"));
        assert!(repository.contains("CHILD_RULE"));
        assert!(!repository.contains("NOT_EKO_PROTOCOL"));
        Ok(())
    }

    #[test]
    fn strict_loader_keeps_project_agents_and_eko_state_rules_distinct() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        let state = temp.path().join("state/.eko");
        std::fs::create_dir_all(project.join(".git"))?;
        std::fs::create_dir_all(project.join(".eko"))?;
        std::fs::create_dir_all(&state)?;
        std::fs::write(project.join("AGENTS.md"), "PROJECT_AGENT_RULE")?;
        std::fs::write(state.join("learned-rules.md"), "STATE_LEARNED_RULE")?;

        let provider =
            InstructionProvider::load_for_strict_with_state(Some(&project), Some(&state))?;
        let repository = provider.repository_level.unwrap_or_default();
        let learned = provider.agents_level.unwrap_or_default();
        assert!(repository.contains("PROJECT_AGENT_RULE"));
        assert_eq!(learned, "STATE_LEARNED_RULE");
        Ok(())
    }

    #[test]
    fn strict_loader_does_not_fallback_to_project_state_files() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        let state = temp.path().join("state/.eko");
        std::fs::create_dir_all(project.join(".git"))?;
        std::fs::create_dir_all(project.join(".eko"))?;
        std::fs::create_dir_all(&state)?;
        std::fs::write(project.join(".eko/learned-rules.md"), "PROJECT_LEARNED")?;
        std::fs::write(project.join(".eko/MEMORY.md"), "PROJECT_MEMORY")?;

        let provider =
            InstructionProvider::load_for_strict_with_state(Some(&project), Some(&state))?;
        assert!(provider.agents_level.is_none());
        assert!(
            !provider
                .hot_memory
                .as_deref()
                .is_some_and(|memory| memory.contains("PROJECT_MEMORY"))
        );
        Ok(())
    }

    #[test]
    fn local_instructions_are_relative_to_working_directory() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let child = root.join("src");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(child.join(".eko"))?;
        std::fs::write(child.join(".eko/local.md"), "LOCAL_CHILD_RULE")?;

        let provider = InstructionProvider::load_for(Some(&child));
        assert_eq!(provider.local_level.as_deref(), Some("LOCAL_CHILD_RULE"));
        Ok(())
    }

    #[test]
    fn strict_loader_rejects_invalid_utf8_in_learned_rules() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".eko"))?;
        std::fs::write(root.join(".eko/learned-rules.md"), [0xff, 0xfe])?;

        assert!(InstructionProvider::load_for_strict(Some(&root)).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn strict_loader_rejects_symlinked_instruction_source() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".eko"))?;
        let outside = temp.path().join("outside.md");
        std::fs::write(&outside, "outside")?;
        std::os::unix::fs::symlink(&outside, root.join(".eko/learned-rules.md"))?;

        assert!(InstructionProvider::load_for_strict(Some(&root)).is_err());
        Ok(())
    }
}
