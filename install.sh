#!/bin/bash
#
# LLMux CLI - Unix Bootstrapper Script (Bash/Zsh)
#
# Clones the repository, builds the native binary, and installs it locally.
#

set -euo pipefail

# 1. Default Setup & Constants
REPO_URL="https://github.com/zhMoody/llmux-cli-rs.git"
RELEASE_REPO_URL="https://github.com/zhMoody/llmux-cli-rs"
REPO_PATH="zhMoody/llmux-cli-rs"
SYSTEM_BIN_DIR=""
TARGET_DIR="$HOME/.local/bin"
CUSTOM_DIR=""
SOURCE_DIR=""
WORK_DIR=""
SHOULD_CLEANUP=false
TMP_BASE_DIR="${TMPDIR:-/tmp}"
INSTALL_MODE=""
UI_LANG=""
OS_NAME=""
ARCH_NAME=""
DOWNLOAD_URL=""
IS_INTERACTIVE=false
UI_MODE="text"
INSTALLED_VERSION=""
CANDIDATE_VERSION=""
SETUP_PATH="auto"

# GitHub release 下载加速镜像（空格分隔；末尾留空 = 直连兜底）
# 用环境变量 LLMUX_MIRRORS 覆盖，例如：LLMUX_MIRRORS="https://ghfast.top"
# 设为单个空格可强制只走直连：LLMUX_MIRRORS=" "
GITHUB_MIRRORS="${LLMUX_MIRRORS:-https://ghfast.top https://ghproxy.net}"

# Helper: Display usage instructions
show_usage() {
    echo "Usage: install.sh [options]"
    echo ""
    echo "Options:"
    echo "  -d, --dir <path>    Specify a custom directory to install the binary"
    echo "  -s, --source <path> Use an existing local checkout instead of cloning"
    echo "  -m, --mode <auto|release|source>  Choose install mode"
    echo "  -l, --lang <auto|zh|en>           Choose UI language"
    echo "  -u, --ui <text|dialog>            Choose interactive UI style"
    echo "  -p, --path <auto|yes|no>          Help set PATH after install"
    echo "  -h, --help          Display this help menu"
    echo ""
}

# Parse Arguments
while [ "$#" -gt 0 ]; do
    case $1 in
        -d|--dir)
            if [ -z "${2:-}" ]; then
                echo "[ERROR] Missing path for --dir parameter" >&2
                exit 1
            fi
            CUSTOM_DIR="$2"
            shift
            ;;
        -s|--source)
            if [ -z "${2:-}" ]; then
                echo "[ERROR] Missing path for --source parameter" >&2
                exit 1
            fi
            SOURCE_DIR="$2"
            shift
            ;;
        -m|--mode)
            if [ -z "${2:-}" ]; then
                echo "[ERROR] Missing value for --mode parameter" >&2
                exit 1
            fi
            INSTALL_MODE="$2"
            shift
            ;;
        -l|--lang)
            if [ -z "${2:-}" ]; then
                echo "[ERROR] Missing value for --lang parameter" >&2
                exit 1
            fi
            UI_LANG="$2"
            shift
            ;;
        -u|--ui)
            if [ -z "${2:-}" ]; then
                echo "[ERROR] Missing value for --ui parameter" >&2
                exit 1
            fi
            UI_MODE="$2"
            shift
            ;;
        -p|--path)
            if [ -z "${2:-}" ]; then
                echo "[ERROR] Missing value for --path parameter" >&2
                exit 1
            fi
            SETUP_PATH="$2"
            shift
            ;;
        -h|--help)
            show_usage
            exit 0
            ;;
        *)
            echo "[ERROR] Unknown parameter: $1" >&2
            show_usage
            exit 1
            ;;
    esac
    shift
done

# Resolve dynamic installation target directory
if [ -d "/opt/homebrew/bin" ] && [ -w "/opt/homebrew/bin" ]; then
    SYSTEM_BIN_DIR="/opt/homebrew/bin"
elif [ -d "/usr/local/bin" ] && [ -w "/usr/local/bin" ]; then
    SYSTEM_BIN_DIR="/usr/local/bin"
