# The Windows installer, end to end (needs an elevated PowerShell; CI runs it on windows-latest):
#   install silently -> service registered right -> configure -> start -> health -> MLLP round trip
#   -> log written -> clean stop -> upgrade over a running, configured install -> uninstall.
#
#   .\scripts\demo-windows.ps1 -Msi dist\ductus-0.5.0-dev-x64.msi -DatabaseUrl postgres://u:p@localhost/db
param(
    [Parameter(Mandatory)] [string] $Msi,
    [Parameter(Mandatory)] [string] $DatabaseUrl,
    # Where the installer logs and the receiver's output go (CI uploads it).
    [string] $LogDir = ""
)
$ErrorActionPreference = "Stop"
$Msi = (Resolve-Path $Msi).Path
$data = "C:\ProgramData\Ductus"
$exe = "C:\Program Files\Ductus\ductus.exe"
$work = if ($LogDir) { $LogDir } else { Join-Path ([IO.Path]::GetTempPath()) ("ductus-win-" + [guid]::NewGuid().ToString("N").Substring(0, 8)) }
New-Item -ItemType Directory -Force $work | Out-Null
$failures = @()
$sink = $null

function Check([string] $name, [bool] $ok, [string] $detail = "") {
    if ($ok) { Write-Host "   ok: $name" } else { Write-Host "   FAILED: $name $detail"; $script:failures += $name }
}
function Msi([string[]] $arguments, [string] $log) {
    $p = Start-Process msiexec.exe -ArgumentList ($arguments + @("/qn", "/l*v", "`"$work\$log.log`"")) -Wait -PassThru
    Write-Host "   (msiexec $($arguments -join ' ') -> exit $($p.ExitCode); log $work\$log.log)"
    return $p.ExitCode
}
function Wait-Until([scriptblock] $condition, [int] $seconds = 60) {
    $until = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $until) { if (& $condition) { return $true }; Start-Sleep -Milliseconds 500 }
    return $false
}
function Health { try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7700/api/v1/health -TimeoutSec 3).StatusCode -eq 200 } catch { $false } }
function Engine-Log { Get-ChildItem "$data\logs\ductus*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime | ForEach-Object { Get-Content $_ } }

try {
    Write-Host "== 1. silent install"
    Check "msiexec /i /qn succeeds" ((Msi @("/i", "`"$Msi`"") "install") -in 0, 3010)
    $service = Get-Service Ductus -ErrorAction SilentlyContinue
    Check "service registered" ($null -ne $service)
    Check "not started (nothing is configured yet)" ($service.Status -eq "Stopped")
    $config = (sc.exe qc Ductus) -join "`n"
    Check "runs as NT SERVICE\Ductus" ($config -match "NT SERVICE\\Ductus")
    Check "starts automatically, with --service and the env file" (($config -match "AUTO_START") -and ($config -match "run --service --env-file"))
    Check "installed files" ((Test-Path $exe) -and (Test-Path "$data\ductus.env") -and (Test-Path "$data\project") -and (Test-Path "$data\logs"))
    $acl = (icacls "$data\ductus.env") -join "`n"
    Check "settings readable by the service, not by users" (($acl -match "NT SERVICE\\Ductus") -and ($acl -notmatch "BUILTIN\\Users"))

    Write-Host "== 2. configure and start"
    & $exe init "$data\project" | Out-Null
    # The sample interface writes files; a service can't write in the project, so send to a receiver.
    $yaml = "$data\project\interfaces\adt-in\interface.yaml"
    (Get-Content $yaml -Raw) -replace 'destination: \{ file: \{ dir: out/adt-in \} \}', 'destination: { mllp: { host: 127.0.0.1, port: 6662 } }' | Set-Content $yaml -Encoding utf8
    Add-Content "$data\ductus.env" "`nDUCTUS_DATABASE_URL=$DatabaseUrl`n# marker kept across the upgrade"
    $sink = Start-Process $exe -ArgumentList "sink --port 6662" -PassThru -NoNewWindow -RedirectStandardOutput "$work\sink.log" -RedirectStandardError "$work\sink.err"
    Start-Service Ductus
    Check "service reaches Running" (Wait-Until { (Get-Service Ductus).Status -eq "Running" })
    Check "health answers" (Wait-Until { Health })

    Write-Host "== 3. a message round trip over MLLP"
    & $exe gen --to 127.0.0.1:6661 --count 100 --patients 10 | Out-Null
    Check "the receiver got all 100" (Wait-Until { (Get-Content "$work\sink.log" -ErrorAction SilentlyContinue | Out-String) -match "unique 100 " } 30)
    Check "the engine wrote its log" ((Engine-Log | Out-String) -match "started interfaces=")

    Write-Host "== 4. clean stop"
    $at = Get-Date
    Stop-Service Ductus
    Check "service stopped" ((Get-Service Ductus).Status -eq "Stopped")
    Check "stop took the clean path" ((Engine-Log | Out-String) -match "stopped took=") ("(" + ((Get-Date) - $at).TotalSeconds + " s)")
    $exit = (sc.exe query Ductus) -join "`n"
    Check "exit code 0" ($exit -match "WIN32_EXIT_CODE\s*:\s*0")

    Write-Host "== 5. upgrade over a configured install that was running"
    Start-Service Ductus
    Check "running again" (Wait-Until { Health })
    Check "msiexec /i (same version) succeeds" ((Msi @("/i", "`"$Msi`"") "upgrade") -in 0, 3010)
    Check "service running after the upgrade" (Wait-Until { (Get-Service Ductus).Status -eq "Running" -and (Health) })
    Check "settings kept" ((Get-Content "$data\ductus.env" -Raw) -match "marker kept across the upgrade")
    Check "project kept" (Test-Path "$data\project\interfaces\adt-in\interface.yaml")
    & $exe gen --to 127.0.0.1:6661 --count 50 --offset 1000 | Out-Null
    Check "still takes messages" (Wait-Until { (Get-Content "$work\sink.log" -ErrorAction SilentlyContinue | Out-String) -match "unique 150 " } 30)

    Write-Host "== 6. uninstall"
    # The receiver runs from the installed program: let go of it first, like an administrator would.
    if ($sink -and -not $sink.HasExited) { Stop-Process -Id $sink.Id -Force; $sink.WaitForExit(5000) | Out-Null }
    Check "msiexec /x succeeds" ((Msi @("/x", "`"$Msi`"") "uninstall") -in 0, 3010)
    Check "service removed" ($null -eq (Get-Service Ductus -ErrorAction SilentlyContinue))
    Check "program removed" (-not (Test-Path $exe))
    Check "settings and project kept" ((Test-Path "$data\ductus.env") -and (Test-Path "$data\project"))
}
catch {
    Write-Host "EXCEPTION: $($_ | Out-String)"
    $failures += "exception"
}
finally {
    if ($sink -and -not $sink.HasExited) { Stop-Process -Id $sink.Id -Force }
    if ($failures.Count) {
        Write-Host "--- engine log"; Engine-Log | Select-Object -Last 40
        Write-Host "logs: $work"
    }
}
if ($failures.Count) { Write-Host "FAIL: $($failures -join ', ')"; exit 1 }
Write-Host "logs: $work"
Write-Host "PASS"
