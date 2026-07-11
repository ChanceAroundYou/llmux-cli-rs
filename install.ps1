<#
.SYNOPSIS
    LLMux CLI - Windows 引导安装脚本 (PowerShell)
.DESCRIPTION
    克隆仓库、构建本地二进制或下载预编译版本，并安装到本地。
.PARAMETER Dir
    指定二进制文件的安装目录（默认 $env:USERPROFILE\.local\bin）
.PARAMETER Source
    使用已有的本地源码目录而非重新克隆
.PARAMETER Mode
    安装模式: auto（交互式选择）| release（下载预编译）| source（从源码构建）
.PARAMETER Lang
    界面语言: auto（自动检测）| zh（中文）| en（英文）
.PARAMETER Path
    PATH 设置: auto（交互式询问）| yes（自动添加）| no（不处理）
.PARAMETER Help
    显示此帮助信息
.EXAMPLE
    .\install.ps1
.EXAMPLE
    .\install.ps1 -Mode release -Lang zh
.EXAMPLE
    .\install.ps1 -Dir D:\tools -Mode source
#>

param(
    [string]$Dir = "",
    [string]$Source = "",
    [ValidateSet("auto", "release", "source")]
    [string]$Mode = "auto",
    [ValidateSet("auto", "zh", "en")]
    [string]$Lang = "auto",
    [ValidateSet("auto", "yes", "no")]
    [string]$Path = "auto",
    [switch]$Help
)

# 显示帮助
if ($Help) {
    Write-Host "用法: .\install.ps1 [选项]"
    Write-Host ""
    Write-Host "选项:"
    Write-Host "  -Dir <path>      指定二进制文件安装目录"
    Write-Host "  -Source <path>   使用本地已有的源码目录"
    Write-Host "  -Mode <auto|release|source>  选择安装方式"
    Write-Host "  -Lang <auto|zh|en>           选择界面语言"
    Write-Host "  -Path <auto|yes|no>          安装后是否设置 PATH"
    Write-Host "  -Help                        显示此帮助菜单"
    exit 0
}

# ============================================================
# 常量
# ============================================================
$REPO_URL        = "https://github.com/zhMoody/llmux-cli-rs.git"
$RELEASE_REPO    = "https://github.com/zhMoody/llmux-cli-rs"
$API_BASE        = "https://api.github.com/repos/zhMoody/llmux-cli-rs"
$DEFAULT_TARGET  = "$env:USERPROFILE\.local\bin"
$TEMP_DIR        = [System.IO.Path]::GetTempPath()

# ============================================================
# 状态变量
# ============================================================
$TargetDir       = ""
$CustomDir       = $Dir
$SourceDir       = $Source
$InstallMode     = $Mode
$UILang          = $Lang
$SetupPath       = $Path
$IsInteractive   = $false
$ShouldCleanup   = $false
$WorkDir         = ""
$BinaryPath      = ""
$InstalledVersion = $null
$CandidateVersion = $null

# ============================================================
# 辅助函数
# ============================================================

function Select-Text($zh, $en) {
    if ($UILang -eq "zh") { return $zh } else { return $en }
}

function Write-Ln($zh, $en) {
    Write-Host (Select-Text -zh $zh -en $en)
}

function Write-ErrorLn($zh, $en) {
    Write-Host -ForegroundColor Red (Select-Text -zh $zh -en $en)
}

function Read-Answer {
    return Read-Host
}

function Invoke-LanguagePicker {
    Write-Host ""
    Write-Host "1) 中文"
    Write-Host "2) English"
    Write-Host -NoNewline (Select-Text -zh "请输入编号: " -en "Enter 1 or 2: ")
    $choice = Read-Answer
    switch ($choice) {
        "1" { return "zh" }
        "2" { return "en" }
        default { return "en" }
    }
}

function Invoke-MenuPicker($titleZh, $titleEn, $opt1Zh, $opt1En, $opt2Zh, $opt2En) {
    Write-Host ""
    Write-Host (Select-Text -zh $titleZh -en $titleEn)
    Write-Host "1) $(Select-Text -zh $opt1Zh -en $opt1En)"
    Write-Host "2) $(Select-Text -zh $opt2Zh -en $opt2En)"
    Write-Host -NoNewline (Select-Text -zh "请输入编号: " -en "Enter number: ")
    $choice = Read-Answer
    return $choice
}