fi

if [ -n "$SYSTEM_BIN_DIR" ]; then
    TARGET_DIR="$SYSTEM_BIN_DIR"
fi

if [ -n "$CUSTOM_DIR" ]; then
    TARGET_DIR="$CUSTOM_DIR"
fi

echo "LLMux installer starting..."
echo ""

detect_language() {
    if [ -n "$UI_LANG" ] && [ "$UI_LANG" != "auto" ]; then
        return 0
    fi

    UI_LANG=""
}

# 优先用 GitHub API 取最新 tag（api.github.com 比 releases/latest 重定向稳）
# 失败再回退到原来的 releases/latest 重定向方式
resolve_latest_release_tag() {
    local api_url="https://api.github.com/repos/${REPO_PATH}/releases/latest"
    local tag=""

    if command -v curl >/dev/null 2>&1; then
        tag=$(curl --connect-timeout 10 --max-time 20 -fsSL "$api_url" 2>/dev/null \
              | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
    fi
    if [ -z "$tag" ] && command -v wget >/dev/null 2>&1; then
        tag=$(wget -T 20 -qO- "$api_url" 2>/dev/null \
              | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
    fi

    # 兜底：releases/latest 重定向方式（可能也走不通，但保留）
    if [ -z "$tag" ]; then
        local latest_release_url="$RELEASE_REPO_URL/releases/latest"
        local resolved_url=""
        if command -v curl >/dev/null 2>&1; then
            resolved_url=$(curl --connect-timeout 10 --max-time 20 -fsSL -o /dev/null -w '%{url_effective}' "$latest_release_url" 2>/dev/null || true)
        elif command -v wget >/dev/null 2>&1; then
            resolved_url=$(wget -T 20 -qO- "$latest_release_url" 2>/dev/null | sed -n 's#.*releases/tag/\([^"/]*\).*#\1#p' | head -n1 || true)
        fi
        [ -n "$resolved_url" ] && tag="${resolved_url##*/}"
    fi

    if [ -n "$tag" ]; then
        printf '%s' "$tag"
        return 0
    fi
    return 1
}

select_text() {
    if [ "$UI_LANG" = "zh" ]; then
        printf '%s' "$1"
    else
        printf '%s' "$2"
    fi
}

version_ge() {
    local left_version="${1#v}"
    local right_version="${2#v}"

    awk -v left="$left_version" -v right="$right_version" 'BEGIN {
        split(left, left_parts, ".")
        split(right, right_parts, ".")
        for (index = 1; index <= 3; index++) {
            left_value = left_parts[index] + 0
            right_value = right_parts[index] + 0
            if (left_value > right_value) exit 0
            if (left_value < right_value) exit 1
        }
        exit 0
    }'
}

get_installed_version() {
    local version_output=""
    local version_text=""

    version_output=$("$BINARY_PATH" --version 2>/dev/null || true)
    version_text=$(printf '%s\n' "$version_output" | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n1 || true)

    if [ -n "$version_text" ]; then
        printf 'v%s' "$version_text"
        return 0
    fi

    return 1
}

get_workspace_version() {
    local cargo_file="$1/Cargo.toml"

    sed -n '/^\[workspace.package\]/,/^\[/{s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p}' "$cargo_file" | head -n1
}

maybe_skip_if_not_newer() {
    local target_version="$1"

    if [ -z "$INSTALLED_VERSION" ] || [ -z "$target_version" ]; then
        return 0
    fi

    if version_ge "$INSTALLED_VERSION" "$target_version"; then
        if [ "$UI_LANG" = "zh" ]; then
            echo "当前版本 $INSTALLED_VERSION 不低于目标版本 $target_version，跳过覆盖。"
        else
            echo "Installed version $INSTALLED_VERSION is not older than target version $target_version; skipping overwrite."
        fi
        exit 0
    fi

    return 0
}

finalize_installed_binary() {
    chmod +x "$BINARY_PATH" || echo "Warning: Could not set executable permission bit on $BINARY_PATH." >&2

    if [ "$(uname -s)" = "Darwin" ] && command -v xattr >/dev/null 2>&1; then
        xattr -d com.apple.quarantine "$BINARY_PATH" 2>/dev/null || true
    fi
}

tty_print() {
    if [ -w /dev/tty ] || [ -c /dev/tty ]; then
        printf '%s\n' "$1" >/dev/tty
    else
        printf '%s\n' "$1"
    fi
}

read_tty() {
    local value=""
    if [ -r /dev/tty ]; then
        read -r value </dev/tty
    else
        read -r value
    fi
    printf '%s' "$value"
}

shell_rc_file() {
    case "$(basename "${SHELL:-}")" in
        zsh)
            printf '%s' "$HOME/.zshrc"
            ;;
        bash)
            if [ -f "$HOME/.bash_profile" ]; then
                printf '%s' "$HOME/.bash_profile"
            else
                printf '%s' "$HOME/.bashrc"
            fi
            ;;
        fish)
            printf '%s' "$HOME/.config/fish/config.fish"
            ;;
        *)
            printf '%s' "$HOME/.profile"
            ;;
    esac
}

