$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$llvmBin = Join-Path $projectRoot '.toolchain\llvm\LLVM\bin'
$cargoXwinBin = Join-Path $projectRoot '.toolchain\cargo-xwin'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$cargoXwin = Join-Path $cargoXwinBin 'cargo-xwin.exe'
$resourceCompiler = Join-Path $llvmBin 'llvm-rc.exe'

if (-not (Test-Path -LiteralPath $cargoXwin) -or -not (Test-Path -LiteralPath $resourceCompiler)) {
    throw '缺少 .toolchain 下的便携 LLVM 或 cargo-xwin。请安装 Visual Studio Build Tools，或先准备本地便携工具链。'
}

$env:PATH = "$cargoXwinBin;$llvmBin;$cargoBin;$env:PATH"
$env:XWIN_CROSS_COMPILER = 'clang-cl'
$env:RC = $resourceCompiler

& cargo xwin build --release --manifest-path (Join-Path $projectRoot 'src-tauri\Cargo.toml') --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw "Rust 构建失败，退出码 $LASTEXITCODE" }

$outputDirectory = Join-Path $projectRoot 'dist'
$outputFile = Join-Path $outputDirectory 'OTrans-Rust-v0.2.0.exe'
$builtFile = Join-Path $projectRoot 'src-tauri\target\x86_64-pc-windows-msvc\release\otrans-pc-rust.exe'
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
Copy-Item -LiteralPath $builtFile -Destination $outputFile -Force
Get-Item -LiteralPath $outputFile | Select-Object FullName, Length, LastWriteTime
