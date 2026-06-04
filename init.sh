#!/usr/bin/env bash
#
# init.sh - EchoCoWork 初始化脚本
#
# 功能:
#   1. 检查并安装 Rust 工具链 (cargo + rustc)
#   2. 检查并安装 Node.js (>= 18)（仅 GUI 模式）
#   3. 安装前端依赖（仅 GUI 模式）
#   4. 编译 TUI 或 GUI 版本
#
# 用法:
#   ./init.sh                  # 编译 TUI 版本（默认）
#   ./init.sh --release        # 编译 TUI Release 版本
#   ./init.sh --gui            # 编译 GUI 桌面应用版本
#   ./init.sh --gui --release  # 编译 GUI Release 版本

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_NAME="echo-agent-cli"
NODE_MIN_VERSION=18

# ── 颜色定义 ────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# ── 日志输出 ────────────────────────────────────────────────────
log_info()  { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }
log_step()  { echo -e "${BLUE}[STEP]${NC} $1"; }

# ── 检测操作系统 ──────────────────────────────────────────────
detect_os() {
    case "$(uname -s)" in
        Linux*)     echo "linux";;
        Darwin*)    echo "macos";;
        CYGWIN*|MINGW*|MSYS*) echo "windows";;
        *)          echo "unknown";;
    esac
}

OS=$(detect_os)
if [[ "$OS" == "unknown" ]]; then
    log_error "不支持的操作系统: $(uname -s)"
    exit 1
fi

if [[ "$OS" == "windows" ]]; then
    log_error "Windows 请使用 WSL2 或 PowerShell 脚本"
    exit 1
fi

# ── 检查命令是否存在 ──────────────────────────────────────────
command_exists() {
    command -v "$1" &>/dev/null
}

# ── 检查并安装 Rust ───────────────────────────────────────────
setup_rust() {
    log_step "检查 Rust 工具链..."

    if command_exists rustc && command_exists cargo; then
        local rust_version
        rust_version=$(rustc --version 2>/dev/null | awk '{print $2}')
        log_info "Rust 已安装: $rust_version"
    else
        log_warn "Rust 未安装，准备安装..."
        install_rust
    fi
}

install_rust() {
    log_step "正在安装 Rust (通过 rustup)..."

    if ! command_exists curl; then
        log_error "缺少 curl，请先安装 curl"
        exit 1
    fi

    # 安装 rustup
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable

    # 加载 rustup 环境
    source "$HOME/.cargo/env"

    # 验证安装
    if command_exists rustc && command_exists cargo; then
        log_info "Rust 安装成功: $(rustc --version)"
    else
        log_error "Rust 安装失败"
        exit 1
    fi
}

# ── 检查并安装 Node.js ────────────────────────────────────────
setup_node() {
    log_step "检查 Node.js..."

    if command_exists node; then
        local node_version
        node_version=$(node --version 2>/dev/null | sed 's/^v//')
        local major_version
        major_version=$(echo "$node_version" | cut -d. -f1)

        if [[ "$major_version" -ge "$NODE_MIN_VERSION" ]]; then
            log_info "Node.js 已安装: v$node_version"
        else
            log_warn "Node.js 版本过低 (v$node_version)，需要 >= v$NODE_MIN_VERSION"
            install_node
        fi
    else
        log_warn "Node.js 未安装，准备安装..."
        install_node
    fi
}

install_node() {
    log_step "正在安装 Node.js v${NODE_MIN_VERSION}.x (通过 nvm)..."

    # 尝试安装 nvm
    if ! command_exists nvm; then
        if [[ -d "$HOME/.nvm" ]]; then
            export NVM_DIR="$HOME/.nvm"
            # shellcheck source=/dev/null
            [[ -s "$NVM_DIR/nvm.sh" ]] && \. "$NVM_DIR/nvm.sh"
        else
            log_warn "nvm 未安装，尝试安装 nvm..."
            curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash
            export NVM_DIR="$HOME/.nvm"
            # shellcheck source=/dev/null
            [[ -s "$NVM_DIR/nvm.sh" ]] && \. "$NVM_DIR/nvm.sh"
        fi
    fi

    if command_exists nvm; then
        nvm install "$NODE_MIN_VERSION"
        nvm use "$NODE_MIN_VERSION"
        nvm alias default "$NODE_MIN_VERSION"
    else
        # 回退: 使用系统包管理器
        if [[ "$OS" == "macos" ]]; then
            if command_exists brew; then
                brew install node@"$NODE_MIN_VERSION"
            else
                log_error "请先安装 Homebrew 或手动安装 Node.js >= v$NODE_MIN_VERSION"
                exit 1
            fi
        else
            log_error "请先安装 nvm 或手动安装 Node.js >= v$NODE_MIN_VERSION"
            log_info "推荐命令: curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash"
            exit 1
        fi
    fi

    # 验证安装
    if command_exists node; then
        log_info "Node.js 安装成功: $(node --version)"
        log_info "npm 版本: $(npm --version)"
    else
        log_error "Node.js 安装失败"
        exit 1
    fi
}

# ── 安装前端依赖 ──────────────────────────────────────────────
setup_frontend() {
    local frontend_dir="$SCRIPT_DIR/web-frontend"

    if [[ ! -d "$frontend_dir" ]]; then
        log_warn "web-frontend 目录不存在，跳过前端依赖安装"
        return 0
    fi

    log_step "安装前端依赖..."
    cd "$frontend_dir"

    if [[ ! -f "package.json" ]]; then
        log_warn "web-frontend/package.json 不存在，跳过前端依赖安装"
        return 0
    fi

    if [[ -d "node_modules" ]]; then
        log_info "前端依赖已存在"
    else
        log_info "正在执行 npm install..."
        npm install
    fi

    cd "$SCRIPT_DIR"
}