add_path_to_shell_rc() {
    local rc_file=""
    local path_line="export PATH=\"$TARGET_DIR:\$PATH\""

    rc_file=$(shell_rc_file)

    if [ -f "$rc_file" ] && grep -Fqx "$path_line" "$rc_file"; then
        return 0
    fi

    printf '\n# Added by LLMux installer\n%s\n' "$path_line" >> "$rc_file"

    if [ "$UI_LANG" = "zh" ]; then
        echo "已写入 $rc_file"
        echo "请执行：source \"$rc_file\" 或重新打开终端。"
    else
        echo "Added to $rc_file"
        echo "Run: source \"$rc_file\" or reopen your terminal."
    fi
}

maybe_help_setup_path() {
    if command -v llmux >/dev/null 2>&1; then
        return 0
    fi

    if [ "$SETUP_PATH" = "no" ]; then
        return 0
    fi

    if [ "$SETUP_PATH" = "yes" ]; then
        add_path_to_shell_rc
        return 0
    fi

    if [ "$IS_INTERACTIVE" != true ]; then
        return 0
    fi

    tty_print ""
    tty_print "$(select_text "要我帮你把安装目录加入 PATH 吗？" "Would you like me to add the install directory to PATH?")"
    tty_print "1) $(select_text "是，帮我设置" "Yes, set it up for me")"
    tty_print "2) $(select_text "否，我自己来" "No, I'll do it myself")"
    printf '%s' "$(select_text "请输入编号: " "Enter 1 or 2 and press Enter: ")" >/dev/tty
    choice=$(read_tty)
    case "$choice" in
        1) add_path_to_shell_rc ;;
        *) : ;;
    esac
}

pick_menu() {
    local title_zh="$1"
    local title_en="$2"
    local option1_zh="$3"
    local option1_en="$4"
    local option2_zh="$5"
    local option2_en="$6"
    local choice=""

    if [ "$UI_MODE" = "dialog" ] && command -v dialog >/dev/null 2>&1 && [ -n "${TERM:-}" ]; then
        if [ "$UI_LANG" = "zh" ]; then
            choice=$(dialog --stdout --clear --title "$title_zh" --menu "$title_zh" 12 72 2 1 "$option1_zh" 2 "$option2_zh")
        else
            choice=$(dialog --stdout --clear --title "$title_en" --menu "$title_en" 12 72 2 1 "$option1_en" 2 "$option2_en")
        fi
        if [ -n "$choice" ]; then
            printf '%s' "$choice"
            return 0
        fi
    elif [ "$UI_MODE" = "dialog" ] && command -v whiptail >/dev/null 2>&1 && [ -n "${TERM:-}" ]; then
        if [ "$UI_LANG" = "zh" ]; then
            choice=$(whiptail --title "$title_zh" --menu "$title_zh" 12 72 2 1 "$option1_zh" 2 "$option2_zh" 3>&1 1>&2 2>&3)
        else
            choice=$(whiptail --title "$title_en" --menu "$title_en" 12 72 2 1 "$option1_en" 2 "$option2_en" 3>&1 1>&2 2>&3)
        fi
        if [ -n "$choice" ]; then
            printf '%s' "$choice"
            return 0
        fi
    fi

    tty_print ""
    tty_print "$(select_text "$title_zh" "$title_en")"
    tty_print "1) $(select_text "$option1_zh" "$option1_en")"
    tty_print "2) $(select_text "$option2_zh" "$option2_en")"
    printf '%s' "$(select_text "请输入编号: " "Enter number: ")" >/dev/tty
    choice=$(read_tty)
    printf '%s' "$choice"
}