function Resolve-LatestReleaseTag {
    # 方法1: 通过重定向获取最新 tag（不依赖 API，不容易被限流）
    try {
        $response = Invoke-WebRequest -Uri "https://github.com/zhMoody/llmux-cli-rs/releases/latest" -UseBasicParsing -MaximumRedirection 0 -ErrorAction Stop
    } catch {
        $statusCode = $_.Exception.Response.StatusCode.value__
        if ($statusCode -eq 302 -or $statusCode -eq 301) {
            $location = $_.Exception.Response.Headers.GetValues("Location")[0]
            if ($location -match 'tag/([^/]+)$') {
                return $matches[1]
            }
        }
    }

    # 方法2: 使用 API（有未认证限流风险）
    try {
        $response = Invoke-RestMethod -Uri "$API_BASE/releases/latest" -TimeoutSec 20 -ErrorAction Stop
        return $response.tag_name
    } catch {}

    return $null
}

function Get-InstalledVersion {
    try {
        $output = & $BinaryPath --version 2>&1
        if ($output -match '(\d+\.\d+\.\d+)') {
            return "v$($matches[1])"
        }
    } catch {}
    return $null
}

function Get-WorkspaceVersion($projectDir) {
    $cargoFile = Join-Path $projectDir "Cargo.toml"
    if (-not (Test-Path $cargoFile)) { return $null }

    $content = Get-Content $cargoFile -Raw
    if ($content -match '\[workspace\.package\][^[]*version\s*=\s*"([^"]+)"') {
        return $matches[1]
    }
    return $null
}

function Compare-Version($leftVersion, $rightVersion) {
    # 返回 $true 如果 leftVersion >= rightVersion
    $lv = $leftVersion.TrimStart('v')
    $rv = $rightVersion.TrimStart('v')
    $leftParts = $lv.Split('.')
    $rightParts = $rv.Split('.')
    for ($i = 0; $i -lt 3; $i++) {
        $l = [int]::Parse($leftParts[$i])
        $r = [int]::Parse($rightParts[$i])
        if ($l -gt $r) { return $true }
        if ($l -lt $r) { return $false }
    }
    return $true  # 相等
}

function Test-Command($cmd) {
    return (Get-Command $cmd -ErrorAction SilentlyContinue) -ne $null
}


function Require-Command($cmd) {
    if (-not (Test-Command $cmd)) {
        Write-ErrorLn "致命错误：未安装 '$cmd'。" "Fatal: '$cmd' is required but not installed."
        exit 1
    }
}

function Add-LlmuxToPath {
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -split ';' -contains $TargetDir) {
        return
    }
    $newPath = "$TargetDir;$userPath"
    [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
    # 同时设置当前会话的 PATH
    $env:PATH = "$TargetDir;$env:PATH"
    Write-Ln "已将 $TargetDir 添加到用户 PATH。" "Added $TargetDir to user PATH."
    Write-Ln "请重新打开终端，或执行：`$env:PATH = `"$TargetDir;`$env:PATH`"" "Run: `$env:PATH = `"$TargetDir;`$env:PATH`" or reopen your terminal."
}

function Confirm-DirectoryWritable($dir) {
    try {
        if (-not (Test-Path $dir)) {
            New-Item -ItemType Directory -Path $dir -Force -ErrorAction Stop | Out-Null
        }
        $testFile = Join-Path $dir ".write_test_$PID"
        [System.IO.File]::WriteAllText($testFile, "test")
        Remove-Item $testFile -Force
        return $true
    } catch {
        return $false
    }
}

# ============================================================
# 开始安装
# ============================================================

Write-Ln "LLMux 安装程序启动中..." "LLMux installer starting..."
Write-Host ""

# ---- 语言选择 ----
if ($UILang -eq "auto") {
    # 检测系统语言
    $culture = [System.Globalization.CultureInfo]::CurrentCulture
    if ($culture.Name -like "zh*") {
        # 暂不自动选择，询问用户
        if ([Console]::IsInputRedirected) {
            $UILang = "en"
        } else {
            $UILang = Invoke-LanguagePicker
        }
    } else {
        $UILang = "en"
    }
}

# ---- 检测是否交互式 ----
if (-not [Console]::IsInputRedirected) {
    $IsInteractive = $true
}

