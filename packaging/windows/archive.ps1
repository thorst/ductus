# Builds the portable archive: ductus-<version>-windows-x64.zip from a release ductus.exe (the
# program, TypeScript's compiler in typescript\ beside it, the license, packaging/archive/README.md),
# for `ductus upgrade --check` before installing and for servers without the installer.
#
#   .\packaging\windows\archive.ps1 -Binary D:\target-win\release\ductus.exe -TypeScript $ts -Version 0.6.0-dev.20261028
#
# -TypeScript is the folder `bash scripts/dev/tsc.sh win32-x64 DIR` fills.
param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $TypeScript,
    [Parameter(Mandatory)] [string] $Version,
    [string] $Out = "dist"
)
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) "..\..")
if (-not (Test-Path (Join-Path $TypeScript "tsc.exe"))) { throw "no tsc.exe in $TypeScript (bash scripts/dev/tsc.sh win32-x64 DIR)" }
New-Item -ItemType Directory -Force $Out | Out-Null
$zip = Join-Path (Resolve-Path $Out) "ductus-$Version-windows-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
$files = [ordered]@{
    "ductus.exe" = Resolve-Path $Binary
    "LICENSE"    = Join-Path $repo "LICENSE"
    "README.md"  = Join-Path $repo "packaging\archive\README.md"
}
foreach ($file in Get-ChildItem (Resolve-Path $TypeScript) -File) {
    $files["typescript/$($file.Name)"] = $file.FullName
}
# Entries named here, under ductus-<version>/, with `/`: Compress-Archive and ZipFile's
# CreateFromDirectory in Windows PowerShell 5.1 write `\`, which other tools unpack as one long
# file name.
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::Open($zip, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($name in $files.Keys) {
        $entry = "ductus-$Version/$name"
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $files[$name], $entry, "Optimal") | Out-Null
    }
} finally {
    $archive.Dispose()
}
Write-Host "built $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