pick_language() {
    local choice=""

    if [ "$UI_MODE" = "dialog" ] && command -v dialog >/dev/null 2>&1 && [ -n "${TERM:-}" ]; then
        choice=$(dialog --stdout --clear --title "Language / 语言" --menu "Choose language / 选择语言" 12 72 2 zh "中文" en "English")
        if [ -n "$choice" ]; then
            printf '%s' "$choice"
            return 0
        fi
    elif [ "$UI_MODE" = "dialog" ] && command -v whiptail >/dev/null 2>&1 && [ -n "${TERM:-}" ]; then
        choice=$(whiptail --title "Language / 语言" --menu "Choose language / 选择语言" 12 72 2 zh "中文" en "English" 3>&1 1>&2 2>&3)
        if [ -n "$choice" ]; then
            printf '%s' "$choice"
            return 0
        fi
    fi

    tty_print ""
    tty_print "$(select_text "请选择语言" "Choose language")"
    tty_print "1) 中文"
    tty_print "2) English"
    printf '%s' "$(select_text "请输入编号: " "Enter 1 or 2 and press Enter: ")" >/dev/tty
    choice=$(read_tty)
    case "$choice" in
        1) printf 'zh' ;;
        2) printf 'en' ;;
        *) printf 'en' ;;
    esac
}

detect_language

if [ -t 0 ] && [ -t 1 ]; then
    IS_INTERACTIVE=true
fi

if [ -z "$UI_LANG" ] || [ "$UI_LANG" = "auto" ]; then
    if [ "$IS_INTERACTIVE" = true ]; then
        UI_LANG=$(pick_language)
    else
        UI_LANG="en"
        echo "Starting installer in non-interactive mode; defaulting to English."
    fi
fi

if [ "$UI_LANG" != "zh" ] && [ "$UI_LANG" != "en" ]; then
    UI_LANG="en"
fi

if [ -z "$INSTALL_MODE" ] || [ "$INSTALL_MODE" = "auto" ]; then
    if [ "$IS_INTERACTIVE" = true ]; then
        INSTALL_MODE=$(pick_menu "请选择安装方式" "Choose installation mode" "下载编译好的版本" "Download the prebuilt release" "从源码构建" "Build from source")
        case "$INSTALL_MODE" in
            1) INSTALL_MODE="release" ;;
            2) INSTALL_MODE="source" ;;
            *)
                echo "Invalid selection." >&2
                exit 1
                ;;
        esac
    else
        INSTALL_MODE="release"
        echo "Defaulting to prebuilt release mode. Use --mode source to build from source."
    fi
else
    :
fi

BINARY_PATH="$TARGET_DIR/llmux"

# 2. STEP 1: Pre-existence & Dynamic Path Check
if [ -f "$BINARY_PATH" ]; then
    INSTALLED_VERSION=$(get_installed_version || true)
fi

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "$(select_text "致命错误：未安装 '$1'。" "Fatal: '$1' is required but not installed.")" >&2
        exit 1
    fi
}

case "$(uname -s)" in
    Darwin) OS_NAME="macos" ;;
    Linux) OS_NAME="linux" ;;
    *)
        echo "$(select_text "致命错误：当前操作系统不受支持。" "Fatal: Unsupported operating system.")" >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    x86_64|amd64) ARCH_NAME="x64" ;;
    arm64|aarch64) ARCH_NAME="arm64" ;;
    *)
        echo "$(select_text "致命错误：当前 CPU 架构不受支持。" "Fatal: Unsupported architecture.")" >&2
        exit 1
        ;;
