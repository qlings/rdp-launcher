# 构建脚本：自动找到 MSVC 和 Windows SDK，把环境变量塞好再调 cargo。
# 换机器不需要改这个文件，只要装了 VS Build Tools（含 C++ 桌面开发）就行。
#
#   .\build.ps1           release 构建
#   .\build.ps1 check     cargo check
#   .\build.ps1 test      cargo test（真连测试默认 ignore，见 README）
#   .\build.ps1 run       直接跑起来
#   .\build.ps1 doctor    只打印探测到的工具链路径，不编译

$ErrorActionPreference = 'Stop'

function Find-MsvcToolset {
    $roots = @()

    # 1) vswhere：装了 Visual Studio / Build Tools 就会有
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) {
        $vswhere = "$env:ProgramFiles\Microsoft Visual Studio\Installer\vswhere.exe"
    }
    if (Test-Path $vswhere) {
        $installPath = & $vswhere -products * -latest `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath 2>$null
        if ($installPath) { $roots += (Join-Path $installPath 'VC\Tools\MSVC') }
    }

    # 2) 常见安装位置（含本项目装过的短路径 C:\BT）
    $roots += @(
        'C:\BT\VC\Tools\MSVC',
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Professional\VC\Tools\MSVC",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Enterprise\VC\Tools\MSVC",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2019\BuildTools\VC\Tools\MSVC"
    ) | Where-Object { $_ }

    foreach ($root in $roots) {
        if (-not (Test-Path $root)) { continue }
        $toolset = Get-ChildItem $root -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending | Select-Object -First 1
        if ($toolset -and (Test-Path (Join-Path $toolset.FullName 'bin\Hostx64\x64\link.exe'))) {
            return $toolset.FullName
        }
    }
    return $null
}

function Find-WindowsSdk {
    foreach ($root in @(
            'C:\Program Files (x86)\Windows Kits\10',
            'C:\Program Files\Windows Kits\10')) {
        if (-not (Test-Path "$root\Lib")) { continue }
        $ver = Get-ChildItem "$root\Lib" -Directory -ErrorAction SilentlyContinue |
            Where-Object { Test-Path (Join-Path $_.FullName 'um\x64\kernel32.lib') } |
            Sort-Object Name -Descending | Select-Object -First 1
        if ($ver) { return @{ Root = $root; Version = $ver.Name } }
    }
    return $null
}

$msvc = Find-MsvcToolset
$sdk = Find-WindowsSdk

if (-not $msvc) {
    Write-Host "找不到 MSVC 链接器。" -ForegroundColor Red
    Write-Host "请安装 Visual Studio Build Tools 2022，勾选「使用 C++ 的桌面开发」：" -ForegroundColor Yellow
    Write-Host "  https://aka.ms/vs/17/release/vs_buildtools.exe" -ForegroundColor Yellow
    exit 1
}
if (-not $sdk) {
    Write-Host "找不到 Windows SDK（缺 um\x64\kernel32.lib）。" -ForegroundColor Red
    Write-Host "在 VS Installer 里补装 Windows 10/11 SDK。" -ForegroundColor Yellow
    exit 1
}

$env:PATH = "$msvc\bin\Hostx64\x64;$($sdk.Root)\bin\$($sdk.Version)\x64;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:INCLUDE = @(
    "$msvc\include",
    "$($sdk.Root)\Include\$($sdk.Version)\ucrt",
    "$($sdk.Root)\Include\$($sdk.Version)\um",
    "$($sdk.Root)\Include\$($sdk.Version)\shared",
    "$($sdk.Root)\Include\$($sdk.Version)\winrt"
) -join ';'
$env:LIB = @(
    "$msvc\lib\x64",
    "$($sdk.Root)\Lib\$($sdk.Version)\ucrt\x64",
    "$($sdk.Root)\Lib\$($sdk.Version)\um\x64"
) -join ';'

Write-Host "MSVC : $msvc"
Write-Host "SDK  : $($sdk.Version)  ($($sdk.Root))"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "找不到 cargo，请先装 Rust：https://rsproxy.cn/ 或 rustup.rs" -ForegroundColor Red
    exit 1
}
Write-Host "Rust : $(cargo --version)"
Write-Host ""

Set-Location $PSScriptRoot
$cmd = if ($args.Count -gt 0) { $args[0] } else { 'build' }
switch ($cmd) {
    'check'  { cargo check --message-format short }
    'test'   { cargo test }
    'run'    { cargo run }
    'doctor' { Write-Host '工具链探测正常，可以构建。' }
    default  { cargo build --release }
}
exit $LASTEXITCODE
