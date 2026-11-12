# Deploys to the Windows service (0.7.4.4.2-0.7.4.4.3), end to end (needs an elevated PowerShell):
#   install the MSI -> the service in release mode (DUCTUS_RELEASES), nothing deployed yet ->
#   `ductus deploy` of a release: handed over in the service's own process (the same pid, the
#   service Running throughout) -> the next release under traffic (nothing refused) -> `ductus
#   handover` (the service's paramchange reload) -> `ductus deploy --rollback` -> uninstall, and
#   everything it made removed.
#
# Refuses to run where Ductus is installed or C:\ProgramData\Ductus exists: it never touches a
# real install.
#
#   .\scripts\demo-windows-deploy.ps1 -Msi dist\ductus-0.7.0-x64.msi -DatabaseUrl postgresql://u:p@localhost:5432/db
param(
    [Parameter(Mandatory)] [string] $Msi,
    [Parameter(Mandatory)] [string] $DatabaseUrl,
    [string] $LogDir = ""
)
$ErrorActionPreference = "Stop"
$Msi = (Resolve-Path $Msi).Path
$data = "C:\ProgramData\Ductus"
$exe = "C:\Program Files\Ductus\ductus.exe"
$api = "http://127.0.0.1:7700"
$work = if ($LogDir) { $LogDir } else { Join-Path ([IO.Path]::GetTempPath()) ("ductus-deploy-" + [guid]::NewGuid().ToString("N").Substring(0, 8)) }
New-Item -ItemType Directory -Force $work | Out-Null
$failures = @()
$sink = $null
$token = "win-deploy-" + [guid]::NewGuid().ToString("N").Substring(0, 6)