# ---- 安装模式选择 ----
if ($InstallMode -eq "auto") {
    if ($IsInteractive) {
        $modeChoice = Invoke-MenuPicker "请选择安装方式" "Choose installation mode" "下载编译好的版本" "Download the prebuilt release" "从源码构建" "Build from source"
        switch ($modeChoice) {
            "1" { $InstallMode = "release" }
            "2" { $InstallMode = "source" }
            default {
                Write-ErrorLn "无效选择。" "Invalid selection."
                exit 1
            }
        }
    } else {
        $InstallMode = "release"
        Write-Host "非交互模式，默认使用预编译 release 模式。使用 -Mode source 可从源码构建。"
    }
}

# ---- 目标目录 ----
if ($CustomDir) {
    $TargetDir = $CustomDir
} else {
    $TargetDir = $DEFAULT_TARGET
}

$BinaryPath = Join-Path $TargetDir "llmux.exe"

# ---- 检查已安装版本 ----
if (Test-Path $BinaryPath) {
    $InstalledVersion = Get-InstalledVersion
}

# ---- 检测架构 ----
$osArch = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE")
$isArm64 = $osArch -eq "ARM64"
$ARCH_NAME = if ($isArm64) { "arm64" } else { "x64" }

Write-Ln "检测到系统：Windows $ARCH_NAME" "Detected: Windows $ARCH_NAME"

# ---- Release 模式：下载预编译二进制 ----
if ($InstallMode -eq "release") {
    Write-Ln "正在获取最新 release 版本..." "Resolving the latest release tag..."
    $RELEASE_TAG = Resolve-LatestReleaseTag
    if (-not $RELEASE_TAG) {
        Write-ErrorLn "致命错误：无法获取最新 release 版本号。" "Fatal: Could not resolve the latest release tag."
        exit 1
    }
    Write-Ln "使用 release 版本：$RELEASE_TAG" "Using release tag: $RELEASE_TAG"
    $CandidateVersion = $RELEASE_TAG

    # 检查是否跳过（已安装版本不更旧）
    if ($InstalledVersion -and $CandidateVersion) {
        if (Compare-Version $InstalledVersion $CandidateVersion) {
            Write-Ln "当前版本 $InstalledVersion 不低于目标版本 $CandidateVersion，跳过覆盖。" "Installed version $InstalledVersion is not older than target version $CandidateVersion; skipping overwrite."
            exit 0
        }
    }

    $releaseBaseUrl = "$RELEASE_REPO/releases/download/$RELEASE_TAG"
    if ($isArm64) {
        $DOWNLOAD_URL = "$releaseBaseUrl/llmux-windows-arm64.exe"
    } else {
        $DOWNLOAD_URL = "$releaseBaseUrl/llmux-windows-x64.exe"
    }

    # 清理函数
    $ShouldCleanup = $true
    $WorkDir = Join-Path $TEMP_DIR "llmux-release.$([System.IO.Path]::GetRandomFileName())"
    New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
    $binarySource = Join-Path $WorkDir "llmux.exe"

    Write-Ln "正在下载预编译版本..." "Downloading prebuilt release..."
    Write-Host (Select-Text -zh "下载地址：" -en "Download URL:") $DOWNLOAD_URL

    try {
        $progressPreference = 'silentlyContinue'  # 下载进度默认不显示
        Invoke-WebRequest -Uri $DOWNLOAD_URL -OutFile $binarySource -TimeoutSec 120 -ErrorAction Stop
        $progressPreference = 'Continue'
    } catch {
        $progressPreference = 'Continue'
        Write-ErrorLn "下载失败：无法获取 release 文件。" "Download failed: could not fetch the release binary."
        exit 1
    }

    Write-Ln "下载完成，正在安装..." "Download complete, installing..."

    # 确保目标目录存在
    if (-not (Confirm-DirectoryWritable $TargetDir)) {
        Write-ErrorLn "写入错误：无法安装到 $TargetDir。请以管理员身份运行或使用 -Dir 指定可写目录。" "Write Error: Cannot write to $TargetDir. Please run as administrator or use -Dir to specify a writable directory."
        exit 1
    }

    Copy-Item -Path $binarySource -Destination $BinaryPath -Force
}
# ---- Source 模式：从源码构建 ----
else {
    Require-Command "git"
    Require-Command "cargo"
    Require-Command "bun"

    if ($SourceDir) {
        if (-not (Test-Path (Join-Path $SourceDir ".git"))) {
            Write-ErrorLn "致命错误：-Source 必须指向 llmux-cli-rs 的 git 仓库。" "Fatal: -Source must point to a git checkout of llmux-cli-rs."
            exit 1
        }
        $WorkDir = $SourceDir
        $ShouldCleanup = $false
        Write-Ln "使用现有源码目录：$SourceDir" "Using existing source checkout: $SourceDir"
    } else {
        Write-Ln "正在克隆源码仓库..." "Cloning source repository..."
        $WorkDir = Join-Path $TEMP_DIR "llmux-build.$([System.IO.Path]::GetRandomFileName())"
        $ShouldCleanup = $true
        try {
            git clone --depth 1 $REPO_URL $WorkDir
        } catch {
            Write-ErrorLn "致命错误：克隆 llmux-cli-rs 失败。" "Fatal: Failed to clone the llmux-cli-rs repository."
            exit 1
        }
    }

    $projectDir = $WorkDir
    $CandidateVersion = Get-WorkspaceVersion $projectDir
    if ($CandidateVersion) {
        Write-Ln "源码版本：$CandidateVersion" "Source version: $CandidateVersion"
        if ($InstalledVersion -and (Compare-Version $InstalledVersion "v$CandidateVersion")) {
            Write-Ln "当前版本 $InstalledVersion 不低于目标版本 v$CandidateVersion，跳过覆盖。" "Installed version $InstalledVersion is not older than target version v$CandidateVersion; skipping overwrite."
            exit 0
        }
    }

    Write-Ln "正在构建前端资源..." "Building web UI..."
    Push-Location (Join-Path $projectDir "ui")
    try {
        bun install
        bun run build
    } catch {
        Pop-Location
        Write-ErrorLn "构建失败：前端资源构建失败。" "Build Error: Failed to build the UI assets."
        exit 1
    }
    Pop-Location

    Write-Ln "正在构建本地二进制..." "Building native binary..."
    Push-Location $projectDir
    try {
        cargo build --release -p llmux
    } catch {
        Pop-Location
        Write-ErrorLn "构建失败：llmux 二进制构建失败。" "Build Error: Failed to build the llmux binary."
        exit 1
    }
    Pop-Location

    $binarySource = Join-Path $projectDir "target\release\llmux.exe"
    if (-not (Test-Path $binarySource)) {
        Write-ErrorLn "构建失败：未生成目标二进制。" "Build Error: The expected binary was not produced."
        exit 1
    }

    # 确保目标目录存在
    if (-not (Confirm-DirectoryWritable $TargetDir)) {
        Write-ErrorLn "写入错误：无法安装到 $TargetDir。请以管理员身份运行或使用 -Dir 指定可写目录。" "Write Error: Cannot write to $TargetDir. Please run as administrator or use -Dir to specify a writable directory."
        exit 1
    }

    Copy-Item -Path $binarySource -Destination $BinaryPath -Force
}

