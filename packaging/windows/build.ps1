# Builds the installer: ductus.msi from a release ductus.exe and TypeScript's compiler.
#
#   .\packaging\windows\build.ps1 -Binary D:\target-win\release\ductus.exe -TypeScript $ts -Version 0.5.0-dev.20261012
#
# -TypeScript is the folder `bash scripts/dev/tsc.sh win32-x64 DIR` fills (tsc.exe and its
# lib.*.d.ts): installed as C:\Program Files\Ductus\typescript, where ductus.exe looks for it.
# Needs WiX v5 (dotnet tool install --global wix --version 5.0.2) and its Util extension
# (wix extension add --global WixToolset.Util.wixext/5.0.2).
param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $TypeScript,
    # Semver, as in the Linux packages. A development suffix is dropped: MSI versions are three
    # numbers, and builds of one version replace each other.
    [Parameter(Mandatory)] [string] $Version,
    [string] $Out = "dist"
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if ($Version -notmatch '^(\d+\.\d+\.\d+)') { throw "Version must start with major.minor.patch, got '$Version'" }
$msiVersion = $Matches[1]
if (-not (Test-Path (Join-Path $TypeScript "tsc.exe"))) { throw "no tsc.exe in $TypeScript (bash scripts/dev/tsc.sh win32-x64 DIR)" }
New-Item -ItemType Directory -Force $Out | Out-Null
$msi = Join-Path $Out "ductus-$Version-x64.msi"
wix build -arch x64 -ext WixToolset.Util.wixext -d "Version=$msiVersion" -d "Binary=$(Resolve-Path $Binary)" `
    -bindpath "TypeScript=$(Resolve-Path $TypeScript)" -b $here -o $msi (Join-Path $here "ductus.wxs")
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
Write-Host "built $msi (ProductVersion $msiVersion)"