function Check([string] $name, [bool] $ok, [string] $detail = "") {
    if ($ok) { Write-Host "   ok: $name" } else { Write-Host "   FAILED: $name $detail"; $script:failures += $name }
}
function Msi([string[]] $arguments, [string] $log) {
    $p = Start-Process msiexec.exe -ArgumentList ($arguments + @("/qn", "/l*v", "`"$work\$log.log`"")) -Wait -PassThru
    return $p.ExitCode
}
function Wait-Until([scriptblock] $condition, [int] $seconds = 60) {
    $until = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $until) { if (& $condition) { return $true }; Start-Sleep -Milliseconds 500 }
    return $false
}
function Health { try { (Invoke-WebRequest -UseBasicParsing "$api/api/v1/health" -TimeoutSec 3).StatusCode -eq 200 } catch { $false } }
function Engine-Log { Get-ChildItem "$data\logs\ductus*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime | ForEach-Object { Get-Content $_ } }
function Service-Pid { (Get-CimInstance Win32_Service -Filter "Name='Ductus'").ProcessId }
# A native program's output, stderr included, without PowerShell 5.1 turning stderr into errors.
function Native([scriptblock] $run) {
    $ErrorActionPreference = "Continue"
    $out = & $run 2>&1 | ForEach-Object { "$_" } | Out-String
    return @{ Ok = ($LASTEXITCODE -eq 0); Out = $out }
}
function Deploy([string[]] $arguments, [string] $name) {
    $env:DUCTUS_API_TOKEN = $script:secret
    $r = Native { & $tools deploy @arguments --to $api }
    Set-Content "$work\$name.txt" $r.Out
    Write-Host ($r.Out -split "`n" | ForEach-Object { "      | $_" } | Out-String).TrimEnd()
    return $r
}
function Engine-Json {
    $headers = @{ Authorization = "Bearer $script:secret" }
    Invoke-RestMethod -Uri "$api/api/v1/engine" -Headers $headers -TimeoutSec 5
}

if (Get-Service Ductus -ErrorAction SilentlyContinue) { Write-Host "FAIL: Ductus is installed here: this demo only runs where it isn't"; exit 1 }
# What an earlier install left (an uninstall keeps settings and the project): moved aside for the
# demo, put back at the end.
$kept = $null
if (Test-Path $data) {
    $kept = "$data.kept-" + (Get-Date -Format "yyyyMMdd-HHmmss")
    Move-Item $data $kept
    Write-Host "== $data from an earlier install moved aside to $kept (put back at the end)"
}

try {
    Write-Host "== 1. install, release mode, nothing deployed"
    Check "msiexec /i /qn succeeds" ((Msi @("/i", "`"$Msi`"") "install") -in 0, 3010)
    $settings = (Get-Content "$data\ductus.env" -Raw) -replace '(?m)^DUCTUS_PROJECT=', '#DUCTUS_PROJECT='
    $settings += "`nDUCTUS_DATABASE_URL=$DatabaseUrl`nDUCTUS_RELEASES=$data\releases`n"
    Set-Content "$data\ductus.env" $settings -Encoding utf8 -NoNewline
    Start-Service Ductus
    Check "service reaches Running" (Wait-Until { (Get-Service Ductus).Status -eq "Running" })
    Check "health answers" (Wait-Until { Health })
    Check "nothing deployed yet: no interfaces" (Wait-Until { (Engine-Log) -match "nothing is deployed to this environment yet" })
    $pid0 = Service-Pid

    # The sender, receiver and CLI run from a copy (an uninstall closes what runs the installed exe).
    $tools = Join-Path $work "ductus-tools.exe"
    Copy-Item $exe $tools -Force
    $env:DUCTUS_DATABASE_URL = $DatabaseUrl
    $script:secret = ((Native { & $tools token create $token --role operator --permission deploy }).Out -split "`r?`n")[0].Trim()
    Check "a deploy token" ($script:secret.Length -gt 20)
    $sink = Start-Process $tools -ArgumentList "sink --port 6662" -PassThru -NoNewWindow -RedirectStandardOutput "$work\sink.log" -RedirectStandardError "$work\sink.err"

    # A project in git: init's interface, sending to the receiver.
    $p = Join-Path $work "integrations"
    & $tools init $p | Out-Null
    $yaml = "$p\interfaces\adt-in\interface.yaml"
    (Get-Content $yaml -Raw) -replace 'file: \{ dir: out/adt-in \}', 'mllp: { host: 127.0.0.1, port: 6662 }' | Set-Content $yaml -Encoding utf8
    Push-Location $p
    $git = { param([string[]] $a) Native { git -c core.autocrlf=true -c user.name=Ann -c user.email=ann@example.org @a } }
    & $git @("init", "-q", "-b", "main") | Out-Null
    & $git @("add", ".") | Out-Null
    & $git @("commit", "-qm", "first") | Out-Null
    $first = Native { & $tools release -m "first" }
    $firstFile = ([regex]::Match($first.Out, "wrote (.+?) \(")).Groups[1].Value
    Check "release 1 made" ($first.Ok -and (Test-Path $firstFile)) $first.Out

    Write-Host "== 2. deployed: handed over in the service's own process"
    $r = Deploy @($firstFile, "-m", "first deploy") "deploy1"
    Check "deployed" ($r.Ok -and $r.Out -match "handed over to a new engine in this process") $r.Out
    Check "the same process, the service Running" ((Service-Pid) -eq $pid0 -and (Get-Service Ductus).Status -eq "Running")
    Check "recorded in the file" ($r.Out -match "recorded in .*: deployed to prod")

    Write-Host "== 3. the next release under traffic: nothing refused"
    $archive = "$p\interfaces\adt-in\archive.ts"
    if (Test-Path $archive) { Add-Content $archive "// second" } else { Add-Content $yaml "# second" }
    & $git @("commit", "-qam", "second") | Out-Null
    $second = Native { & $tools release -m "second" }
    $secondFile = ([regex]::Match($second.Out, "wrote (.+?) \(")).Groups[1].Value
    $gen = Start-Process $tools -ArgumentList "gen --count 600 --patients 60 --rate 100 --connections 2" -PassThru -NoNewWindow -RedirectStandardOutput "$work\gen.log" -RedirectStandardError "$work\gen.err"
    Start-Sleep 1
    $r = Deploy @($secondFile, "-m", "second deploy") "deploy2"
    Check "deployed" ($r.Ok -and $r.Out -match "handed over to a new engine in this process") $r.Out
    $gen.WaitForExit()
    $sent = Get-Content "$work\gen.log" -Raw
    Write-Host "      sender: $(($sent -split "`n" | Select-Object -Last 2) -join ' ')"
    Check "the sender: nothing refused, nothing rejected" ($sent -match "rejected 0, refused 0,") $sent
    Check "the same process, the service Running" ((Service-Pid) -eq $pid0 -and (Get-Service Ductus).Status -eq "Running")

    Write-Host "== 4. ductus handover (the service's paramchange)"
    $before = @((Engine-Log) | Where-Object { $_ -match "took over: receiving never stopped" }).Count
    $h = Native { & $exe handover }
    Write-Host ($h.Out -split "`n" | ForEach-Object { "      | $_" } | Out-String).TrimEnd()
    Check "asked" $h.Ok $h.Out
    Check "handed over in the process" (Wait-Until { @((Engine-Log) | Where-Object { $_ -match "took over: receiving never stopped" }).Count -gt $before } 60)
    Check "the same process, the service Running" ((Service-Pid) -eq $pid0 -and (Get-Service Ductus).Status -eq "Running")
    $engine = Engine-Json
    Check "GET /engine: in process" ($engine.handover.in_process -eq $true)

    Write-Host "== 5. rolled back"
    $r = Deploy @("--rollback", "-m", "back to the first") "rollback"
    Check "rolled back to release 1" ($r.Ok -and $r.Out -match "rollback to release" -and $r.Out -match "handed over") $r.Out
    $id1 = ([regex]::Match($firstFile, "-([0-9a-f]{12})\.tar\.gz$")).Groups[1].Value
    Check "release 1 runs" ((Engine-Json).deployment.release -like "$id1*")
    Check "the same process, the service Running" ((Service-Pid) -eq $pid0 -and (Get-Service Ductus).Status -eq "Running")
    Pop-Location
}
catch {
    Write-Host "   FAILED: $_"
    $failures += "error"
}
finally {
    Write-Host "== 6. clean up"
    if ($sink) { Stop-Process -Id $sink.Id -Force -ErrorAction SilentlyContinue }
    # What the service said, kept before its data folder goes: its log, and our Event Log entries
    # (read in a child process: formatting them loads ductus.exe, which an uninstall would wait for).
    Copy-Item "$data\logs" "$work\service-logs" -Recurse -ErrorAction SilentlyContinue
    powershell -NoProfile -Command "Get-WinEvent -FilterHashtable @{ LogName = 'Application'; ProviderName = 'Ductus'; StartTime = (Get-Date).AddMinutes(-15) } -ErrorAction SilentlyContinue | ForEach-Object { '[' + `$_.Id + '] ' + `$_.Message }" | Set-Content "$work\events.txt"
    if ($failures.Count -gt 0) {
        Write-Host "   the service's log, last lines:"
        Get-ChildItem "$work\service-logs\*" -ErrorAction SilentlyContinue | ForEach-Object { Get-Content $_ -Tail 15 } | ForEach-Object { "      | $_" }
        Write-Host "   Event Log:"
        Get-Content "$work\events.txt" -ErrorAction SilentlyContinue | Select-Object -First 10 | ForEach-Object { "      | $_" }
    }
    if ($script:secret) { $null = Native { & $tools token revoke $token } }
    if (Get-Service Ductus -ErrorAction SilentlyContinue) {
        Stop-Service Ductus -ErrorAction SilentlyContinue
        Check "msiexec /x succeeds" ((Msi @("/x", "`"$Msi`"") "uninstall") -in 0, 3010)
    }
    Remove-Item $data -Recurse -Force -ErrorAction SilentlyContinue
    Check "nothing left: no service, no $data" (($null -eq (Get-Service Ductus -ErrorAction SilentlyContinue)) -and -not (Test-Path $data))
    if ($kept) {
        Move-Item $kept $data
        Check "the earlier install's $data put back" ((Test-Path $data) -and -not (Test-Path $kept))
    }
    Write-Host "logs: $work"
}
if ($failures.Count -eq 0) { Write-Host "PASS" } else { Write-Host "FAIL: $($failures -join ', ')"; exit 1 }