# ---- 清理 ----
if ($ShouldCleanup -and $WorkDir -and (Test-Path $WorkDir)) {
    Remove-Item -Path $WorkDir -Recurse -Force -ErrorAction SilentlyContinue
}

# ---- 安装成功 ----
Write-Host ""
Write-Ln "✓ LLMux 安装成功：$BinaryPath" "✓ LLMux installed successfully at $BinaryPath"
Write-Host ""
Write-Ln "运行命令：" "Run it with:"
Write-Host "  `"$BinaryPath`""
Write-Host ""

# ---- PATH 检查与设置 ----
$inPath = ($env:PATH -split ';') -contains $TargetDir

if (-not $inPath) {
    if ($SetupPath -eq "yes") {
        Add-LlmuxToPath
    } elseif ($SetupPath -eq "auto" -and $IsInteractive) {
        Write-Host ""
        Write-Ln "当前终端还找不到 llmux。需要将安装目录添加到 PATH。" "Your shell cannot find llmux yet. Would you like to add the install directory to PATH?"
        $pathChoice = Invoke-MenuPicker "是否将安装目录添加到 PATH？" "Add install directory to PATH?" "是，帮我设置" "Yes, set it up for me" "否，我自己来" "No, I'll do it myself"
        if ($pathChoice -eq "1") {
            Add-LlmuxToPath
        }
    }

    if (-not (($env:PATH -split ';') -contains $TargetDir)) {
        Write-Host ""
        Write-Ln "可以先临时执行：" "You can run this temporarily:"
        Write-Host "  `$env:PATH = `"$TargetDir;`$env:PATH`""
        Write-Host "  llmux"
    }
} else {
    Write-Ln "现在可以直接输入：" "You can now run:"
    Write-Host "  llmux"
}

Write-Host ""
Write-Ln "启动后会打开本地网关，管理界面通常在：" "After launch, the local gateway is available at:"
Write-Host "  http://localhost:25976"