# ── 编译 TUI 版本 ─────────────────────────────────────────────
build_tui() {
    log_step "编译 TUI 版本 ($PROJECT_NAME)..."

    cd "$SCRIPT_DIR"

    # 检查是否已经有 echo-agent 框架（本地路径依赖）
    if [[ ! -d "$SCRIPT_DIR/../echo-agent" ]]; then
        log_warn "echo-agent 框架未在同级目录中找到"
        log_info "项目依赖本地路径 ../echo-agent，请确保 echo-agent 框架已 clone"
    fi

    # 编译 Rust TUI（显式指定 features，避免拉入 GUI 依赖）
    log_step "编译 Rust 项目..."
    if [[ "${1:-}" == "--release" ]]; then
        cargo build --bin echo-agent-cli --no-default-features --features tui --release
        log_info "✅ TUI 编译完成 (Release)"
        log_info "可执行文件: $SCRIPT_DIR/target/release/echo-agent-cli"
    else
        cargo build --bin echo-agent-cli --no-default-features --features tui
        log_info "✅ TUI 编译完成 (Debug)"
        log_info "可执行文件: $SCRIPT_DIR/target/debug/echo-agent-cli"
    fi
}

# ── 编译 GUI 版本 ─────────────────────────────────────────────
build_gui() {
    log_step "编译 GUI 版本 ($PROJECT_NAME — Tauri 桌面应用)..."

    cd "$SCRIPT_DIR"

    # 检查 echo-agent 框架
    if [[ ! -d "$SCRIPT_DIR/../echo-agent" ]]; then
        log_warn "echo-agent 框架未在同级目录中找到"
        log_info "项目依赖本地路径 ../echo-agent，请确保 echo-agent 框架已 clone"
    fi

    # 构建前端资源
    if [[ -d "$SCRIPT_DIR/web-frontend" ]]; then
        log_step "构建前端资源..."
        cd "$SCRIPT_DIR/web-frontend"
        if [[ -f "package.json" ]]; then
            npm run build 2>/dev/null || { log_error "前端构建失败"; exit 1; }
        fi
        cd "$SCRIPT_DIR"
    else
        log_error "web-frontend 目录不存在，GUI 构建需要前端资源"
        exit 1
    fi

    # 编译 Rust GUI（显式指定 features）
    log_step "编译 Rust Tauri 应用..."
    if [[ "${1:-}" == "--release" ]]; then
        cargo build --bin echo-agent-tauri --no-default-features --features gui --release
        log_info "✅ GUI 编译完成 (Release)"
        log_info "可执行文件: $SCRIPT_DIR/target/release/echo-agent-tauri"
    else
        cargo build --bin echo-agent-tauri --no-default-features --features gui
        log_info "✅ GUI 编译完成 (Debug)"
        log_info "可执行文件: $SCRIPT_DIR/target/debug/echo-agent-tauri"
    fi
}

# ── 主入口 ────────────────────────────────────────────────────
main() {
    local build_mode="tui"  # 默认编译 TUI
    local release_flag=""

    # 解析参数
    for arg in "$@"; do
        case "$arg" in
            --gui)    build_mode="gui" ;;
            --release) release_flag="--release" ;;
            *)        log_warn "未知参数: $arg" ;;
        esac
    done

    echo -e "${CYAN}"
    echo "╔══════════════════════════════════════════════════════════════╗"
    if [[ "$build_mode" == "gui" ]]; then
        echo "║    EchoCoWork — 初始化脚本 (GUI 桌面应用)                    ║"
    else
        echo "║    EchoCoWork — 初始化脚本 (TUI 终端界面)                     ║"
    fi
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"

    cd "$SCRIPT_DIR"

    # 1. 安装 Rust（TUI 和 GUI 都需要）
    setup_rust

    if [[ "$build_mode" == "gui" ]]; then
        # GUI 需要 Node.js 和前端依赖
        setup_node
        setup_frontend
        build_gui "$release_flag"
    else
        # TUI 不需要 Node.js
        build_tui "$release_flag"
    fi

    echo ""
    log_info "🎉 初始化完成！"
    echo ""
    echo -e "${GREEN}快速启动:${NC}"
    if [[ "$build_mode" == "gui" ]]; then
        if [[ "$release_flag" == "--release" ]]; then
            echo -e "  ${CYAN}./target/release/echo-agent-tauri${NC}"
        else
            echo -e "  ${CYAN}./target/debug/echo-agent-tauri${NC}"
        fi
    else
        if [[ "$release_flag" == "--release" ]]; then
            echo -e "  ${CYAN}./target/release/echo-agent-cli${NC}"
        else
            echo -e "  ${CYAN}./target/debug/echo-agent-cli${NC}"
        fi
    fi
    echo ""
    if [[ "$build_mode" == "tui" ]]; then
        echo -e "${YELLOW}提示: 使用 --gui 编译 GUI 桌面应用版本${NC}"
        echo -e "${YELLOW}提示: 使用 --release 编译生产版本${NC}"
    else
        echo -e "${YELLOW}提示: 使用 --release 编译生产版本${NC}"
    fi
}

# ── 执行 ───────────────────────────────────────────────────────
main "$@"