esac

if [ "$INSTALL_MODE" = "release" ]; then
    echo "$(select_text "正在获取最新 release 版本..." "Resolving the latest release tag...")"
    RELEASE_TAG=$(resolve_latest_release_tag || true)
    if [ -z "$RELEASE_TAG" ]; then
        echo "$(select_text "致命错误：无法获取最新 release 版本号。" "Fatal: Could not resolve the latest release tag.")" >&2
        exit 1
    fi
    echo "$(select_text "使用 release 版本：" "Using release tag:") $RELEASE_TAG"
    CANDIDATE_VERSION="$RELEASE_TAG"
    maybe_skip_if_not_newer "$CANDIDATE_VERSION"
    RELEASE_BASE_URL="$RELEASE_REPO_URL/releases/$RELEASE_TAG"
    case "$OS_NAME-$ARCH_NAME" in
        linux-x64) DOWNLOAD_URL="$RELEASE_BASE_URL/llmux-linux-x64" ;;
        linux-arm64) DOWNLOAD_URL="$RELEASE_BASE_URL/llmux-linux-arm64" ;;
        macos-arm64) DOWNLOAD_URL="$RELEASE_BASE_URL/llmux-macos-arm64" ;;
        *)
            echo "$(select_text "致命错误：当前平台没有可用的预编译包。" "Fatal: No prebuilt release is available for this platform.")" >&2
            exit 1
            ;;
    esac
else
    require_command git
    require_command cargo
    require_command bun
fi

if ! mkdir -p "$TARGET_DIR" 2>/dev/null; then
    echo "$(select_text "写入错误：无法创建 $TARGET_DIR。请使用 --dir 指定可写目录。" "Write Error: Permission denied when attempting to create $TARGET_DIR. Please re-run the installer using the '--dir' flag to specify a writable custom directory.")" >&2
    exit 1
fi

