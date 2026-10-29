# Builds the portable archive: ductus-<version>-windows-x64.zip from a release ductus.exe (the
# program, the license, packaging/archive/README.md), for `ductus upgrade --check` before
# installing and for servers without the installer.
#
#   .\packaging\windows\archive.ps1 -Binary D:\target-win\release\ductus.exe -Version 0.6.0-dev.20261028
param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $Version,
    [string] $Out = "dist"
)
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) "..\..")
New-Item -ItemType Directory -Force $Out | Out-Null
$zip = Join-Path (Resolve-Path $Out) "ductus-$Version-windows-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
$files = [ordered]@{
    "ductus.exe" = Resolve-Path $Binary
    "LICENSE"    = Join-Path $repo "LICENSE"
    "README.md"  = Join-Path $repo "packaging\archive\README.md"
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