cleanup() {
    if [ "$SHOULD_CLEANUP" = true ] && [ -n "$WORK_DIR" ]; then
        rm -rf "$WORK_DIR" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM HUP QUIT

# 经镜像列表下载：依次尝试各镜像，最后直连兜底；单源失败自动换下一个
download_with_mirrors() {
    local dest="$1"
    local url="$2"
    local try_url=""

    # $GITHUB_MIRRORS 不加引号以触发分词；末尾追加空串作为直连兜底
    for mirror in $GITHUB_MIRRORS ""; do
        try_url="${mirror:+${mirror}/}${url}"
        echo "$(select_text "  尝试源：$try_url" "  Trying: $try_url")"
        if command -v curl >/dev/null 2>&1; then
            if curl --connect-timeout 10 --max-time 120 -fL --retry 2 -C - --progress-bar --show-error -o "$dest" "$try_url"; then
                return 0
            fi
        elif command -v wget >/dev/null 2>&1; then
            if wget -T 120 -t 2 -c -O "$dest" "$try_url"; then
                return 0
            fi
        else
            echo "$(select_text "致命错误：需要 curl 或 wget。" "Fatal: curl or wget is required.")" >&2
            exit 1
        fi
        # 该源失败，清理半成品后换下一个
        rm -f "$dest" 2>/dev/null || true
    done
    return 1
}

if [ "$INSTALL_MODE" = "release" ]; then
    echo "$(select_text "⠋ 下载编译好的版本..." "⠋ Downloading prebuilt release...")"
    WORK_DIR=$(mktemp -d "$TMP_BASE_DIR/llmux-release.XXXXXX")
    SHOULD_CLEANUP=true
    BINARY_SOURCE="$WORK_DIR/llmux"
    echo "$(select_text "下载地址：" "Download URL:") $DOWNLOAD_URL"
    echo "$(select_text "下载中，请稍等..." "Downloading, please wait...")"

    if ! download_with_mirrors "$BINARY_SOURCE" "$DOWNLOAD_URL"; then
        echo "$(select_text "下载失败：所有下载源均不可用。" "Download failed: all download sources are unreachable.")" >&2
        exit 1
    fi

    echo "$(select_text "下载完成，正在安装..." "Download complete, installing...")"

    if ! cp "$BINARY_SOURCE" "$BINARY_PATH" 2>/dev/null; then
        echo "$(select_text "写入错误：无法安装到目标目录。" "Write Error: Permission denied when attempting to write binary to $BINARY_PATH.")" >&2
        exit 1
    fi
    finalize_installed_binary
else
    if [ -n "$SOURCE_DIR" ]; then
        if [ ! -d "$SOURCE_DIR/.git" ]; then
            echo "$(select_text "致命错误：--source 必须指向 llmux-cli-rs 的 git 仓库。" "Fatal: --source must point to a git checkout of llmux-cli-rs.")" >&2
            exit 1
        fi
        WORK_DIR="$SOURCE_DIR"
        SHOULD_CLEANUP=false
        echo "$(select_text "⠋ 使用现有源码目录：" "⠋ Using existing source checkout:") $SOURCE_DIR"
    else
        echo "$(select_text "⠋ 克隆源码仓库..." "⠋ Cloning source repository...")"
        WORK_DIR=$(mktemp -d "$TMP_BASE_DIR/llmux-build.XXXXXX")
        SHOULD_CLEANUP=true
        if ! git clone --depth 1 "$REPO_URL" "$WORK_DIR"; then
            echo "$(select_text "致命错误：克隆 llmux-cli-rs 失败。" "Fatal: Failed to clone the llmux-cli-rs repository.")" >&2
            exit 1
        fi
    fi

    PROJECT_DIR="$WORK_DIR"
    CANDIDATE_VERSION=$(get_workspace_version "$PROJECT_DIR" || true)
    if [ -n "$CANDIDATE_VERSION" ]; then
        echo "$(select_text "源码版本：" "Source version:") $CANDIDATE_VERSION"
        maybe_skip_if_not_newer "$CANDIDATE_VERSION"
    fi

    echo "$(select_text "⠋ 构建前端资源..." "⠋ Building web UI...")"
    if ! (cd "$PROJECT_DIR/ui" && bun install && bun run build); then
        echo "$(select_text "构建失败：前端资源构建失败。" "Build Error: Failed to build the UI assets.")" >&2
        exit 1
    fi

    echo "$(select_text "⠋ 构建本地二进制..." "⠋ Building native binary...")"
    if ! (cd "$PROJECT_DIR" && cargo build --release -p llmux); then
        echo "$(select_text "构建失败：llmux 二进制构建失败。" "Build Error: Failed to build the llmux binary.")" >&2
        exit 1
    fi

    BINARY_SOURCE="$PROJECT_DIR/target/release/llmux"
    if [ ! -f "$BINARY_SOURCE" ]; then
        echo "$(select_text "构建失败：未生成目标二进制。" "Build Error: The expected binary was not produced.")" >&2
        exit 1
    fi

    if ! cp "$BINARY_SOURCE" "$BINARY_PATH" 2>/dev/null; then
        echo "$(select_text "写入错误：无法安装到目标目录。" "Write Error: Permission denied when attempting to write binary to $BINARY_PATH.")" >&2
        exit 1
    fi

    finalize_installed_binary
fi

echo "$(select_text "✓ LLMux 安装成功：$BINARY_PATH" "✓ LLMux installed successfully at $BINARY_PATH")"
echo ""
echo "$(select_text "运行命令：" "Run it with:")"
echo "  \"$BINARY_PATH\""
echo ""

if command -v llmux >/dev/null 2>&1; then
    echo "$(select_text "现在可以直接输入：" "You can now run:")"
    echo "  llmux"
else
    echo "$(select_text "当前终端还找不到 llmux，可能需要刷新 shell 缓存或设置 PATH。" "Your shell cannot find llmux yet. You may need to refresh shell cache or set PATH.")"
    echo "$(select_text "可以先临时执行：" "You can run this temporarily:")"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo "  hash -r"
    echo ""
fi

maybe_help_setup_path

echo ""
echo "$(select_text "启动后会打开本地网关，管理界面通常在：" "After launch, the local gateway is available at:")"
echo "  http://localhost:25976"
